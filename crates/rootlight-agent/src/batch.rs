//! Bounded batch query planning and response shaping.
//!
//! This module owns the transport-neutral dependency plan, child dispatch
//! schedule, typed binding resolution, shared budget and deadline policy,
//! deterministic request-order result shaping, and aggregate usage accounting.
//! The composing application implements [`crate::port::AgentToolPort`] to map
//! admitted child calls onto its concrete client.

use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_ids::RepositoryId;
use rootlight_mcp_contract::{
    ErrorCode, McpTool, PublicError, SchemaVersion, TrustClassification,
    context::{
        BatchOperation as ContractBatchOperation, BatchOperationResult, BatchOperationStatus,
        BatchStatus, BatchTool, FailurePolicy, QueryBatchData, QueryBatchInput,
    },
    vertical::{
        CacheStatus, CoverageSummary, ReadEnvelope, RequiredNullable, ResolvedRepository,
        ResponseBudget, ResponseWarning, UsageSummary,
    },
};
use serde_json::{Map, Value};

use crate::{
    policy::{
        BudgetCharge, BudgetLedger, CancellationSignal, ExecutionPolicyError, is_compact_profile,
    },
    port::{AgentCallContext, AgentPortError, AgentToolPort, AgentToolRequest},
};

/// Maximum operations accepted in one public batch request.
pub const MAX_BATCH_OPERATIONS: usize = 16;

/// Maximum dependency depth in the batch operation DAG.
pub const MAX_BATCH_DEPTH: usize = 8;

/// Maximum dependencies one operation may declare.
pub const MAX_DEPS_PER_OPERATION: usize = 8;

/// The closed allowlist of tools permitted inside a public batch.
///
/// Aliased from the capability registry, the single source of truth for batch
/// eligibility. Mutation tools, repository or operation polling, nested
/// batches, `history.compare`, `query.advanced`, and cross-generation
/// operations are forbidden.
pub const BATCH_ALLOWLIST: [McpTool; 11] = rootlight_mcp_contract::capability::BATCH_ELIGIBLE;

/// Errors returned during batch validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BatchValidationError {
    /// The batch contains zero operations or more than sixteen.
    #[error("batch operation count is outside the 1..16 range")]
    InvalidOperationCount,
    /// An operation references a tool not in the batch allowlist.
    #[error("operation uses a tool not in the batch allowlist")]
    ForbiddenTool,
    /// The dependency graph contains a cycle.
    #[error("batch dependency graph contains a cycle")]
    CyclicDependency,
    /// The dependency graph exceeds depth eight.
    #[error("batch dependency graph exceeds depth eight")]
    DepthExceeded,
    /// An operation references a nonexistent dependency index.
    #[error("operation references a nonexistent dependency")]
    InvalidDependencyReference,
    /// An operation declares more than eight dependencies.
    #[error("operation declares too many dependencies")]
    TooManyDependencies,
    /// A binding references an invalid source operation or field.
    #[error("binding references an invalid source")]
    InvalidBinding,
    /// The batch attempts to use a nested batch operation.
    #[error("nested batch operations are forbidden")]
    NestedBatch,
}

/// Failure while resolving a validated batch for execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BatchExecutionError {
    /// Two operations use the same request-scoped identity.
    #[error("batch operation identifiers are not unique")]
    DuplicateOperationId,
    /// An operation names a dependency that is not present.
    #[error("batch operation references an unknown dependency")]
    UnknownDependency,
    /// A typed binding does not resolve through a declared completed dependency.
    #[error("batch binding does not resolve through a completed dependency")]
    InvalidBinding,
    /// A batch-inherited field could not be represented as JSON.
    #[error("batch inherited field could not be serialized")]
    Serialization,
    /// A bounded orchestration allocation could not be reserved.
    #[error("batch orchestration memory is unavailable")]
    MemoryUnavailable,
}

/// One operation in a validated batch request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOperation {
    /// Zero-based position in request order.
    pub index: usize,
    /// The tool to invoke.
    pub tool: McpTool,
    /// Indices of operations this one depends on.
    pub depends_on: Vec<usize>,
}

/// A validated batch execution plan.
///
/// Operations are topologically sorted for execution but output is always
/// returned in original request order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    /// Operations in request order.
    pub operations: Vec<BatchOperation>,
    /// Topologically sorted execution order (indices into operations).
    pub execution_order: Vec<usize>,
}

