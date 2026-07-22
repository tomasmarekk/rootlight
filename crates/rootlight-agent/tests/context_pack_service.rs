//! Public integration tests for transport-neutral context-pack orchestration.
//!
//! The fake port records the policy and immutable identity crossing the agent
//! boundary without relying on the MCP application or daemon client.

use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use rootlight_agent::{
    context_continuation::{
        ContextContinuationBinding, ContextContinuationCodec, ContextContinuationError,
        ContextContinuationState,
    },
    context_evidence::{
        ContextEvidenceCallContext, ContextEvidencePort, ContextEvidencePortError,
        ContextEvidencePortErrorKind, ContextSourceMaterial, ContextSourceOutput,
        ContextSourceRequest, EvidenceProvider, EvidenceProviderInvocation,
        EvidenceProviderObservation, EvidenceProviderObservationKind, EvidenceProviderOutput,
    },
    context_pack::{CONTEXT_PACK_TIMEOUT_MS, ContextPackService, ContextPackServiceError},
    policy::{BudgetCharge, CancellationSignal},
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentPortFuture,
        AgentResolutionContext, AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
};
use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{CoverageStatus, LineRange, SourceRef, SourceSpan};
use rootlight_mcp_contract::{
    ErrorCode, PublicError, RepositorySelector, SchemaVersion, TrustClassification,
    context::{ContextPackData, ContextPackInput, ContextSeedSelector},
    vertical::{
        CacheStatus, ContinuationCursor, CoverageSummary, Freshness, GenerationSelector,
        GenerationSummary, ReadEnvelope, RelationSummary, RepositoryIdSelector, RequiredNullable,
        ResolvedRepository, SymbolExplainData, SymbolExplanation, UsageSummary,
    },
};
use serde_json::Value;

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
struct EvidenceCall {
    invocation: EvidenceProviderInvocation,
    reservation: BudgetCharge,
    cancelled: bool,
    deadline: Instant,
}

#[derive(Debug)]
struct FakePort {
    identity_response: Mutex<Option<Result<AgentResolvedIdentity, AgentPortError>>>,
    child_response: Mutex<Option<Result<ReadEnvelope<Value>, AgentPortError>>>,
    identity_calls: Mutex<Vec<IdentityCall>>,
    evidence_calls: Mutex<Vec<EvidenceCall>>,
    call_count: AtomicUsize,
    definition_candidates: usize,
    candidate_tokens: u64,
    signature_bytes: Vec<usize>,
}

impl FakePort {
    fn new(
        identity_response: Result<AgentResolvedIdentity, AgentPortError>,
        child_response: Option<Result<ReadEnvelope<Value>, AgentPortError>>,
    ) -> Self {
        Self {
            identity_response: Mutex::new(Some(identity_response)),
            child_response: Mutex::new(child_response),
            identity_calls: Mutex::new(Vec::new()),
            evidence_calls: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            definition_candidates: 1,
            candidate_tokens: 32,
            signature_bytes: vec![1],
        }
    }

    fn paged(
        identity_response: Result<AgentResolvedIdentity, AgentPortError>,
        definition_candidates: usize,
        candidate_tokens: u64,
        signature_bytes: Vec<usize>,
    ) -> Self {
        let mut port = Self::new(identity_response, None);
        port.definition_candidates = definition_candidates;
        port.candidate_tokens = candidate_tokens;
        port.signature_bytes = signature_bytes;
        port
    }
}

