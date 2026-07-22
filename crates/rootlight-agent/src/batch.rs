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
    McpTool, PublicError, SchemaVersion, TrustClassification,
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
        LimitingResourceKind, ResultCompleteness,
    },
    context::{
        BatchOperation as ContractBatchOperation, BatchOperationResult, BatchOperationStatus,
        BatchStatus, BatchTool, FailurePolicy, QueryBatchData, QueryBatchInput,
    },
    vertical::{
        CacheStatus, GenerationSelector, ReadEnvelope, RepositoryIdSelector, RequiredNullable,
        ResponseBudget, UsageSummary,
    },
};
use serde_json::{Map, Value};

use crate::{
    policy::{
        BudgetAllocation, BudgetCharge, BudgetLedger, BudgetLimits, CancellationSignal,
        ExecutionPolicyError, is_compact_profile,
    },
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentResolutionContext,
        AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
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
pub const BATCH_ALLOWLIST: [McpTool; 12] = rootlight_mcp_contract::capability::BATCH_ELIGIBLE;

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
    /// An operation or dependency identifier violates the public grammar.
    #[error("batch operation identifier is invalid")]
    InvalidOperationId,
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
        if !batch_operation_id_is_valid(&operation.id)
            || operation
                .depends_on
                .iter()
                .flatten()
                .any(|dependency| !batch_operation_id_is_valid(dependency))
        {
            return Err(BatchExecutionError::InvalidOperationId);
        }
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
    let mut materialized_binding_paths = Vec::new();
    for (key, value) in &operation.arguments {
        let destination = append_pointer("", key)?;
        let resolved = resolve_binding(
            value,
            envelopes,
            &input.operations,
            declared,
            &destination,
            &mut materialized_binding_paths,
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
        materialized_binding_paths,
    })
}

/// Resolved child arguments plus exact dependency-binding destinations.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBatchArguments {
    /// JSON arguments ready for dynamic child dispatch.
    pub arguments: Map<String, Value>,
    /// JSON Pointer destinations populated from dependency outputs.
    pub materialized_binding_paths: Vec<String>,
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

fn aggregate_completeness(
    envelopes: &[Option<ReadEnvelope<Value>>],
    aggregate_truncated: bool,
) -> Result<ResultCompleteness, BatchOrchestrationError> {
    let mut state = CompletenessState::Complete;
    let mut resources = Vec::new();
    let mut guidance = Vec::new();
    for envelope in envelopes.iter().flatten() {
        state = state.max(envelope.completeness.state);
        resources.extend(envelope.completeness.limiting_resources.iter().copied());
        guidance.extend(
            envelope
                .completeness
                .guidance
                .iter()
                .copied()
                .filter(|value| *value != ContinuationGuidance::UseCursor),
        );
        if envelope.truncated && envelope.completeness.state == CompletenessState::Complete {
            state = CompletenessState::Truncated;
            resources.push(LimitingResource::kind(LimitingResourceKind::Results));
        }
    }
    if aggregate_truncated && state == CompletenessState::Complete {
        state = CompletenessState::Truncated;
        resources.push(LimitingResource::kind(LimitingResourceKind::Results));
    }
    if state == CompletenessState::Complete {
        return Ok(ResultCompleteness::complete());
    }
    guidance.push(ContinuationGuidance::SplitRequest);
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
    .map_err(|_| BatchOrchestrationError::InvalidResponse)
}