impl BatchPlan {
    /// Validates and builds a batch execution plan from raw operation specs.
    ///
    /// # Errors
    ///
    /// Returns [BatchValidationError] when the batch violates any public
    /// contract invariant.
    pub fn validate(
        tools: &[McpTool],
        dependencies: &[Vec<usize>],
    ) -> Result<Self, BatchValidationError> {
        let count = tools.len();
        if count == 0 || count > MAX_BATCH_OPERATIONS {
            return Err(BatchValidationError::InvalidOperationCount);
        }
        if dependencies.len() != count {
            return Err(BatchValidationError::InvalidDependencyReference);
        }

        let mut operations = Vec::new();
        operations
            .try_reserve_exact(count)
            .map_err(|_| BatchValidationError::InvalidOperationCount)?;

        for (index, (tool, deps)) in tools.iter().zip(dependencies).enumerate() {
            if !BATCH_ALLOWLIST.contains(tool) {
                return Err(BatchValidationError::ForbiddenTool);
            }
            if *tool == McpTool::QueryBatch {
                return Err(BatchValidationError::NestedBatch);
            }
            if deps.len() > MAX_DEPS_PER_OPERATION {
                return Err(BatchValidationError::TooManyDependencies);
            }
            for dep in deps {
                if *dep >= count || *dep == index {
                    return Err(BatchValidationError::InvalidDependencyReference);
                }
            }
            operations.push(BatchOperation {
                index,
                tool: *tool,
                depends_on: deps.clone(),
            });
        }

        let execution_order = topological_sort(&operations)?;
        Ok(Self {
            operations,
            execution_order,
        })
    }

    /// Returns the maximum dependency depth in the plan.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        let mut depths = vec![0usize; self.operations.len()];
        for idx in &self.execution_order {
            let op = &self.operations[*idx];
            let dep_depth = op
                .depends_on
                .iter()
                .map(|d| depths[*d] + 1)
                .max()
                .unwrap_or(0);
            depths[*idx] = dep_depth;
        }
        depths.into_iter().max().unwrap_or(0)
    }
}

/// Kahn's algorithm for topological sort with cycle detection.
fn topological_sort(operations: &[BatchOperation]) -> Result<Vec<usize>, BatchValidationError> {
    let count = operations.len();
    let mut in_degree = vec![0usize; count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];

    for op in operations {
        for dep in &op.depends_on {
            in_degree[op.index] += 1;
            dependents[*dep].push(op.index);
        }
    }

    let mut queue: Vec<usize> = (0..count).filter(|i| in_degree[*i] == 0).collect();
    let mut order = Vec::new();
    order
        .try_reserve_exact(count)
        .map_err(|_| BatchValidationError::CyclicDependency)?;

    while let Some(node) = queue.pop() {
        order.push(node);
        for dependent in &dependents[node] {
            in_degree[*dependent] -= 1;
            if in_degree[*dependent] == 0 {
                queue.push(*dependent);
            }
        }
    }

    if order.len() != count {
        return Err(BatchValidationError::CyclicDependency);
    }

    // Verify depth constraint
    let mut depths = vec![0usize; count];
    for idx in &order {
        let op = &operations[*idx];
        let dep_depth = op
            .depends_on
            .iter()
            .map(|d| depths[*d] + 1)
            .max()
            .unwrap_or(0);
        depths[*idx] = dep_depth;
        if dep_depth > MAX_BATCH_DEPTH {
            return Err(BatchValidationError::DepthExceeded);
        }
    }

    Ok(order)
}

/// Reports whether a tool is in the batch allowlist.
#[must_use]
pub fn is_batch_allowed(tool: McpTool) -> bool {
    BATCH_ALLOWLIST.contains(&tool)
}

/// Reports whether a tool is visible under the given profile AND in the batch
/// allowlist. `query.batch` cannot bypass profile filtering.
#[must_use]
pub fn is_batch_allowed_under_profile(
    tool: McpTool,
    profile: rootlight_mcp_contract::ExposureProfile,
) -> bool {
    is_batch_allowed(tool) && profile.exposes(tool)
}

/// Maps a public batch subtool to its catalog counterpart.
#[must_use]
pub const fn mcp_tool_for_batch(tool: BatchTool) -> McpTool {
    match tool {
        BatchTool::CodeLocate => McpTool::CodeLocate,
        BatchTool::SymbolExplain => McpTool::SymbolExplain,
        BatchTool::SymbolRelationships => McpTool::SymbolRelationships,
        BatchTool::FlowTrace => McpTool::FlowTrace,
        BatchTool::ChangeImpact => McpTool::ChangeImpact,
        BatchTool::TestsSelect => McpTool::TestsSelect,
        BatchTool::ArchitectureOverview => McpTool::ArchitectureOverview,
        BatchTool::ArchitectureCycles => McpTool::ArchitectureCycles,
        BatchTool::CodeDead => McpTool::CodeDead,
        BatchTool::PlanChange => McpTool::PlanChange,
        BatchTool::ContextPack => McpTool::ContextPack,
        BatchTool::SourceRead => McpTool::SourceRead,
    }
}