impl ContextEvidencePort<TestCancellation> for FakePort {
    fn retrieve(
        &self,
        invocation: EvidenceProviderInvocation,
        context: ContextEvidenceCallContext<TestCancellation>,
    ) -> AgentPortFuture<Result<EvidenceProviderOutput, ContextEvidencePortError>> {
        self.call_count.fetch_add(1, Ordering::Relaxed);
        self.evidence_calls
            .lock()
            .expect("evidence call lock is available")
            .push(EvidenceCall {
                invocation: invocation.clone(),
                reservation: context.reservation(),
                cancelled: context.cancellation().is_cancelled(),
                deadline: context.deadline(),
            });
        if let Some(Err(error)) = self
            .child_response
            .lock()
            .expect("child response lock is available")
            .take()
        {
            let kind = match error {
                AgentPortError::Cancelled => ContextEvidencePortErrorKind::Cancelled,
                AgentPortError::DeadlineExceeded => ContextEvidencePortErrorKind::DeadlineExceeded,
                AgentPortError::InvalidResponse => ContextEvidencePortErrorKind::InvalidResponse,
                _ => ContextEvidencePortErrorKind::Unavailable,
            };
            return Box::pin(async move {
                Err(ContextEvidencePortError {
                    kind,
                    usage: BudgetCharge::default(),
                })
            });
        }

        if invocation.provider() != EvidenceProvider::Definition {
            return Box::pin(async move {
                Err(ContextEvidencePortError {
                    kind: ContextEvidencePortErrorKind::Unsupported,
                    usage: BudgetCharge::default(),
                })
            });
        }
        let observations = (0..self.definition_candidates)
            .map(|index| {
                let byte = u8::try_from(index).unwrap_or(u8::MAX).saturating_add(3);
                let candidate_symbol = SymbolId::from_bytes([byte; 20]);
                let definition = SourceRef::new(
                    invocation.repository(),
                    invocation.generation(),
                    SourceSpan::new(FileId::from_bytes([byte.saturating_add(20); 20]), 0, 32)
                        .expect("fixture source span is valid"),
                    ContentHash::from_bytes([byte; 32]),
                    Some(LineRange::new(1, 2).expect("fixture line range is valid")),
                );
                EvidenceProviderObservation {
                    kind: EvidenceProviderObservationKind::Primary,
                    symbol_id: Some(candidate_symbol),
                    identity: candidate_symbol.to_string(),
                    observed_score: Some(
                        900_u16.saturating_sub(u16::try_from(index).unwrap_or(u16::MAX)),
                    ),
                    estimated_tokens: self.candidate_tokens,
                    source_bytes: 0,
                    source_refs: vec![definition],
                }
            })
            .collect::<Vec<_>>();
        let output = EvidenceProviderOutput {
            repository: invocation.repository(),
            generation: invocation.generation(),
            invocation: invocation.id().clone(),
            observations,
            completeness: rootlight_mcp_contract::completeness::ResultCompleteness::complete(),
            usage: BudgetCharge {
                results: u64::try_from(self.definition_candidates).unwrap_or(u64::MAX),
                tokens: 32_u64
                    .saturating_mul(u64::try_from(self.definition_candidates).unwrap_or(u64::MAX)),
                ..BudgetCharge::default()
            },
        };
        Box::pin(async move { Ok(output) })
    }

    fn materialize_source(
        &self,
        request: ContextSourceRequest,
        _context: ContextEvidenceCallContext<TestCancellation>,
    ) -> AgentPortFuture<Result<ContextSourceOutput, ContextEvidencePortError>> {
        let signature_bytes = self.signature_bytes.clone();
        let materials = request
            .targets
            .into_iter()
            .enumerate()
            .map(|(index, target)| {
                let bytes = signature_bytes
                    .get(index)
                    .copied()
                    .or_else(|| signature_bytes.last().copied())
                    .unwrap_or(1);
                ContextSourceMaterial {
                    candidate_id: target.candidate_id,
                    source_ref: target.source_ref,
                    signature: Some("s".repeat(bytes)),
                    snippet: None,
                }
            })
            .collect::<Vec<_>>();
        let source_bytes = materials
            .iter()
            .filter_map(|material| material.signature.as_ref())
            .map(|signature| u64::try_from(signature.len()).unwrap_or(u64::MAX))
            .sum();
        let output = ContextSourceOutput {
            repository: request.repository,
            generation: request.generation,
            materials,
            completeness: rootlight_mcp_contract::completeness::ResultCompleteness::complete(),
            usage: BudgetCharge {
                results: u64::try_from(self.definition_candidates).unwrap_or(u64::MAX),
                source_bytes,
                ..BudgetCharge::default()
            },
        };
        Box::pin(async move { Ok(output) })
    }
}