fn resolve_binding(
    value: &Value,
    envelopes: &[Option<ReadEnvelope<Value>>],
    operations: &[ContractBatchOperation],
    declared: &[usize],
    destination: &str,
    materialized_binding_paths: &mut Vec<String>,
) -> Result<Value, BatchExecutionError> {
    match value {
        Value::Object(map) => {
            if let Some((from_name, pointer)) = binding_reference(map)? {
                let dependency = declared
                    .iter()
                    .find(|&&index| operations[index].id == from_name)
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let envelope = envelopes[*dependency]
                    .as_ref()
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let data_pointer = pointer
                    .strip_prefix("/data")
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let resolved = envelope
                    .data
                    .pointer(data_pointer)
                    .cloned()
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                materialized_binding_paths
                    .try_reserve(1)
                    .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
                materialized_binding_paths.push(destination.to_owned());
                Ok(resolved)
            } else {
                let mut resolved = Map::new();
                for (key, inner) in map {
                    let child_destination = append_pointer(destination, key)?;
                    resolved.insert(
                        key.clone(),
                        resolve_binding(
                            inner,
                            envelopes,
                            operations,
                            declared,
                            &child_destination,
                            materialized_binding_paths,
                        )?,
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
            for (index, inner) in items.iter().enumerate() {
                let child_destination = append_pointer(destination, &index.to_string())?;
                resolved.push(resolve_binding(
                    inner,
                    envelopes,
                    operations,
                    declared,
                    &child_destination,
                    materialized_binding_paths,
                )?);
            }
            Ok(Value::Array(resolved))
        }
        scalar => Ok(scalar.clone()),
    }
}

fn append_pointer(parent: &str, segment: &str) -> Result<String, BatchExecutionError> {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    let mut pointer = String::new();
    pointer
        .try_reserve(parent.len().saturating_add(escaped.len()).saturating_add(1))
        .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
    pointer.push_str(parent);
    pointer.push('/');
    pointer.push_str(&escaped);
    Ok(pointer)
}

fn batch_operation_id_is_valid(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn binding_reference(
    map: &Map<String, Value>,
) -> Result<Option<(&str, &str)>, BatchExecutionError> {
    let Some(from) = map.get("$from") else {
        return Ok(None);
    };
    if map.len() != 2 {
        return Err(BatchExecutionError::InvalidBinding);
    }
    let from = from.as_str().ok_or(BatchExecutionError::InvalidBinding)?;
    let pointer = map
        .get("pointer")
        .and_then(Value::as_str)
        .ok_or(BatchExecutionError::InvalidBinding)?;
    if !batch_operation_id_is_valid(from)
        || !(1..=1024).contains(&pointer.len())
        || !binding_pointer_is_allowed(pointer)
    {
        return Err(BatchExecutionError::InvalidBinding);
    }
    Ok(Some((from, pointer)))
}

fn binding_objects_are_valid(arguments: &Map<String, Value>) -> bool {
    arguments.values().all(binding_value_is_valid)
}

fn binding_value_is_valid(value: &Value) -> bool {
    match value {
        Value::Object(map) => match binding_reference(map) {
            Ok(Some(_)) => true,
            Ok(None) => map.values().all(binding_value_is_valid),
            Err(_) => false,
        },
        Value::Array(items) => items.iter().all(binding_value_is_valid),
        _ => true,
    }
}

/// Default aggregate token ceiling for a batch without an explicit budget.
pub const DEFAULT_BATCH_TOKENS: u16 = 3_000;

/// Default wall-clock ceiling for a batch without an explicit timeout.
pub const DEFAULT_BATCH_TIMEOUT_MS: u32 = 30_000;

/// Restricts bindings to typed identifiers and source references under the
/// child data payload.
///
/// The closed leaf set deliberately excludes envelope metadata, warnings,
/// display text, snippets, rationale, and other repository-controlled strings.
/// Materialized values still pass the destination tool's strict schema before
/// dispatch, which verifies target type compatibility.
fn binding_pointer_is_allowed(pointer: &str) -> bool {
    let mut segments = pointer.split('/').peekable();
    if segments.next() != Some("") || segments.next() != Some("data") {
        return false;
    }
    let mut saw_leaf = false;
    while let Some(segment) = segments.next() {
        if segment.is_empty() || segment.contains('~') || saw_leaf {
            return false;
        }
        let is_last = segments.peek().is_none();
        if is_last {
            saw_leaf = matches!(
                segment,
                "symbol_id"
                    | "symbol_ids"
                    | "source_ref"
                    | "source_refs"
                    | "definition"
                    | "nodes"
                    | "test_id"
                    | "pack_id"
            );
        } else if !segment.bytes().all(|byte| byte.is_ascii_digit())
            && !matches!(
                segment,
                "matches"
                    | "symbols"
                    | "paths"
                    | "nodes"
                    | "tests"
                    | "components"
                    | "cycles"
                    | "candidates"
            )
        {
            return false;
        }
    }
    saw_leaf
}

/// Checked public failures injected into transport-neutral batch orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPublicErrors {
    binding_invalid: PublicError,
    operation_failed: PublicError,
    budget_exceeded: PublicError,
}

impl BatchPublicErrors {
    /// Creates the source-free public failures used by batch result shaping.
    #[must_use]
    pub const fn new(
        binding_invalid: PublicError,
        operation_failed: PublicError,
        budget_exceeded: PublicError,
    ) -> Self {
        Self {
            binding_invalid,
            operation_failed,
            budget_exceeded,
        }
    }
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
    /// Repository or generation identity could not be resolved.
    IdentityResolution(Box<PublicError>),
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
    /// cancellation or a deadline wins, the parent budget is exhausted,
    /// identity resolution fails, or a child violates the port contract.
    pub async fn execute<P, C>(
        &self,
        port: Arc<P>,
        mut input: QueryBatchInput,
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
        if input.operations.iter().any(|operation| {
            operation.arguments.keys().any(|key| {
                matches!(
                    key.as_str(),
                    "repository"
                        | "generation"
                        | "budget"
                        | "cursor"
                        | "profile"
                        | "response_profile"
                )
            }) || !binding_objects_are_valid(&operation.arguments)
        }) {
            return Err(BatchOrchestrationError::InvalidArguments);
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
        let started_at = Instant::now();
        let parent_deadline = deadline_from(started_at, parent_budget.timeout_ms)?
            .ok_or(BatchOrchestrationError::Internal)?;
        let identity = resolve_identity(
            Arc::clone(&port),
            &input,
            cancellation.clone(),
            parent_deadline,
        )
        .await?;
        if identity.repository.repository_id != repository {
            return Err(BatchOrchestrationError::InvalidResponse);
        }
        input.repository = rootlight_mcp_contract::RepositorySelector::ById(RepositoryIdSelector {
            repository_id: identity.repository.repository_id,
        });
        input.generation = Some(GenerationSelector::Explicit(
            identity.generation.generation_id,
        ));

        let mut parent_ledger = BudgetLedger::new(Some(parent_budget.clone()));
        let fail_fast = matches!(input.failure_policy, Some(FailurePolicy::FailFast));
        let count = input.operations.len();
        let mut results: Vec<Option<BatchOperationResult>> = vec![None; count];
        let mut binding_envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
        let mut observed_envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
        let mut stop_scheduling = false;

        for index in plan.execution_order {
            checkpoint(&cancellation)?;
            check_deadline(Some(parent_deadline))?;
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
                    BatchExecutionError::InvalidOperationId
                    | BatchExecutionError::DuplicateOperationId
                    | BatchExecutionError::UnknownDependency,
                ) => return Err(BatchOrchestrationError::InvalidArguments),
                Err(
                    BatchExecutionError::Serialization | BatchExecutionError::MemoryUnavailable,
                ) => return Err(BatchOrchestrationError::Internal),
            };
            let remaining = match remaining_parent_budget(
                &parent_budget,
                parent_ledger.consumed(),
                started_at.elapsed(),
            ) {
                Ok(remaining) => remaining,
                Err(BatchOrchestrationError::BudgetExceeded) => {
                    results[index] = Some(error_result(operation, &errors.budget_exceeded));
                    stop_scheduling |= fail_fast;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let tool_limits =
                BudgetLimits::server_ceiling().constrained_by_response_budget(Some(&remaining));
            let mut allocation =
                match parent_ledger.allocate_child(tool_limits, operation.local_budget.as_ref()) {
                    Ok(allocation) => allocation,
                    Err(ExecutionPolicyError::BudgetExceeded { .. }) => {
                        results[index] = Some(error_result(operation, &errors.budget_exceeded));
                        stop_scheduling |= fail_fast;
                        continue;
                    }
                    Err(error) => return Err(map_policy_error(error)),
                };
            let child_budget = response_budget_from_limits(
                allocation.limits(),
                minimum_evidence(
                    remaining.evidence_level,
                    operation
                        .local_budget
                        .as_ref()
                        .and_then(|budget| budget.evidence_level),
                ),
            );
            let effective_deadline = effective_child_deadline(
                parent_deadline,
                operation
                    .local_budget
                    .as_ref()
                    .and_then(|budget| budget.timeout_ms),
                Instant::now(),
            )?;
            let context = AgentCallContext::new(
                cancellation.clone(),
                child_budget.clone(),
                Some(effective_deadline.at),
            )
            .with_local_budget(operation.local_budget.clone())
            .with_local_deadline(effective_deadline.source == DeadlineSource::Local);
            let request = AgentToolRequest::new(operation.tool, resolved.arguments)
                .with_materialized_binding_paths(resolved.materialized_binding_paths);

            match port.execute(request, context).await {
                Ok(envelope) => {
                    checkpoint(&cancellation)?;
                    check_deadline(Some(parent_deadline))?;
                    validate_child_identity(&envelope, &identity)?;

                    let charge = charge_for(operation.tool, &envelope)?;
                    observed_envelopes[index] = Some(envelope.clone());

                    if allocation.ledger_mut().charge(charge).is_err() {
                        consume_allocation(allocation)?;
                        results[index] = Some(error_result(operation, &errors.budget_exceeded));
                        stop_scheduling |= fail_fast;
                        continue;
                    }
                    allocation.commit().map_err(map_policy_error)?;
                    results[index] = Some(success_result(operation, &envelope));
                    binding_envelopes[index] = Some(envelope);
                }
                Err(AgentPortError::Public(error)) => {
                    results[index] = Some(error_result(operation, &error));
                    stop_scheduling |= fail_fast;
                }
                Err(AgentPortError::Cancelled) => {
                    return Err(BatchOrchestrationError::Cancelled);
                }
                Err(AgentPortError::DeadlineExceeded) => {
                    return Err(BatchOrchestrationError::DeadlineExceeded);
                }
                Err(AgentPortError::LocalDeadlineExceeded) => {
                    results[index] = Some(error_result(operation, &errors.budget_exceeded));
                    stop_scheduling |= fail_fast;
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
        check_deadline(Some(parent_deadline))?;
        let operation_results: Vec<BatchOperationResult> = results.into_iter().flatten().collect();
        let truncated = operation_results.iter().any(|result| result.truncated);
        let completeness = aggregate_completeness(&observed_envelopes, truncated)?;
        let usage = aggregate_usage(&observed_envelopes);
        let data = QueryBatchData {
            batch_status: aggregate_status(&operation_results),
            generation_id: identity.generation.generation_id,
            operation_results,
            explanation: None,
        };
        Ok(ReadEnvelope {
            schema_version: SchemaVersion::V1_0,
            repository: identity.repository,
            generation: identity.generation,
            coverage: identity.coverage,
            data,
            truncated,
            completeness,
            next_cursor: RequiredNullable(None),
            usage,
            warnings: identity.warnings,
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
    admitted.timeout_ms = Some(
        admitted
            .timeout_ms
            .unwrap_or(DEFAULT_BATCH_TIMEOUT_MS)
            .min(DEFAULT_BATCH_TIMEOUT_MS),
    );
    let maximums = BudgetLimits::server_ceiling()
        .constrained_by_response_budget(Some(&admitted))
        .maximums();
    ResponseBudget {
        max_results: Some(u16::try_from(maximums.results).unwrap_or(u16::MAX)),
        max_tokens: Some(u16::try_from(maximums.tokens).unwrap_or(u16::MAX)),
        max_source_bytes: Some(u32::try_from(maximums.source_bytes).unwrap_or(u32::MAX)),
        max_traversal_facts: Some(u32::try_from(maximums.traversal_facts).unwrap_or(u32::MAX)),
        max_depth: Some(u8::try_from(maximums.depth).unwrap_or(u8::MAX)),
        max_paths: Some(u16::try_from(maximums.paths).unwrap_or(u16::MAX)),
        timeout_ms: Some(u32::try_from(maximums.time_ms).unwrap_or(u32::MAX)),
        evidence_level: admitted.evidence_level,
    }
}

async fn resolve_identity<P, C>(
    port: Arc<P>,
    input: &QueryBatchInput,
    cancellation: C,
    deadline: Instant,
) -> Result<AgentResolvedIdentity, BatchOrchestrationError>
where
    P: AgentToolPort<C>,
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    let request = AgentIdentityRequest::new(input.repository.clone(), input.generation.clone());
    let context = AgentResolutionContext::new(cancellation, deadline);
    match port.resolve_identity(request, context).await {
        Ok(identity)
            if matches!(
                input.generation.as_ref(),
                Some(GenerationSelector::Explicit(expected))
                    if identity.generation.generation_id != *expected
            ) =>
        {
            Err(BatchOrchestrationError::InvalidResponse)
        }
        Ok(identity) => Ok(identity),
        Err(AgentPortError::Public(error)) => {
            Err(BatchOrchestrationError::IdentityResolution(error))
        }
        Err(AgentPortError::Cancelled) => Err(BatchOrchestrationError::Cancelled),
        Err(AgentPortError::DeadlineExceeded) => Err(BatchOrchestrationError::DeadlineExceeded),
        Err(AgentPortError::LocalDeadlineExceeded) => Err(BatchOrchestrationError::InvalidResponse),
        Err(AgentPortError::InvalidResponse) => Err(BatchOrchestrationError::InvalidResponse),
        Err(AgentPortError::Unavailable) => Err(BatchOrchestrationError::Internal),
    }
}

fn remaining_parent_budget(
    parent: &ResponseBudget,
    consumed: BudgetCharge,
    elapsed: Duration,
) -> Result<ResponseBudget, BatchOrchestrationError> {
    Ok(ResponseBudget {
        max_results: remaining_u16(parent.max_results, consumed.results)?,
        max_tokens: remaining_u16(parent.max_tokens, consumed.tokens)?,
        max_source_bytes: remaining_u32(parent.max_source_bytes, consumed.source_bytes)?,
        max_traversal_facts: remaining_u32(parent.max_traversal_facts, consumed.traversal_facts)?,
        // Depth is a maximum over children, so each child retains the admitted
        // ceiling rather than receiving an additive remainder.
        max_depth: parent.max_depth,
        max_paths: remaining_u16(parent.max_paths, consumed.paths)?,
        timeout_ms: remaining_u32(
            parent.timeout_ms,
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
        )?,
        evidence_level: parent.evidence_level,
    })
}

fn remaining_u16(
    limit: Option<u16>,
    consumed: u64,
) -> Result<Option<u16>, BatchOrchestrationError> {
    limit
        .map(|limit| {
            let remaining = u64::from(limit).saturating_sub(consumed);
            u16::try_from(remaining)
                .ok()
                .filter(|remaining| *remaining > 0)
                .ok_or(BatchOrchestrationError::BudgetExceeded)
        })
        .transpose()
}

fn remaining_u32(
    limit: Option<u32>,
    consumed: u64,
) -> Result<Option<u32>, BatchOrchestrationError> {
    limit
        .map(|limit| {
            let remaining = u64::from(limit).saturating_sub(consumed);
            u32::try_from(remaining)
                .ok()
                .filter(|remaining| *remaining > 0)
                .ok_or(BatchOrchestrationError::BudgetExceeded)
        })
        .transpose()
}

fn response_budget_from_limits(
    limits: BudgetLimits,
    evidence_level: Option<rootlight_mcp_contract::vertical::ProvenanceLevel>,
) -> ResponseBudget {
    let maximums = limits.maximums();
    ResponseBudget {
        max_results: Some(u16::try_from(maximums.results).unwrap_or(u16::MAX)),
        max_tokens: Some(u16::try_from(maximums.tokens).unwrap_or(u16::MAX)),
        max_source_bytes: Some(u32::try_from(maximums.source_bytes).unwrap_or(u32::MAX)),
        max_traversal_facts: Some(u32::try_from(maximums.traversal_facts).unwrap_or(u32::MAX)),
        max_depth: Some(u8::try_from(maximums.depth).unwrap_or(u8::MAX)),
        max_paths: Some(u16::try_from(maximums.paths).unwrap_or(u16::MAX)),
        timeout_ms: Some(u32::try_from(maximums.time_ms).unwrap_or(u32::MAX)),
        evidence_level,
    }
}

fn consume_allocation(mut allocation: BudgetAllocation<'_>) -> Result<(), BatchOrchestrationError> {
    let admitted = allocation.limits().maximums();
    allocation
        .ledger_mut()
        .charge(admitted)
        .map_err(map_policy_error)?;
    allocation.commit().map_err(map_policy_error)?;
    Ok(())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeadlineSource {
    Parent,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EffectiveDeadline {
    at: Instant,
    source: DeadlineSource,
}

fn effective_child_deadline(
    parent: Instant,
    local_timeout_ms: Option<u32>,
    started_at: Instant,
) -> Result<EffectiveDeadline, BatchOrchestrationError> {
    let Some(local) = deadline_from(started_at, local_timeout_ms)? else {
        return Ok(EffectiveDeadline {
            at: parent,
            source: DeadlineSource::Parent,
        });
    };
    if local < parent {
        Ok(EffectiveDeadline {
            at: local,
            source: DeadlineSource::Local,
        })
    } else {
        Ok(EffectiveDeadline {
            at: parent,
            source: DeadlineSource::Parent,
        })
    }
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

fn charge_for(
    tool: BatchTool,
    envelope: &ReadEnvelope<Value>,
) -> Result<BudgetCharge, BatchOrchestrationError> {
    let usage = &envelope.usage;
    Ok(BudgetCharge {
        rows: usage.rows,
        results: returned_result_count(tool, &envelope.data)?,
        tokens: usage.estimated_tokens,
        // The public envelope currently exposes only its deterministic estimate.
        actual_tokens: 0,
        source_bytes: usage.source_bytes,
        traversal_facts: usage.edges,
        depth: returned_depth(tool, &envelope.data),
        paths: returned_path_count(tool, &envelope.data),
        json_bytes: usage.json_bytes,
        // Owned lower-layer response memory is not present in UsageSummary.
        memory_bytes: 0,
        time_ms: usage.wall_time_ms,
    })
}

fn returned_result_count(tool: BatchTool, data: &Value) -> Result<u64, BatchOrchestrationError> {
    if tool == BatchTool::SymbolRelationships {
        return data
            .pointer("/totals/returned_edges")
            .and_then(Value::as_u64)
            .ok_or(BatchOrchestrationError::InvalidResponse);
    }
    let field = match tool {
        BatchTool::CodeLocate => "matches",
        BatchTool::SymbolExplain => "symbols",
        BatchTool::SymbolRelationships => unreachable!("handled above"),
        BatchTool::FlowTrace => "paths",
        BatchTool::ChangeImpact => "impacted",
        BatchTool::TestsSelect => "tests",
        BatchTool::ArchitectureOverview => "components",
        BatchTool::ArchitectureCycles => "cycles",
        BatchTool::CodeDead => "candidates",
        BatchTool::ContextPack => "items",
        BatchTool::SourceRead => "chunks",
        BatchTool::PlanChange => return Ok(0),
    };
    Ok(data
        .get(field)
        .and_then(Value::as_array)
        .map_or(0, |items| u64::try_from(items.len()).unwrap_or(u64::MAX)))
}

fn returned_path_count(tool: BatchTool, data: &Value) -> u64 {
    if tool != BatchTool::FlowTrace {
        return 0;
    }
    data.get("paths")
        .and_then(Value::as_array)
        .map_or(0, |paths| u64::try_from(paths.len()).unwrap_or(u64::MAX))
}

fn returned_depth(tool: BatchTool, data: &Value) -> u64 {
    if tool != BatchTool::FlowTrace {
        return 0;
    }
    data.get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|path| path.get("nodes").and_then(Value::as_array))
        .map(|nodes| u64::try_from(nodes.len().saturating_sub(1)).unwrap_or(u64::MAX))
        .max()
        .unwrap_or(0)
}

fn validate_child_identity(
    envelope: &ReadEnvelope<Value>,
    identity: &AgentResolvedIdentity,
) -> Result<(), BatchOrchestrationError> {
    if envelope.repository.repository_id != identity.repository.repository_id {
        return Err(BatchOrchestrationError::InvalidResponse);
    }
    if envelope.generation.generation_id != identity.generation.generation_id {
        return Err(BatchOrchestrationError::InvalidResponse);
    }
    let continuation_available =
        envelope.completeness.continuation == ContinuationAvailability::Available;
    let resource_truncated = envelope.completeness.state == CompletenessState::Truncated
        || envelope
            .completeness
            .limiting_resources
            .iter()
            .any(|resource| {
                !matches!(
                    resource.kind,
                    LimitingResourceKind::Capability | LimitingResourceKind::Coverage
                )
            });
    if continuation_available != envelope.next_cursor.0.is_some()
        || resource_truncated != envelope.truncated
    {
        return Err(BatchOrchestrationError::InvalidResponse);
    }
    Ok(())
}

fn map_execution_error(error: BatchExecutionError) -> BatchOrchestrationError {
    match error {
        BatchExecutionError::InvalidOperationId
        | BatchExecutionError::DuplicateOperationId
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
        BatchExecutionError, BatchPlan, BatchValidationError, DEFAULT_BATCH_TOKENS, DeadlineSource,
        MAX_BATCH_DEPTH, MAX_BATCH_OPERATIONS, admitted_parent_budget, aggregate_status,
        effective_child_deadline, is_batch_allowed, is_batch_allowed_under_profile,
        resolve_dependencies, terminal_result,
    };
    use crate::policy::BudgetLimits;
    use rootlight_mcp_contract::{
        ExposureProfile, McpTool,
        context::{
            BatchOperation as ContractBatchOperation, BatchOperationStatus, BatchStatus, BatchTool,
        },
        vertical::ResponseBudget,
    };
    use serde_json::Map;
    use std::time::{Duration, Instant};

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

    #[test]
    fn deadline_source_comes_from_the_same_absolute_minimum() {
        let started_at = Instant::now();
        let parent = started_at + Duration::from_millis(100);
        let equal = effective_child_deadline(parent, Some(100), started_at)
            .expect("equal deadlines are representable");
        assert_eq!(equal.at, parent);
        assert_eq!(equal.source, DeadlineSource::Parent);

        let tighter = effective_child_deadline(parent, Some(99), started_at)
            .expect("local deadline is representable");
        assert_eq!(tighter.at, started_at + Duration::from_millis(99));
        assert_eq!(tighter.source, DeadlineSource::Local);
    }

    #[test]
    fn admitted_batch_defaults_are_complete_server_bounded_limits() {
        let admitted = admitted_parent_budget(None);
        let ceiling = BudgetLimits::server_ceiling().maximums();

        assert_eq!(admitted.max_results, u16::try_from(ceiling.results).ok());
        assert_eq!(admitted.max_tokens, Some(DEFAULT_BATCH_TOKENS));
        assert_eq!(
            admitted.max_source_bytes,
            u32::try_from(ceiling.source_bytes).ok()
        );
        assert_eq!(
            admitted.max_traversal_facts,
            u32::try_from(ceiling.traversal_facts).ok()
        );
        assert_eq!(admitted.max_depth, u8::try_from(ceiling.depth).ok());
        assert_eq!(admitted.max_paths, u16::try_from(ceiling.paths).ok());
        assert_eq!(admitted.timeout_ms, u32::try_from(ceiling.time_ms).ok());
        assert_eq!(admitted.evidence_level, None);

        let attempted_raise = admitted_parent_budget(Some(&ResponseBudget {
            max_results: Some(1_000),
            max_tokens: Some(16_000),
            max_source_bytes: Some(524_288),
            max_traversal_facts: Some(100_000),
            max_depth: Some(16),
            max_paths: Some(1_000),
            timeout_ms: Some(30_000),
            evidence_level: None,
        }));
        assert_eq!(
            attempted_raise.timeout_ms,
            u32::try_from(ceiling.time_ms).ok()
        );
    }
}