/// Resolves declared operation identities to request-order indices.
///
/// # Errors
///
/// Returns [`BatchExecutionError`] for duplicate operation identities or
/// references to operations absent from the request.
pub fn resolve_dependencies(
    operations: &[ContractBatchOperation],
) -> Result<Vec<Vec<usize>>, BatchExecutionError> {
    let mut seen = BTreeSet::new();
    for operation in operations {
        if !seen.insert(operation.id.as_str()) {
            return Err(BatchExecutionError::DuplicateOperationId);
        }
    }

    let mut dependencies = Vec::new();
    dependencies
        .try_reserve_exact(operations.len())
        .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
    for operation in operations {
        let mut resolved = Vec::new();
        if let Some(declared) = &operation.depends_on {
            resolved
                .try_reserve_exact(declared.len())
                .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
            for name in declared {
                let index = operations
                    .iter()
                    .position(|other| other.id == *name)
                    .ok_or(BatchExecutionError::UnknownDependency)?;
                resolved.push(index);
            }
        }
        dependencies.push(resolved);
    }
    Ok(dependencies)
}

/// Reports whether any declared dependency failed to complete successfully.
#[must_use]
pub fn dependency_failed(dependencies: &[usize], results: &[Option<BatchOperationResult>]) -> bool {
    dependencies.iter().any(|index| {
        matches!(
            results[*index].as_ref().map(|result| result.status),
            Some(
                BatchOperationStatus::Error
                    | BatchOperationStatus::SkippedDependency
                    | BatchOperationStatus::NotRunFailFast
            )
        )
    })
}

/// Resolves typed bindings and injects batch-inherited request fields.
///
/// # Errors
///
/// Returns [`BatchExecutionError::InvalidBinding`] when a binding does not
/// target a declared completed dependency, or
/// [`BatchExecutionError::Serialization`] when an inherited field cannot be
/// encoded.
pub fn resolve_arguments(
    operation: &ContractBatchOperation,
    envelopes: &[Option<ReadEnvelope<Value>>],
    input: &QueryBatchInput,
    declared: &[usize],
) -> Result<ResolvedBatchArguments, BatchExecutionError> {
    let mut arguments = Map::new();
    let mut had_binding = false;
    for (key, value) in &operation.arguments {
        let resolved = resolve_binding(
            value,
            envelopes,
            &input.operations,
            declared,
            &mut had_binding,
        )?;
        arguments.insert(key.clone(), resolved);
    }
    arguments.insert(
        "repository".to_owned(),
        serde_json::to_value(&input.repository).map_err(|_| BatchExecutionError::Serialization)?,
    );
    if let Some(generation) = &input.generation {
        arguments.insert(
            "generation".to_owned(),
            serde_json::to_value(generation).map_err(|_| BatchExecutionError::Serialization)?,
        );
    }
    Ok(ResolvedBatchArguments {
        arguments,
        had_binding,
    })
}

/// Resolved child arguments plus whether dependency data was inserted.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBatchArguments {
    /// JSON arguments ready for dynamic child dispatch.
    pub arguments: Map<String, Value>,
    /// Whether at least one typed binding contributed a value.
    pub had_binding: bool,
}

/// Shapes one successful child response into a batch operation result.
#[must_use]
pub fn success_result(
    operation: &ContractBatchOperation,
    envelope: &ReadEnvelope<Value>,
) -> BatchOperationResult {
    BatchOperationResult {
        id: operation.id.clone(),
        tool: operation.tool,
        status: BatchOperationStatus::Ok,
        data: Some(envelope.data.clone()),
        error: None,
        truncated: envelope.truncated,
        next_cursor: envelope.next_cursor.clone(),
        usage: Some(envelope.usage.clone()),
        warnings: envelope.warnings.clone(),
    }
}

/// Shapes one checked child failure into a batch operation result.
#[must_use]
pub fn error_result(
    operation: &ContractBatchOperation,
    error: &PublicError,
) -> BatchOperationResult {
    BatchOperationResult {
        id: operation.id.clone(),
        tool: operation.tool,
        status: BatchOperationStatus::Error,
        data: None,
        error: Some(error.clone()),
        truncated: false,
        next_cursor: RequiredNullable(None),
        usage: None,
        warnings: Vec::new(),
    }
}

/// Shapes a child that was intentionally not executed.
#[must_use]
pub fn terminal_result(
    operation: &ContractBatchOperation,
    status: BatchOperationStatus,
) -> BatchOperationResult {
    BatchOperationResult {
        id: operation.id.clone(),
        tool: operation.tool,
        status,
        data: None,
        error: None,
        truncated: false,
        next_cursor: RequiredNullable(None),
        usage: None,
        warnings: Vec::new(),
    }
}

/// Derives the aggregate batch status from request-order child results.
#[must_use]
pub fn aggregate_status(results: &[BatchOperationResult]) -> BatchStatus {
    let any_ok = results
        .iter()
        .any(|result| result.status == BatchOperationStatus::Ok);
    let all_ok = results
        .iter()
        .all(|result| result.status == BatchOperationStatus::Ok);
    if all_ok {
        BatchStatus::Ok
    } else if any_ok {
        BatchStatus::Partial
    } else {
        BatchStatus::Error
    }
}