impl ContextContinuationCodec for FakePort {
    fn open_context_continuation(
        &self,
        cursor: &ContinuationCursor,
        binding: ContextContinuationBinding,
    ) -> Result<ContextContinuationState, ContextContinuationError> {
        let payload = cursor
            .as_str()
            .strip_prefix("test:")
            .ok_or(ContextContinuationError::Invalid)?;
        let (encoded_binding, encoded) = payload
            .split_once(':')
            .ok_or(ContextContinuationError::Invalid)?;
        if encoded_binding != binding_fingerprint(binding) {
            return Err(ContextContinuationError::Invalid);
        }
        let bytes = encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| {
                let text =
                    std::str::from_utf8(pair).map_err(|_| ContextContinuationError::Invalid)?;
                u8::from_str_radix(text, 16).map_err(|_| ContextContinuationError::Invalid)
            })
            .collect::<Result<Vec<_>, _>>()?;
        ContextContinuationState::decode(&bytes)
    }

    fn seal_context_continuation(
        &self,
        state: ContextContinuationState,
        binding: ContextContinuationBinding,
    ) -> Result<ContinuationCursor, ContextContinuationError> {
        let encoded = state
            .encode()
            .into_iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        ContinuationCursor::parse(&format!("test:{}:{encoded}", binding_fingerprint(binding)))
            .map_err(|_| ContextContinuationError::Unavailable)
    }
}

fn binding_fingerprint(binding: ContextContinuationBinding) -> String {
    let mut hasher = blake3::Hasher::new_derive_key("rootlight.context-test-binding.v1");
    hasher.update(binding.repository.as_bytes());
    hasher.update(binding.generation.as_bytes());
    hasher.update(&binding.request_digest);
    hasher.update(&[binding.response_profile as u8]);
    hasher.update(&binding.token_budget.to_le_bytes());
    hasher.update(&binding.planner_version.to_le_bytes());
    hasher.update(&binding.role_policy_version.to_le_bytes());
    hasher
        .finalize()
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl AgentToolPort<TestCancellation> for FakePort {
    fn resolve_identity(
        &self,
        request: AgentIdentityRequest,
        context: AgentResolutionContext<TestCancellation>,
    ) -> AgentPortFuture<Result<AgentResolvedIdentity, AgentPortError>> {
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
        _request: AgentToolRequest,
        _context: AgentCallContext<TestCancellation>,
    ) -> AgentPortFuture<Result<ReadEnvelope<Value>, AgentPortError>> {
        let response = self
            .child_response
            .lock()
            .expect("child response lock is available")
            .take()
            .expect("test configured one child response");
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
        generation: generation_summary(generation_id),
        coverage: coverage(),
        warnings: Vec::new(),
    }
}

fn generation_summary(generation_id: GenerationId) -> GenerationSummary {
    GenerationSummary {
        generation_id,
        parent_generation: RequiredNullable(None),
        structural_freshness: Freshness::Current,
        semantic_freshness: Freshness::Current,
    }
}

fn coverage() -> CoverageSummary {
    CoverageSummary {
        status: CoverageStatus::Bounded,
        languages: Vec::new(),
        skipped_inputs: 0,
    }
}

fn usage() -> UsageSummary {
    UsageSummary {
        rows: 7,
        edges: 0,
        source_bytes: 0,
        json_bytes: 128,
        estimated_tokens: 32,
        wall_time_ms: 2,
        cache_status: CacheStatus::Miss,
        trace_id: "context-pack-child".to_owned(),
    }
}

fn child_response(generation_id: GenerationId, data: Value) -> ReadEnvelope<Value> {
    ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: identity(generation_id).repository,
        generation: generation_summary(generation_id),
        coverage: coverage(),
        data,
        truncated: false,
        completeness: rootlight_mcp_contract::completeness::ResultCompleteness::complete(),
        next_cursor: RequiredNullable(None),
        usage: usage(),
        warnings: Vec::new(),
        trust: TrustClassification::UntrustedRepositoryData,
    }
}