/// Aggregates child usage without double-counting parallel wall-clock time.
#[must_use]
pub fn aggregate_usage(envelopes: &[Option<ReadEnvelope<Value>>]) -> UsageSummary {
    let mut usage = UsageSummary {
        rows: 0,
        edges: 0,
        source_bytes: 0,
        json_bytes: 0,
        estimated_tokens: 0,
        wall_time_ms: 0,
        cache_status: CacheStatus::Miss,
        trace_id: "batch".to_owned(),
    };
    for envelope in envelopes.iter().flatten() {
        usage.rows = usage.rows.saturating_add(envelope.usage.rows);
        usage.edges = usage.edges.saturating_add(envelope.usage.edges);
        usage.source_bytes = usage
            .source_bytes
            .saturating_add(envelope.usage.source_bytes);
        usage.json_bytes = usage.json_bytes.saturating_add(envelope.usage.json_bytes);
        usage.estimated_tokens = usage
            .estimated_tokens
            .saturating_add(envelope.usage.estimated_tokens);
        usage.wall_time_ms = usage.wall_time_ms.max(envelope.usage.wall_time_ms);
    }
    usage
}

fn resolve_binding(
    value: &Value,
    envelopes: &[Option<ReadEnvelope<Value>>],
    operations: &[ContractBatchOperation],
    declared: &[usize],
    had_binding: &mut bool,
) -> Result<Value, BatchExecutionError> {
    match value {
        Value::Object(map) => {
            if let Some(from) = map.get("$from") {
                let from_name = from.as_str().ok_or(BatchExecutionError::InvalidBinding)?;
                let pointer = map
                    .get("pointer")
                    .and_then(Value::as_str)
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let dependency = declared
                    .iter()
                    .find(|&&index| operations[index].id == from_name)
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let envelope = envelopes[*dependency]
                    .as_ref()
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let encoded = serde_json::to_value(envelope)
                    .map_err(|_| BatchExecutionError::Serialization)?;
                let resolved = encoded
                    .pointer(pointer)
                    .cloned()
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                *had_binding = true;
                Ok(resolved)
            } else {
                let mut resolved = Map::new();
                for (key, inner) in map {
                    resolved.insert(
                        key.clone(),
                        resolve_binding(inner, envelopes, operations, declared, had_binding)?,
                    );
                }
                Ok(Value::Object(resolved))
            }
        }
        Value::Array(items) => {
            let mut resolved = Vec::new();
            resolved
                .try_reserve_exact(items.len())
                .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
            for inner in items {
                resolved.push(resolve_binding(
                    inner,
                    envelopes,
                    operations,
                    declared,
                    had_binding,
                )?);
            }
            Ok(Value::Array(resolved))
        }
        scalar => Ok(scalar.clone()),
    }
}

/// Default aggregate token ceiling for a batch without an explicit budget.
pub const DEFAULT_BATCH_TOKENS: u16 = 3_000;

/// Checked public failures injected into transport-neutral batch orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPublicErrors {
    binding_invalid: PublicError,
    binding_type_mismatch: PublicError,
    operation_failed: PublicError,
    budget_exceeded: PublicError,
}

impl BatchPublicErrors {
    /// Creates the source-free public failures used by batch result shaping.
    #[must_use]
    pub const fn new(
        binding_invalid: PublicError,
        binding_type_mismatch: PublicError,
        operation_failed: PublicError,
        budget_exceeded: PublicError,
    ) -> Self {
        Self {
            binding_invalid,
            binding_type_mismatch,
            operation_failed,
            budget_exceeded,
        }
    }
}

/// Complete per-operation outcome when no child produced a usable envelope.
///
/// The composing adapter adds repository and generation metadata from its
/// independently pinned status read without promoting child failures to a
/// top-level domain error.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchUnsuccessfulOutput {
    /// Results in the original request order.
    pub operation_results: Vec<BatchOperationResult>,
    /// Whether any observed child reported truncation.
    pub truncated: bool,
    /// Aggregate usage observed before all operations failed.
    pub usage: UsageSummary,
    /// Source-free warnings retained from observed child envelopes.
    pub warnings: Vec<ResponseWarning>,
}

/// Failure returned by complete transport-neutral batch orchestration.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum BatchOrchestrationError {
    /// The request violated a batch invariant.
    InvalidArguments,
    /// The requested response profile is not implemented.
    UnsupportedProfile,
    /// Cooperative cancellation won.
    Cancelled,
    /// The parent deadline elapsed.
    DeadlineExceeded,
    /// Aggregate resource usage exceeded the parent budget.
    BudgetExceeded,
    /// No operation produced a successful generation-pinned result.
    NoSuccessfulOperation(Box<BatchUnsuccessfulOutput>),
    /// A child response violated repository or generation invariants.
    InvalidResponse,
    /// A bounded allocation or serialization operation failed.
    Internal,
}

/// Complete application service for one public `query.batch` request.
#[derive(Debug, Clone, Copy, Default)]
pub struct BatchService;

impl BatchService {
    /// Executes a validated batch through a client-free agent tool port.
    ///
    /// The service owns dependency scheduling, fail-fast behavior, typed
    /// bindings, parent and child budget enforcement, cancellation checkpoints,
    /// deadline propagation, result shaping, and aggregate accounting.
    ///
    /// # Errors
    ///
    /// Returns [`BatchOrchestrationError`] when request admission fails,
    /// cancellation or a deadline wins, the parent budget is exhausted, no
    /// operation succeeds, or a child violates the port contract.
    pub async fn execute<P, C>(
        &self,
        port: Arc<P>,
        input: QueryBatchInput,
        repository: RepositoryId,
        cancellation: C,
        errors: BatchPublicErrors,
    ) -> Result<ReadEnvelope<QueryBatchData>, BatchOrchestrationError>
    where
        P: AgentToolPort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        checkpoint(&cancellation)?;
        if !is_compact_profile(input.response_profile) {
            return Err(BatchOrchestrationError::UnsupportedProfile);
        }

        let dependencies = resolve_dependencies(&input.operations).map_err(map_execution_error)?;
        let tools: Vec<McpTool> = input
            .operations
            .iter()
            .map(|operation| mcp_tool_for_batch(operation.tool))
            .collect();
        let plan = BatchPlan::validate(&tools, &dependencies)
            .map_err(|_| BatchOrchestrationError::InvalidArguments)?;

        let parent_budget = admitted_parent_budget(input.budget.as_ref());
        if parent_budget.evidence_level.is_some() {
            return Err(BatchOrchestrationError::InvalidArguments);
        }
        let mut parent_ledger = BudgetLedger::new(Some(parent_budget.clone()));
        parent_ledger
            .charge(BudgetCharge {
                results: u64::try_from(input.operations.len()).unwrap_or(u64::MAX),
                ..BudgetCharge::default()
            })
            .map_err(map_policy_error)?;

        let started_at = Instant::now();
        let parent_deadline = deadline_from(started_at, parent_budget.timeout_ms)?;
        let fail_fast = matches!(input.failure_policy, Some(FailurePolicy::FailFast));
        let count = input.operations.len();
        let mut results: Vec<Option<BatchOperationResult>> = vec![None; count];
        let mut binding_envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
        let mut observed_envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
        let mut stop_scheduling = false;

        for index in plan.execution_order {
            checkpoint(&cancellation)?;
            check_deadline(parent_deadline)?;
            let operation = &input.operations[index];
            if dependency_failed(&dependencies[index], &results) {
                results[index] = Some(terminal_result(
                    operation,
                    BatchOperationStatus::SkippedDependency,
                ));
                continue;
            }
            if stop_scheduling {
                results[index] = Some(terminal_result(
                    operation,
                    BatchOperationStatus::NotRunFailFast,
                ));
                continue;
            }

            let resolved = match resolve_arguments(
                operation,
                &binding_envelopes,
                &input,
                &dependencies[index],
            ) {
                Ok(resolved) => resolved,
                Err(BatchExecutionError::InvalidBinding) => {
                    results[index] = Some(error_result(operation, &errors.binding_invalid));
                    stop_scheduling |= fail_fast;
                    continue;
                }
                Err(
                    BatchExecutionError::DuplicateOperationId
                    | BatchExecutionError::UnknownDependency,
                ) => return Err(BatchOrchestrationError::InvalidArguments),
                Err(
                    BatchExecutionError::Serialization | BatchExecutionError::MemoryUnavailable,
                ) => return Err(BatchOrchestrationError::Internal),
            };
            let child_budget =
                effective_child_budget(&parent_budget, operation.local_budget.as_ref());
            let child_deadline =
                effective_child_deadline(parent_deadline, child_budget.timeout_ms)?;
            let context =
                AgentCallContext::new(cancellation.clone(), child_budget.clone(), child_deadline);
            let request = AgentToolRequest::new(operation.tool, resolved.arguments);

            match port.execute(request, context).await {
                Ok(envelope) => {
                    checkpoint(&cancellation)?;
                    check_deadline(parent_deadline)?;
                    validate_child_identity(&envelope, repository, &binding_envelopes)?;

                    let charge = charge_for(&envelope.usage);
                    parent_ledger.charge(charge).map_err(map_policy_error)?;
                    observed_envelopes[index] = Some(envelope.clone());

                    let mut child_ledger = BudgetLedger::new(Some(child_budget));
                    if child_ledger
                        .charge(BudgetCharge {
                            results: 1,
                            ..charge
                        })
                        .is_err()
                    {
                        results[index] = Some(error_result(operation, &errors.budget_exceeded));
                        stop_scheduling |= fail_fast;
                        continue;
                    }
                    results[index] = Some(success_result(operation, &envelope));
                    binding_envelopes[index] = Some(envelope);
                }
                Err(AgentPortError::Public(error)) => {
                    let public = if resolved.had_binding && error.code() == ErrorCode::TypeMismatch
                    {
                        &errors.binding_type_mismatch
                    } else {
                        &error
                    };
                    results[index] = Some(error_result(operation, public));
                    stop_scheduling |= fail_fast;
                }
                Err(AgentPortError::Cancelled) => {
                    return Err(BatchOrchestrationError::Cancelled);
                }
                Err(AgentPortError::DeadlineExceeded) => {
                    return Err(BatchOrchestrationError::DeadlineExceeded);
                }
                Err(AgentPortError::InvalidResponse) => {
                    return Err(BatchOrchestrationError::InvalidResponse);
                }
                Err(AgentPortError::Unavailable) => {
                    results[index] = Some(error_result(operation, &errors.operation_failed));
                    stop_scheduling |= fail_fast;
                }
            }
        }