fn explanation(generation_id: GenerationId) -> SymbolExplanation {
    SymbolExplanation {
        symbol_id: symbol(),
        kind: rootlight_mcp_contract::vertical::EntityKind::Function,
        display_name: "admit_request".to_owned(),
        signature: Some("fn admit_request()".to_owned()),
        definition: SourceRef::new(
            repository(),
            generation_id,
            SourceSpan::new(FileId::from_bytes([4; 20]), 0, 32)
                .expect("fixture source span is valid"),
            ContentHash::from_bytes([5; 32]),
            Some(LineRange::new(1, 2).expect("fixture line range is valid")),
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

fn input(generation_id: GenerationId) -> ContextPackInput {
    ContextPackInput {
        repository: RepositorySelector::ById(RepositoryIdSelector {
            repository_id: repository(),
        }),
        generation: Some(GenerationSelector::Explicit(generation_id)),
        task: "explain request admission".to_owned(),
        seeds: ContextSeedSelector {
            symbols: Some(vec![symbol()]),
            paths: None,
            routes: None,
            tests: None,
            located: None,
            change: None,
            plan: None,
        },
        token_budget: 4_000,
        source_policy: None,
        sections: None,
        diversity: None,
        min_confidence: None,
        response_profile: None,
        continuation: None,
        explain: None,
    }
}

#[tokio::test]
async fn invalid_continuation_is_rejected_after_identity_resolution() {
    let mut request = input(generation(2));
    request.continuation =
        Some(ContinuationCursor::parse("next-page").expect("fixture cursor is valid"));
    let port = Arc::new(FakePort::new(Ok(identity(generation(2))), None));

    let result = ContextPackService
        .execute(
            Arc::clone(&port),
            request,
            repository(),
            TestCancellation(false),
        )
        .await;

    assert_eq!(result, Err(ContextPackServiceError::InvalidContinuation));
    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .len()
            == 1
    );
    assert_eq!(port.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn explain_resolves_explicit_identity_without_child_and_shapes_envelope() {
    let mut request = input(generation(2));
    request.explain = Some(true);
    let port = Arc::new(FakePort::new(Ok(identity(generation(2))), None));
    let started = Instant::now();

    let output = ContextPackService
        .execute(
            Arc::clone(&port),
            request,
            repository(),
            TestCancellation(false),
        )
        .await
        .expect("explain request succeeds");

    assert_eq!(port.call_count.load(Ordering::Relaxed), 0);
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
    assert!(calls[0].deadline > started);
    assert_eq!(output.generation.generation_id, generation(2));
    assert!(output.data.explanation.is_some());
    assert!(output.data.items.is_empty());
    assert_eq!(
        output.usage.estimated_tokens,
        u64::from(output.data.token_accounting.estimated_total)
    );
    assert_eq!(
        output
            .data
            .token_accounting
            .by_section
            .values()
            .copied()
            .fold(0_u32, u32::saturating_add),
        output.data.token_accounting.estimated_total
    );
    assert!(output.usage.estimated_tokens <= 4_000);
    let encoded = serde_json::to_value(output).expect("context-pack envelope serializes");
    serde_json::from_value::<ReadEnvelope<ContextPackData>>(encoded)
        .expect("context-pack envelope retains its public schema");
}

#[tokio::test]
async fn execution_propagates_policy_and_shapes_child_response() {
    let response_data = serde_json::to_value(SymbolExplainData {
        symbols: vec![explanation(generation(2))],
        unresolved_ids: Vec::new(),
        detail_handles: Vec::new(),
        explanation: None,
    })
    .expect("symbol explanation fixture serializes");
    let port = Arc::new(FakePort::new(
        Ok(identity(generation(2))),
        Some(Ok(child_response(generation(2), response_data))),
    ));

    let output = ContextPackService
        .execute(
            Arc::clone(&port),
            input(generation(2)),
            repository(),
            TestCancellation(false),
        )
        .await
        .expect("context-pack request succeeds");

    let calls = port
        .evidence_calls
        .lock()
        .expect("evidence call lock is available");
    assert_eq!(calls.len(), 8);
    assert_eq!(calls[0].invocation.provider(), EvidenceProvider::Definition);
    assert_eq!(calls[0].invocation.repository(), repository());
    assert_eq!(calls[0].invocation.generation(), generation(2));
    assert_eq!(calls[0].reservation, calls[0].invocation.reservation());
    assert_eq!(
        u64::from(CONTEXT_PACK_TIMEOUT_MS),
        rootlight_agent::policy::BudgetLimits::server_ceiling()
            .maximums()
            .time_ms
    );
    assert!(!calls[0].cancelled);
    assert!(calls[0].deadline > Instant::now());
    assert_eq!(
        output.usage.estimated_tokens,
        u64::from(output.data.token_accounting.estimated_total)
    );
    assert!(output.data.token_accounting.by_section.len() >= 10);
    assert_eq!(
        output
            .data
            .token_accounting
            .by_section
            .values()
            .copied()
            .fold(0_u32, u32::saturating_add),
        output.data.token_accounting.estimated_total
    );
    assert!(output.usage.json_bytes > 0);
    assert!(!output.data.role_coverage.complete());
    assert_ne!(
        output.completeness.state,
        rootlight_mcp_contract::completeness::CompletenessState::Complete
    );
    assert!(!output.data.followups.is_empty());
    assert_eq!(output.generation.generation_id, generation(2));
    let encoded = serde_json::to_value(output).expect("context-pack envelope serializes");
    serde_json::from_value::<ReadEnvelope<ContextPackData>>(encoded)
        .expect("context-pack envelope retains its public schema");
}

#[tokio::test]
async fn authenticated_continuation_resumes_without_duplicates_and_preserves_partial_truth() {
    let token_budget = 1_550;
    let mut request = input(generation(2));
    request.token_budget = token_budget;
    let port = Arc::new(FakePort::paged(
        Ok(identity(generation(2))),
        2,
        900,
        vec![1],
    ));
    let first = ContextPackService
        .execute(port, request, repository(), TestCancellation(false))
        .await
        .expect("bounded first page succeeds");
    assert!(
        first
            .completeness
            .guidance
            .contains(&rootlight_mcp_contract::completeness::ContinuationGuidance::UseCursor)
    );
    let cursor = first
        .next_cursor
        .0
        .clone()
        .expect("first page exposes its authenticated cursor");
    let first_symbols = first
        .data
        .items
        .iter()
        .filter_map(|item| item.symbol_id)
        .collect::<Vec<_>>();

    let mut resume = input(generation(2));
    resume.token_budget = token_budget;
    resume.continuation = Some(cursor.clone());
    let resume_port = Arc::new(FakePort::paged(
        Ok(identity(generation(2))),
        2,
        900,
        vec![1],
    ));
    let second = ContextPackService
        .execute(resume_port, resume, repository(), TestCancellation(false))
        .await
        .expect("authenticated second page resumes");
    let second_symbols = second
        .data
        .items
        .iter()
        .filter_map(|item| item.symbol_id)
        .collect::<Vec<_>>();
    assert!(!second_symbols.is_empty());
    assert!(
        first_symbols
            .iter()
            .all(|symbol| !second_symbols.contains(symbol))
    );
    assert!(second.next_cursor.0.is_none());

    let stale_definition = rootlight_mcp_contract::error_definition(ErrorCode::StaleGeneration);
    let stale = PublicError::builder(ErrorCode::StaleGeneration, stale_definition.message)
        .build()
        .expect("stale-generation fixture is canonical");
    let retired_port = Arc::new(FakePort::paged(
        Err(AgentPortError::Public(Box::new(stale))),
        2,
        900,
        vec![1],
    ));
    let mut retired_resume = input(generation(2));
    retired_resume.token_budget = token_budget;
    retired_resume.continuation = Some(cursor);
    assert_eq!(
        ContextPackService
            .execute(
                retired_port,
                retired_resume,
                repository(),
                TestCancellation(false),
            )
            .await,
        Err(ContextPackServiceError::InvalidContinuation)
    );
}

#[tokio::test]
async fn pinned_identity_path_skips_resolution_and_preserves_child_behavior() {
    let response_data = serde_json::to_value(SymbolExplainData {
        symbols: vec![explanation(generation(2))],
        unresolved_ids: Vec::new(),
        detail_handles: Vec::new(),
        explanation: None,
    })
    .expect("symbol explanation fixture serializes");
    let port = Arc::new(FakePort::new(
        Err(AgentPortError::Unavailable),
        Some(Ok(child_response(generation(2), response_data))),
    ));
    let deadline = Instant::now() + std::time::Duration::from_secs(1);

    let output = ContextPackService
        .execute_with_identity(
            Arc::clone(&port),
            input(generation(2)),
            repository(),
            identity(generation(2)),
            TestCancellation(false),
            deadline,
        )
        .await
        .expect("context pack succeeds under the pinned identity");

    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    let calls = port
        .evidence_calls
        .lock()
        .expect("evidence call lock is available");
    assert_eq!(calls.len(), 8);
    assert_eq!(calls[0].invocation.provider(), EvidenceProvider::Definition);
    assert_eq!(calls[0].invocation.generation(), generation(2));
    assert_eq!(calls[0].deadline, deadline);
    assert_eq!(output.generation.generation_id, generation(2));
    assert_eq!(
        output.usage.estimated_tokens,
        u64::from(output.data.token_accounting.estimated_total)
    );
}

#[tokio::test]
async fn already_cancelled_request_stops_before_identity_or_child_work() {
    let port = Arc::new(FakePort::new(Ok(identity(generation(2))), None));

    assert_eq!(
        ContextPackService
            .execute(
                Arc::clone(&port),
                input(generation(2)),
                repository(),
                TestCancellation(true),
            )
            .await,
        Err(ContextPackServiceError::Cancelled)
    );
    assert!(
        port.identity_calls
            .lock()
            .expect("identity call lock is available")
            .is_empty()
    );
    assert_eq!(port.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn explicit_identity_mismatch_fails_before_child_dispatch() {
    let port = Arc::new(FakePort::new(Ok(identity(generation(9))), None));

    let result = ContextPackService
        .execute(
            Arc::clone(&port),
            input(generation(2)),
            repository(),
            TestCancellation(false),
        )
        .await;

    assert_eq!(result, Err(ContextPackServiceError::InvalidResponse));
    assert_eq!(port.call_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn port_errors_preserve_public_and_policy_classification() {
    let public = PublicError::builder(ErrorCode::UnsupportedCapability, "capability unavailable")
        .build()
        .expect("static public error is valid");
    let public_port = Arc::new(FakePort::new(
        Err(AgentPortError::Public(Box::new(public.clone()))),
        None,
    ));
    assert_eq!(
        ContextPackService
            .execute(
                public_port,
                input(generation(2)),
                repository(),
                TestCancellation(false),
            )
            .await,
        Err(ContextPackServiceError::Public(Box::new(public)))
    );

    for (port_error, expected) in [
        (
            AgentPortError::Cancelled,
            ContextPackServiceError::Cancelled,
        ),
        (
            AgentPortError::DeadlineExceeded,
            ContextPackServiceError::DeadlineExceeded,
        ),
        (
            AgentPortError::InvalidResponse,
            ContextPackServiceError::InvalidResponse,
        ),
    ] {
        let port = Arc::new(FakePort::new(
            Ok(identity(generation(2))),
            Some(Err(port_error)),
        ));
        assert_eq!(
            ContextPackService
                .execute(
                    port,
                    input(generation(2)),
                    repository(),
                    TestCancellation(false),
                )
                .await,
            Err(expected)
        );
    }
}