        checkpoint(&cancellation)?;
        check_deadline(parent_deadline)?;
        let operation_results: Vec<BatchOperationResult> = results.into_iter().flatten().collect();
        let truncated = operation_results.iter().any(|result| result.truncated);
        let usage = aggregate_usage(&observed_envelopes);
        let Some(source) = binding_envelopes.iter().flatten().next() else {
            let warnings = observed_envelopes
                .iter()
                .flatten()
                .flat_map(|envelope| envelope.warnings.iter().cloned())
                .collect();
            return Err(BatchOrchestrationError::NoSuccessfulOperation(Box::new(
                BatchUnsuccessfulOutput {
                    operation_results,
                    truncated,
                    usage,
                    warnings,
                },
            )));
        };

        let data = QueryBatchData {
            batch_status: aggregate_status(&operation_results),
            generation_id: source.generation.generation_id,
            operation_results,
            explanation: None,
        };
        Ok(ReadEnvelope {
            schema_version: SchemaVersion::V1_0,
            repository: ResolvedRepository {
                repository_id: repository,
                display_name: source.repository.display_name.clone(),
            },
            generation: source.generation.clone(),
            coverage: CoverageSummary {
                status: rootlight_ir::CoverageStatus::Bounded,
                languages: source.coverage.languages.clone(),
                skipped_inputs: source.coverage.skipped_inputs,
            },
            data,
            truncated,
            next_cursor: RequiredNullable(None),
            usage,
            warnings: source.warnings.clone(),
            trust: TrustClassification::UntrustedRepositoryData,
        })
    }
}

fn admitted_parent_budget(requested: Option<&ResponseBudget>) -> ResponseBudget {
    let mut admitted = requested.cloned().unwrap_or(ResponseBudget {
        max_results: None,
        max_tokens: None,
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: None,
        evidence_level: None,
    });
    admitted.max_tokens = Some(
        admitted
            .max_tokens
            .unwrap_or(DEFAULT_BATCH_TOKENS)
            .min(16_000),
    );
    admitted
}

fn effective_child_budget(
    parent: &ResponseBudget,
    local: Option<&ResponseBudget>,
) -> ResponseBudget {
    let Some(local) = local else {
        return parent.clone();
    };
    ResponseBudget {
        max_results: minimum(parent.max_results, local.max_results),
        max_tokens: minimum(parent.max_tokens, local.max_tokens),
        max_source_bytes: minimum(parent.max_source_bytes, local.max_source_bytes),
        max_traversal_facts: minimum(parent.max_traversal_facts, local.max_traversal_facts),
        max_depth: minimum(parent.max_depth, local.max_depth),
        max_paths: minimum(parent.max_paths, local.max_paths),
        timeout_ms: minimum(parent.timeout_ms, local.timeout_ms),
        evidence_level: minimum_evidence(parent.evidence_level, local.evidence_level),
    }
}

fn minimum<T>(parent: Option<T>, local: Option<T>) -> Option<T>
where
    T: Ord + Copy,
{
    match (parent, local) {
        (Some(parent), Some(local)) => Some(parent.min(local)),
        (Some(parent), None) => Some(parent),
        (None, Some(local)) => Some(local),
        (None, None) => None,
    }
}

fn minimum_evidence(
    parent: Option<rootlight_mcp_contract::vertical::ProvenanceLevel>,
    local: Option<rootlight_mcp_contract::vertical::ProvenanceLevel>,
) -> Option<rootlight_mcp_contract::vertical::ProvenanceLevel> {
    use rootlight_mcp_contract::vertical::ProvenanceLevel;

    fn rank(value: ProvenanceLevel) -> u8 {
        match value {
            ProvenanceLevel::None => 0,
            ProvenanceLevel::Compact => 1,
            ProvenanceLevel::Full => 2,
        }
    }

    match (parent, local) {
        (Some(parent), Some(local)) => Some(if rank(parent) <= rank(local) {
            parent
        } else {
            local
        }),
        (Some(parent), None) => Some(parent),
        (None, Some(local)) => Some(local),
        (None, None) => None,
    }
}

fn deadline_from(
    started_at: Instant,
    timeout_ms: Option<u32>,
) -> Result<Option<Instant>, BatchOrchestrationError> {
    timeout_ms
        .map(|timeout_ms| {
            started_at
                .checked_add(Duration::from_millis(u64::from(timeout_ms)))
                .ok_or(BatchOrchestrationError::Internal)
        })
        .transpose()
}

fn effective_child_deadline(
    parent: Option<Instant>,
    timeout_ms: Option<u32>,
) -> Result<Option<Instant>, BatchOrchestrationError> {
    let local = deadline_from(Instant::now(), timeout_ms)?;
    Ok(match (parent, local) {
        (Some(parent), Some(local)) => Some(parent.min(local)),
        (Some(parent), None) => Some(parent),
        (None, Some(local)) => Some(local),
        (None, None) => None,
    })
}

fn checkpoint<C>(cancellation: &C) -> Result<(), BatchOrchestrationError>
where
    C: CancellationSignal,
{
    if cancellation.is_cancelled() {
        Err(BatchOrchestrationError::Cancelled)
    } else {
        Ok(())
    }
}

fn check_deadline(deadline: Option<Instant>) -> Result<(), BatchOrchestrationError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(BatchOrchestrationError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn charge_for(usage: &UsageSummary) -> BudgetCharge {
    BudgetCharge {
        results: 0,
        tokens: usage.estimated_tokens,
        source_bytes: usage.source_bytes,
        traversal_facts: usage.edges,
        depth: 0,
        paths: 0,
        time_ms: usage.wall_time_ms,
    }
}

fn validate_child_identity(
    envelope: &ReadEnvelope<Value>,
    repository: RepositoryId,
    completed: &[Option<ReadEnvelope<Value>>],
) -> Result<(), BatchOrchestrationError> {
    if envelope.repository.repository_id != repository {
        return Err(BatchOrchestrationError::InvalidResponse);
    }
    if let Some(first) = completed.iter().flatten().next()
        && envelope.generation.generation_id != first.generation.generation_id
    {
        return Err(BatchOrchestrationError::InvalidResponse);
    }
    Ok(())
}

fn map_execution_error(error: BatchExecutionError) -> BatchOrchestrationError {
    match error {
        BatchExecutionError::DuplicateOperationId
        | BatchExecutionError::UnknownDependency
        | BatchExecutionError::InvalidBinding => BatchOrchestrationError::InvalidArguments,
        BatchExecutionError::Serialization | BatchExecutionError::MemoryUnavailable => {
            BatchOrchestrationError::Internal
        }
    }
}

fn map_policy_error(error: ExecutionPolicyError) -> BatchOrchestrationError {
    match error {
        ExecutionPolicyError::Cancelled => BatchOrchestrationError::Cancelled,
        ExecutionPolicyError::BudgetExceeded { .. } => BatchOrchestrationError::BudgetExceeded,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BatchExecutionError, BatchPlan, BatchValidationError, MAX_BATCH_DEPTH,
        MAX_BATCH_OPERATIONS, aggregate_status, is_batch_allowed, is_batch_allowed_under_profile,
        resolve_dependencies, terminal_result,
    };
    use rootlight_mcp_contract::{
        ExposureProfile, McpTool,
        context::{
            BatchOperation as ContractBatchOperation, BatchOperationStatus, BatchStatus, BatchTool,
        },
    };
    use serde_json::Map;

    fn contract_operation(
        id: &str,
        tool: BatchTool,
        depends_on: Option<Vec<&str>>,
    ) -> ContractBatchOperation {
        ContractBatchOperation {
            id: id.to_owned(),
            tool,
            depends_on: depends_on
                .map(|dependencies| dependencies.into_iter().map(str::to_owned).collect()),
            arguments: Map::new(),
            local_budget: None,
        }
    }

    #[test]
    fn batch_allowlist_aliases_the_capability_registry() {
        // The batch validator must not maintain a parallel eligibility list; it
        // aliases the capability registry, the single source of truth.
        assert_eq!(
            super::BATCH_ALLOWLIST,
            rootlight_mcp_contract::capability::BATCH_ELIGIBLE
        );
    }

    #[test]
    fn empty_batch_is_rejected() {
        assert_eq!(
            BatchPlan::validate(&[], &[]),
            Err(BatchValidationError::InvalidOperationCount)
        );
    }

    #[test]
    fn oversized_batch_is_rejected() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_OPERATIONS + 1];
        let deps = vec![vec![]; MAX_BATCH_OPERATIONS + 1];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::InvalidOperationCount)
        );
    }

    #[test]
    fn maximum_sixteen_operations_are_accepted() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_OPERATIONS];
        let deps = vec![vec![]; MAX_BATCH_OPERATIONS];
        assert!(BatchPlan::validate(&tools, &deps).is_ok());
    }

    #[test]
    fn forbidden_tools_are_rejected() {
        let tools = [McpTool::RepoIndex];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );

        let tools = [McpTool::QueryAdvanced];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );

        let tools = [McpTool::HistoryCompare];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );
    }

    #[test]
    fn nested_batch_is_rejected() {
        let tools = [McpTool::QueryBatch];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );
    }

    #[test]
    fn cyclic_dependency_is_rejected() {
        let tools = [McpTool::CodeLocate, McpTool::SymbolExplain];
        let deps = [vec![1], vec![0]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::CyclicDependency)
        );
    }

    #[test]
    fn self_dependency_is_rejected() {
        let tools = [McpTool::CodeLocate];
        let deps = [vec![0]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::InvalidDependencyReference)
        );
    }

    #[test]
    fn out_of_range_dependency_is_rejected() {
        let tools = [McpTool::CodeLocate];
        let deps = [vec![5]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::InvalidDependencyReference)
        );
    }

    #[test]
    fn depth_eight_is_accepted() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_DEPTH + 1];
        let deps: Vec<Vec<usize>> = (0..=MAX_BATCH_DEPTH)
            .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
            .collect();
        let plan = BatchPlan::validate(&tools, &deps).expect("depth 8 is valid");
        assert_eq!(plan.max_depth(), MAX_BATCH_DEPTH);
    }

    #[test]
    fn depth_nine_is_rejected() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_DEPTH + 2];
        let deps: Vec<Vec<usize>> = (0..=MAX_BATCH_DEPTH + 1)
            .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
            .collect();
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::DepthExceeded)
        );
    }

    #[test]
    fn execution_order_is_topologically_valid() {
        let tools = [
            McpTool::CodeLocate,
            McpTool::SymbolExplain,
            McpTool::FlowTrace,
        ];
        let deps = [vec![], vec![0], vec![0, 1]];
        let plan = BatchPlan::validate(&tools, &deps).expect("valid DAG");
        let order = &plan.execution_order;
        let pos = |idx: usize| order.iter().position(|i| *i == idx).unwrap();
        assert!(pos(0) < pos(1));
        assert!(pos(0) < pos(2));
        assert!(pos(1) < pos(2));
    }

    #[test]
    fn batch_allowlist_matches_profile_intersection() {
        for tool in McpTool::ALL {
            let allowed = is_batch_allowed(tool);
            let scout_allowed = is_batch_allowed_under_profile(tool, ExposureProfile::Scout);
            if scout_allowed {
                assert!(allowed, "scout batch tool must be in allowlist");
                assert!(
                    ExposureProfile::Scout.exposes(tool),
                    "scout batch tool must be visible in scout"
                );
            }
        }
    }

    #[test]
    fn too_many_dependencies_per_operation_is_rejected() {
        let tools = vec![McpTool::CodeLocate; 10];
        let mut deps = vec![vec![]; 10];
        deps[9] = (0..9).collect();
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::TooManyDependencies)
        );
    }

    #[test]
    fn request_identities_resolve_to_request_order_indices() {
        let operations = [
            contract_operation("find", BatchTool::CodeLocate, None),
            contract_operation("explain", BatchTool::SymbolExplain, Some(vec!["find"])),
        ];

        assert_eq!(resolve_dependencies(&operations), Ok(vec![vec![], vec![0]]));
    }

    #[test]
    fn duplicate_and_unknown_dependencies_are_typed_errors() {
        let duplicate = [
            contract_operation("same", BatchTool::CodeLocate, None),
            contract_operation("same", BatchTool::SymbolExplain, None),
        ];
        assert_eq!(
            resolve_dependencies(&duplicate),
            Err(BatchExecutionError::DuplicateOperationId)
        );

        let unknown = [contract_operation(
            "explain",
            BatchTool::SymbolExplain,
            Some(vec!["missing"]),
        )];
        assert_eq!(
            resolve_dependencies(&unknown),
            Err(BatchExecutionError::UnknownDependency)
        );
    }

    #[test]
    fn aggregate_status_is_derived_from_child_terminal_states() {
        let operation = contract_operation("find", BatchTool::CodeLocate, None);
        let error = terminal_result(&operation, BatchOperationStatus::Error);
        assert_eq!(aggregate_status(&[error]), BatchStatus::Error);

        let ok = terminal_result(&operation, BatchOperationStatus::Ok);
        let skipped = terminal_result(&operation, BatchOperationStatus::SkippedDependency);
        assert_eq!(aggregate_status(&[ok, skipped]), BatchStatus::Partial);
    }
}
