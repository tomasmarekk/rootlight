//! Bounded batch query planning and response shaping.
//!
//! This module owns the transport-neutral dependency plan, child dispatch
//! schedule, typed binding resolution, shared budget and deadline policy,
//! deterministic request-order result shaping, and aggregate usage accounting.
//! The composing application implements [`crate::port::AgentToolPort`] to map
//! admitted child calls onto its concrete client.

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId, SymbolId};
use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_mcp_contract::{
    ExposureProfile, McpTool, PublicError, SchemaVersion, TrustClassification,
    batch::{
        BatchBindingCardinality, BatchBindingPathSegment, BatchBindingSourceSlot,
        BatchBindingTargetSlot, BatchBindingValueType, BatchBudgetDimension,
        BatchResponseProfilePolicy, BatchToolDescriptor, batch_descriptor,
    },
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
        LimitingResourceKind, ResultCompleteness,
    },
    context::{
        BatchBinding, BatchOperation as ContractBatchOperation, BatchOperationResult,
        BatchOperationStatus, BatchStatus, BatchTool, FailurePolicy, QueryBatchData,
        QueryBatchInput,
    },
    vertical::{
        CacheStatus, GenerationSelector, ReadEnvelope, RepositoryIdSelector, RequiredNullable,
        ResponseBudget, ResponseProfile, UsageSummary,
    },
};
use serde_json::{Map, Value};

use crate::{
    policy::{
        BudgetAllocation, BudgetCharge, BudgetLedger, BudgetLimits, BudgetResource,
        CancellationSignal, ExecutionPolicyError,
    },
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentResolutionContext,
        AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
    response_profile::{BatchProfileProjectionError, shape_batch_child_data},
};

/// Maximum operations accepted in one public batch request.
pub const MAX_BATCH_OPERATIONS: usize = 16;

/// Maximum dependency depth in the batch operation DAG.
pub const MAX_BATCH_DEPTH: usize = 8;

/// Maximum dependencies one operation may declare.
pub const MAX_DEPS_PER_OPERATION: usize = 8;

/// Fixed-point counters can grow by only a few digits after the skeleton is
/// measured; this bound also covers the untagged success wrapper.
const PUBLICATION_COUNTER_TOLERANCE_BYTES: u64 = 128;

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
    /// A successful dependency omitted an optional typed slot selected by the binding.
    #[error("batch binding selected a missing optional value")]
    MissingBindingValue,
    /// A dependency returned a value outside its typed slot contract.
    #[error("batch binding value has the wrong type")]
    BindingTypeMismatch,
    /// A dependency returned a collection outside the target slot bounds.
    #[error("batch binding collection violates its cardinality contract")]
    BindingCardinalityMismatch,
    /// A generation-pinned source reference does not match the batch identity.
    #[error("batch binding source identity does not match the pinned batch")]
    BindingIdentityMismatch,
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

/// A typed argument template compiled before any repository or child call.
///
/// Object keys and array positions remain structural nodes while dependency
/// references become reviewed typed binding edges. Runtime materialization
/// therefore never has to reinterpret repository-controlled JSON as template
/// syntax.
#[derive(Debug, Clone, PartialEq)]
pub enum ArgumentTemplate {
    /// A scalar JSON value copied unchanged into the child request.
    Literal(Value),
    /// A recursively compiled JSON object.
    Object(BTreeMap<String, ArgumentTemplate>),
    /// A recursively compiled JSON array.
    Array(Vec<ArgumentTemplate>),
    /// One registry-reviewed dependency-output binding.
    Binding(PlannedBinding),
}

/// One statically validated typed binding edge in a batch plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBinding {
    source_operation: usize,
    source_slot: &'static BatchBindingSourceSlot,
    source_indices: Vec<usize>,
    target_slot: &'static BatchBindingTargetSlot,
    destination: String,
}

/// One immutable operation consumed by the batch scheduler.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedBatchOperation {
    id: String,
    request_index: usize,
    tool: BatchTool,
    dependencies: Vec<usize>,
    depth: usize,
    arguments: ArgumentTemplate,
    local_budget: Option<ResponseBudget>,
    effective_budget: ResponseBudget,
    descriptor: &'static BatchToolDescriptor,
}

/// Complete deterministic batch plan admitted before identity resolution.
///
/// The selected repository and generation are resolved only after this plan is
/// accepted. The resulting pinned identity is passed to every materialization
/// and child call without changing the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct StaticBatchPlan {
    repository: rootlight_mcp_contract::RepositorySelector,
    generation: Option<GenerationSelector>,
    operations: Vec<PlannedBatchOperation>,
    execution_order: Vec<usize>,
    max_depth: usize,
    failure_policy: FailurePolicy,
    parent_budget: ResponseBudget,
    response_profile: ResponseProfile,
    explain: bool,
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

impl StaticBatchPlan {
    /// Compiles and admits every deterministic part of a batch request.
    ///
    /// This function performs no repository or child-tool calls. A successful
    /// return is the only input accepted by [`BatchService::execute_plan`].
    ///
    /// # Errors
    ///
    /// Returns a checked orchestration error when the DAG, templates, profile,
    /// binding edges, or statically knowable budget requirements are invalid.
    pub fn build(
        input: QueryBatchInput,
        exposure_profile: ExposureProfile,
    ) -> Result<Self, BatchOrchestrationError> {
        let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
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
            })
        }) {
            return Err(BatchOrchestrationError::InvalidArguments);
        }

        let mut dependencies =
            resolve_dependencies(&input.operations).map_err(map_execution_error)?;
        for declared in &mut dependencies {
            declared.sort_unstable();
            declared.dedup();
        }
        let tools: Vec<McpTool> = input
            .operations
            .iter()
            .map(|operation| mcp_tool_for_batch(operation.tool))
            .collect();
        let dag = BatchPlan::validate(&tools, &dependencies)
            .map_err(|_| BatchOrchestrationError::InvalidArguments)?;
        if tools
            .iter()
            .any(|tool| !is_batch_allowed_under_profile(*tool, exposure_profile))
        {
            return Err(BatchOrchestrationError::InvalidArguments);
        }

        let parent_budget = admitted_parent_budget(input.budget.as_ref());
        if parent_budget.evidence_level.is_some() {
            return Err(BatchOrchestrationError::InvalidArguments);
        }

        let mut depths = vec![0usize; input.operations.len()];
        for index in &dag.execution_order {
            depths[*index] = dependencies[*index]
                .iter()
                .map(|dependency| depths[*dependency] + 1)
                .max()
                .unwrap_or(0);
        }

        let mut operations = Vec::new();
        operations
            .try_reserve_exact(input.operations.len())
            .map_err(|_| BatchOrchestrationError::Internal)?;
        let mut required_tokens = 0_u64;
        for (index, operation) in input.operations.iter().enumerate() {
            let descriptor = batch_descriptor(operation.tool);
            if !batch_profile_is_supported(descriptor.response_profiles, response_profile) {
                return Err(BatchOrchestrationError::UnsupportedProfile);
            }
            validate_local_budget(descriptor, operation.local_budget.as_ref())?;
            let effective_budget =
                effective_static_budget(&parent_budget, operation.local_budget.as_ref());
            let minimum_tokens = minimum_static_tokens(operation.tool);
            if effective_budget
                .max_tokens
                .is_some_and(|tokens| u64::from(tokens) < minimum_tokens)
            {
                return Err(BatchOrchestrationError::BudgetExceeded);
            }
            required_tokens = required_tokens.saturating_add(minimum_tokens);

            let mut arguments = BTreeMap::new();
            for (key, value) in &operation.arguments {
                let destination = append_pointer("", key).map_err(map_execution_error)?;
                arguments.insert(
                    key.clone(),
                    compile_argument_template(
                        value,
                        operation.tool,
                        &destination,
                        &input.operations,
                        &dependencies[index],
                    )
                    .map_err(map_execution_error)?,
                );
            }
            operations.push(PlannedBatchOperation {
                id: operation.id.clone(),
                request_index: index,
                tool: operation.tool,
                dependencies: dependencies[index].clone(),
                depth: depths[index],
                arguments: ArgumentTemplate::Object(arguments),
                local_budget: operation.local_budget.clone(),
                effective_budget,
                descriptor,
            });
        }
        if parent_budget
            .max_tokens
            .is_some_and(|tokens| required_tokens > u64::from(tokens))
        {
            return Err(BatchOrchestrationError::BudgetExceeded);
        }

        let max_depth = dag.max_depth();
        Ok(Self {
            repository: input.repository,
            generation: input.generation,
            operations,
            execution_order: dag.execution_order,
            max_depth,
            failure_policy: input
                .failure_policy
                .unwrap_or(FailurePolicy::ContinueIndependent),
            parent_budget,
            response_profile,
            explain: input.explain.unwrap_or(false),
        })
    }

    /// Returns the selected repository before identity pinning.
    #[must_use]
    pub const fn repository(&self) -> &rootlight_mcp_contract::RepositorySelector {
        &self.repository
    }

    /// Returns the normalized generation selector.
    #[must_use]
    pub const fn generation(&self) -> Option<&GenerationSelector> {
        self.generation.as_ref()
    }

    /// Returns operations in public request order.
    #[must_use]
    pub fn operations(&self) -> &[PlannedBatchOperation] {
        &self.operations
    }

    /// Returns the normalized admitted parent budget.
    #[must_use]
    pub const fn parent_budget(&self) -> &ResponseBudget {
        &self.parent_budget
    }

    /// Returns the representation inherited by every selectable child.
    #[must_use]
    pub const fn response_profile(&self) -> ResponseProfile {
        self.response_profile
    }

    /// Returns whether the caller requested source-free explain mode.
    #[must_use]
    pub const fn explain(&self) -> bool {
        self.explain
    }

    /// Returns the maximum admitted dependency depth.
    #[must_use]
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Computes the request-sensitive canonical plan digest.
    ///
    /// Repository and resolved generation identity are intentionally added by
    /// explain finalization. This digest covers every normalized deterministic
    /// request field, registry selection, budget, dependency, and template.
    #[must_use]
    pub fn canonical_digest(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"rootlight.static-batch-plan.v1");
        hasher.update(&[failure_policy_tag(self.failure_policy)]);
        hasher.update(&[response_profile_tag(self.response_profile)]);
        hash_budget(&mut hasher, &self.parent_budget);
        hash_usize(&mut hasher, self.max_depth);
        hash_usize(&mut hasher, self.operations.len());
        for operation in &self.operations {
            operation.hash_canonical(&mut hasher);
        }
        *hasher.finalize().as_bytes()
    }

    /// Returns a stable lowercase encoding of [`Self::canonical_digest`].
    #[must_use]
    pub fn canonical_digest_hex(&self) -> String {
        blake3::Hash::from_bytes(self.canonical_digest())
            .to_hex()
            .to_string()
    }
}

impl PlannedBatchOperation {
    /// Returns the canonical request-scoped operation ID.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the public request position.
    #[must_use]
    pub const fn request_index(&self) -> usize {
        self.request_index
    }

    /// Returns the selected batch tool.
    #[must_use]
    pub const fn tool(&self) -> BatchTool {
        self.tool
    }

    /// Returns the dependency indices in request order.
    #[must_use]
    pub fn dependencies(&self) -> &[usize] {
        &self.dependencies
    }

    /// Returns the statically computed DAG depth.
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns the selected canonical tool descriptor.
    #[must_use]
    pub const fn descriptor(&self) -> &'static BatchToolDescriptor {
        self.descriptor
    }

    /// Returns the normalized initial effective child budget.
    #[must_use]
    pub const fn effective_budget(&self) -> &ResponseBudget {
        &self.effective_budget
    }

    /// Returns the caller-provided child-local cap after static admission.
    #[must_use]
    pub const fn local_budget(&self) -> Option<&ResponseBudget> {
        self.local_budget.as_ref()
    }

    /// Materializes schema-valid typed witnesses for static child validation.
    ///
    /// Witnesses are never dispatched. Real dependency values are independently
    /// validated again by the standalone child validator before execution.
    ///
    /// # Errors
    ///
    /// Returns an internal error if a built-in witness cannot be represented.
    pub fn witness_arguments(&self) -> Result<Map<String, Value>, BatchOrchestrationError> {
        let Value::Object(arguments) = witness_template(&self.arguments)? else {
            return Err(BatchOrchestrationError::Internal);
        };
        Ok(arguments)
    }

    fn materialize_arguments(
        &self,
        envelopes: &[Option<ReadEnvelope<Value>>],
        expected_repository: RepositoryId,
        expected_generation: GenerationId,
    ) -> Result<ResolvedBatchArguments, BatchExecutionError> {
        let mut binding_paths = Vec::new();
        let Value::Object(arguments) = materialize_template(
            &self.arguments,
            envelopes,
            expected_repository,
            expected_generation,
            &mut binding_paths,
        )?
        else {
            return Err(BatchExecutionError::Serialization);
        };
        Ok(ResolvedBatchArguments {
            arguments,
            materialized_binding_paths: binding_paths,
        })
    }

    fn hash_canonical(&self, hasher: &mut blake3::Hasher) {
        hash_length_prefixed(hasher, self.id.as_bytes());
        hash_usize(hasher, self.request_index);
        hash_length_prefixed(hasher, self.tool.name().as_bytes());
        hash_length_prefixed(hasher, self.descriptor.contract_version.as_bytes());
        hash_length_prefixed(hasher, self.descriptor.adapter.name().as_bytes());
        hash_usize(hasher, self.depth);
        hash_usize(hasher, self.dependencies.len());
        for dependency in &self.dependencies {
            hash_usize(hasher, *dependency);
        }
        hash_budget(hasher, &self.effective_budget);
        hash_template(hasher, &self.arguments);
    }
}

fn compile_argument_template(
    value: &Value,
    target_tool: BatchTool,
    destination: &str,
    operations: &[ContractBatchOperation],
    declared: &[usize],
) -> Result<ArgumentTemplate, BatchExecutionError> {
    match value {
        Value::Object(map) => {
            if let Some(binding) = binding_reference(map)? {
                let source_operation = declared
                    .iter()
                    .find(|&&index| operations[index].id == binding.from)
                    .copied()
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let source = translate_source_binding(
                    operations[source_operation].tool,
                    binding.source,
                    binding.index,
                )?;
                let target = translate_target_binding(target_tool, destination)?;
                validate_binding_pair(source.slot, target)?;
                Ok(ArgumentTemplate::Binding(PlannedBinding {
                    source_operation,
                    source_slot: source.slot,
                    source_indices: source.indices,
                    target_slot: target,
                    destination: destination.to_owned(),
                }))
            } else {
                let mut object = BTreeMap::new();
                for (key, inner) in map {
                    let child_destination = append_pointer(destination, key)?;
                    object.insert(
                        key.clone(),
                        compile_argument_template(
                            inner,
                            target_tool,
                            &child_destination,
                            operations,
                            declared,
                        )?,
                    );
                }
                Ok(ArgumentTemplate::Object(object))
            }
        }
        Value::Array(items) => {
            let mut array = Vec::new();
            array
                .try_reserve_exact(items.len())
                .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
            for (index, inner) in items.iter().enumerate() {
                let child_destination = append_pointer(destination, &index.to_string())?;
                array.push(compile_argument_template(
                    inner,
                    target_tool,
                    &child_destination,
                    operations,
                    declared,
                )?);
            }
            Ok(ArgumentTemplate::Array(array))
        }
        scalar => Ok(ArgumentTemplate::Literal(scalar.clone())),
    }
}

fn validate_local_budget(
    descriptor: &BatchToolDescriptor,
    local: Option<&ResponseBudget>,
) -> Result<(), BatchOrchestrationError> {
    let Some(local) = local else {
        return Ok(());
    };
    if local.evidence_level.is_some() {
        return Err(BatchOrchestrationError::InvalidArguments);
    }
    for (present, dimension) in [
        (local.max_results.is_some(), BatchBudgetDimension::Results),
        (local.max_tokens.is_some(), BatchBudgetDimension::Tokens),
        (
            local.max_source_bytes.is_some(),
            BatchBudgetDimension::SourceBytes,
        ),
        (
            local.max_traversal_facts.is_some(),
            BatchBudgetDimension::TraversalFacts,
        ),
        (local.max_depth.is_some(), BatchBudgetDimension::Depth),
        (local.max_paths.is_some(), BatchBudgetDimension::Paths),
        (local.timeout_ms.is_some(), BatchBudgetDimension::Timeout),
    ] {
        if present && !descriptor.budget.locally_reducible.contains(&dimension) {
            return Err(BatchOrchestrationError::InvalidArguments);
        }
    }
    Ok(())
}

const fn batch_profile_is_supported(
    policy: BatchResponseProfilePolicy,
    requested: ResponseProfile,
) -> bool {
    match policy {
        BatchResponseProfilePolicy::Fixed(fixed) => fixed as u8 == requested as u8,
        BatchResponseProfilePolicy::Selectable { supported, .. } => {
            let mut index = 0;
            while index < supported.len() {
                if supported[index] as u8 == requested as u8 {
                    return true;
                }
                index += 1;
            }
            false
        }
    }
}

fn effective_static_budget(
    parent: &ResponseBudget,
    local: Option<&ResponseBudget>,
) -> ResponseBudget {
    let local = local.cloned().unwrap_or(ResponseBudget {
        max_results: None,
        max_tokens: None,
        max_source_bytes: None,
        max_traversal_facts: None,
        max_depth: None,
        max_paths: None,
        timeout_ms: None,
        evidence_level: None,
    });
    ResponseBudget {
        max_results: min_optional(parent.max_results, local.max_results),
        max_tokens: min_optional(parent.max_tokens, local.max_tokens),
        max_source_bytes: min_optional(parent.max_source_bytes, local.max_source_bytes),
        max_traversal_facts: min_optional(parent.max_traversal_facts, local.max_traversal_facts),
        max_depth: min_optional(parent.max_depth, local.max_depth),
        max_paths: min_optional(parent.max_paths, local.max_paths),
        timeout_ms: min_optional(parent.timeout_ms, local.timeout_ms),
        evidence_level: None,
    }
}

fn min_optional<T: Ord + Copy>(parent: Option<T>, local: Option<T>) -> Option<T> {
    match (parent, local) {
        (Some(parent), Some(local)) => Some(parent.min(local)),
        (parent, None) => parent,
        (None, local) => local,
    }
}

const fn minimum_static_tokens(tool: BatchTool) -> u64 {
    match tool {
        BatchTool::ContextPack => 500,
        BatchTool::CodeLocate
        | BatchTool::SymbolExplain
        | BatchTool::SymbolRelationships
        | BatchTool::FlowTrace
        | BatchTool::ChangeImpact
        | BatchTool::TestsSelect
        | BatchTool::ArchitectureOverview
        | BatchTool::ArchitectureCycles
        | BatchTool::CodeDead
        | BatchTool::PlanChange
        | BatchTool::SourceRead => 0,
    }
}

fn witness_template(template: &ArgumentTemplate) -> Result<Value, BatchOrchestrationError> {
    match template {
        ArgumentTemplate::Literal(value) => Ok(value.clone()),
        ArgumentTemplate::Object(fields) => {
            let mut object = Map::new();
            for (key, value) in fields {
                object.insert(key.clone(), witness_template(value)?);
            }
            Ok(Value::Object(object))
        }
        ArgumentTemplate::Array(items) => items
            .iter()
            .map(witness_template)
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        ArgumentTemplate::Binding(binding) => binding_witness(binding),
    }
}

fn materialize_template(
    template: &ArgumentTemplate,
    envelopes: &[Option<ReadEnvelope<Value>>],
    expected_repository: RepositoryId,
    expected_generation: GenerationId,
    binding_paths: &mut Vec<String>,
) -> Result<Value, BatchExecutionError> {
    match template {
        ArgumentTemplate::Literal(value) => Ok(value.clone()),
        ArgumentTemplate::Object(fields) => {
            let mut object = Map::new();
            for (key, value) in fields {
                object.insert(
                    key.clone(),
                    materialize_template(
                        value,
                        envelopes,
                        expected_repository,
                        expected_generation,
                        binding_paths,
                    )?,
                );
            }
            Ok(Value::Object(object))
        }
        ArgumentTemplate::Array(items) => {
            let mut array = Vec::new();
            array
                .try_reserve_exact(items.len())
                .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
            for value in items {
                array.push(materialize_template(
                    value,
                    envelopes,
                    expected_repository,
                    expected_generation,
                    binding_paths,
                )?);
            }
            Ok(Value::Array(array))
        }
        ArgumentTemplate::Binding(binding) => {
            let envelope = envelopes
                .get(binding.source_operation)
                .and_then(Option::as_ref)
                .ok_or(BatchExecutionError::InvalidBinding)?;
            if envelope.trust != TrustClassification::UntrustedRepositoryData {
                return Err(BatchExecutionError::InvalidBinding);
            }
            let resolved =
                extract_typed_source(&envelope.data, binding.source_slot, &binding.source_indices)?
                    .clone();
            validate_runtime_binding(
                &resolved,
                binding.source_slot,
                binding.target_slot,
                expected_repository,
                expected_generation,
            )?;
            binding_paths
                .try_reserve(1)
                .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
            binding_paths.push(binding.destination.clone());
            Ok(resolved)
        }
    }
}

fn binding_witness(binding: &PlannedBinding) -> Result<Value, BatchOrchestrationError> {
    let seed = u8::try_from(binding.destination.len() % 251 + 1).unwrap_or(1);
    let symbol = || SymbolId::from_bytes([seed; 20]);
    let source_ref = || {
        let span = SourceSpan::new(FileId::from_bytes([seed; 20]), 0, 1)
            .map_err(|_| BatchOrchestrationError::Internal)?;
        Ok::<SourceRef, BatchOrchestrationError>(SourceRef::new(
            RepositoryId::from_bytes([seed; 16]),
            GenerationId::from_bytes([seed; 20]),
            span,
            ContentHash::from_bytes([seed; 32]),
            None,
        ))
    };
    let collection_length = match binding.target_slot.cardinality {
        BatchBindingCardinality::Scalar => 1,
        BatchBindingCardinality::Collection { min, max } => usize::from(min.max(1).min(max)),
    };
    let value = match binding.target_slot.value_type {
        BatchBindingValueType::SymbolId => serde_json::to_value(symbol()),
        BatchBindingValueType::SymbolIds => serde_json::to_value(vec![symbol(); collection_length]),
        BatchBindingValueType::SourceRef => serde_json::to_value(source_ref()?),
        BatchBindingValueType::SourceRefs => {
            serde_json::to_value(vec![source_ref()?; collection_length])
        }
        BatchBindingValueType::TestId => serde_json::to_value(format!("test_{seed}")),
        BatchBindingValueType::PackId => serde_json::to_value(format!("pack_{seed}")),
    };
    value.map_err(|_| BatchOrchestrationError::Internal)
}

fn hash_template(hasher: &mut blake3::Hasher, template: &ArgumentTemplate) {
    match template {
        ArgumentTemplate::Literal(value) => {
            hasher.update(&[0]);
            let encoded = serde_json::to_vec(value).unwrap_or_default();
            hash_length_prefixed(hasher, &encoded);
        }
        ArgumentTemplate::Object(fields) => {
            hasher.update(&[1]);
            hash_usize(hasher, fields.len());
            for (key, value) in fields {
                hash_length_prefixed(hasher, key.as_bytes());
                hash_template(hasher, value);
            }
        }
        ArgumentTemplate::Array(items) => {
            hasher.update(&[2]);
            hash_usize(hasher, items.len());
            for value in items {
                hash_template(hasher, value);
            }
        }
        ArgumentTemplate::Binding(binding) => {
            hasher.update(&[3]);
            hash_usize(hasher, binding.source_operation);
            hash_length_prefixed(hasher, binding.destination.as_bytes());
            hash_usize(hasher, binding.source_indices.len());
            for index in &binding.source_indices {
                hash_usize(hasher, *index);
            }
            hasher.update(&[binding_value_type_tag(binding.source_slot.value_type)]);
            hash_cardinality(hasher, binding.source_slot.cardinality);
            hash_cardinality(hasher, binding.target_slot.cardinality);
            hasher.update(&[binding_trust_tag(binding.source_slot.trust)]);
        }
    }
}

fn hash_budget(hasher: &mut blake3::Hasher, budget: &ResponseBudget) {
    hash_option(hasher, budget.max_results.map(u64::from));
    hash_option(hasher, budget.max_tokens.map(u64::from));
    hash_option(hasher, budget.max_source_bytes.map(u64::from));
    hash_option(hasher, budget.max_traversal_facts.map(u64::from));
    hash_option(hasher, budget.max_depth.map(u64::from));
    hash_option(hasher, budget.max_paths.map(u64::from));
    hash_option(hasher, budget.timeout_ms.map(u64::from));
}

fn hash_option(hasher: &mut blake3::Hasher, value: Option<u64>) {
    match value {
        Some(value) => {
            hasher.update(&[1]);
            hasher.update(&value.to_le_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
}

fn hash_usize(hasher: &mut blake3::Hasher, value: usize) {
    hasher.update(&u64::try_from(value).unwrap_or(u64::MAX).to_le_bytes());
}

fn hash_length_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hash_usize(hasher, value.len());
    hasher.update(value);
}

const fn failure_policy_tag(policy: FailurePolicy) -> u8 {
    match policy {
        FailurePolicy::ContinueIndependent => 0,
        FailurePolicy::FailFast => 1,
    }
}

const fn response_profile_tag(profile: ResponseProfile) -> u8 {
    match profile {
        ResponseProfile::Compact => 0,
        ResponseProfile::Standard => 1,
        ResponseProfile::Evidence => 2,
    }
}

const fn binding_value_type_tag(value_type: BatchBindingValueType) -> u8 {
    match value_type {
        BatchBindingValueType::SymbolId => 0,
        BatchBindingValueType::SymbolIds => 1,
        BatchBindingValueType::SourceRef => 2,
        BatchBindingValueType::SourceRefs => 3,
        BatchBindingValueType::TestId => 4,
        BatchBindingValueType::PackId => 5,
    }
}

fn hash_cardinality(hasher: &mut blake3::Hasher, cardinality: BatchBindingCardinality) {
    match cardinality {
        BatchBindingCardinality::Scalar => {
            hasher.update(&[0]);
        }
        BatchBindingCardinality::Collection { min, max } => {
            hasher.update(&[1]);
            hasher.update(&min.to_le_bytes());
            hasher.update(&max.to_le_bytes());
        }
    }
}

const fn binding_trust_tag(trust: rootlight_mcp_contract::batch::BatchBindingTrust) -> u8 {
    use rootlight_mcp_contract::batch::BatchBindingTrust;
    match trust {
        BatchBindingTrust::TypedIdentifier => 0,
        BatchBindingTrust::GenerationPinnedReference => 1,
        BatchBindingTrust::OpaqueRootlightIdentifier => 2,
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
                    | BatchOperationStatus::NotRunBudget
                    | BatchOperationStatus::Cancelled
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
    expected_repository: RepositoryId,
    expected_generation: GenerationId,
) -> Result<ResolvedBatchArguments, BatchExecutionError> {
    let mut arguments = Map::new();
    let mut materialized_binding_paths = Vec::new();
    {
        let mut resolver = BindingResolver {
            envelopes,
            operations: &input.operations,
            declared,
            target_tool: operation.tool,
            materialized_binding_paths: &mut materialized_binding_paths,
            expected_repository,
            expected_generation,
        };
        for (key, value) in &operation.arguments {
            let destination = append_pointer("", key)?;
            arguments.insert(key.clone(), resolver.resolve(value, &destination)?);
        }
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

fn planned_success_result(
    operation: &PlannedBatchOperation,
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

fn planned_error_result(
    operation: &PlannedBatchOperation,
    error: &PublicError,
) -> BatchOperationResult {
    planned_error_result_with_usage(operation, error, None)
}

fn planned_error_result_with_usage(
    operation: &PlannedBatchOperation,
    error: &PublicError,
    usage: Option<UsageSummary>,
) -> BatchOperationResult {
    BatchOperationResult {
        id: operation.id.clone(),
        tool: operation.tool,
        status: BatchOperationStatus::Error,
        data: None,
        error: Some(error.clone()),
        truncated: false,
        next_cursor: RequiredNullable(None),
        usage,
        warnings: Vec::new(),
    }
}

fn planned_terminal_result(
    operation: &PlannedBatchOperation,
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
    aggregate_usage_with_receipts(envelopes, &[])
}

fn aggregate_usage_with_receipts(
    envelopes: &[Option<ReadEnvelope<Value>>],
    receipts: &[Option<UsageSummary>],
) -> UsageSummary {
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
        merge_usage(&mut usage, &envelope.usage);
    }
    for receipt in receipts.iter().flatten() {
        merge_usage(&mut usage, receipt);
    }
    usage
}

fn merge_usage(aggregate: &mut UsageSummary, usage: &UsageSummary) {
    aggregate.rows = aggregate.rows.saturating_add(usage.rows);
    aggregate.edges = aggregate.edges.saturating_add(usage.edges);
    aggregate.source_bytes = aggregate.source_bytes.saturating_add(usage.source_bytes);
    aggregate.json_bytes = aggregate.json_bytes.saturating_add(usage.json_bytes);
    aggregate.estimated_tokens = aggregate
        .estimated_tokens
        .saturating_add(usage.estimated_tokens);
    aggregate.wall_time_ms = aggregate.wall_time_ms.max(usage.wall_time_ms);
}

fn aggregate_completeness(
    envelopes: &[Option<ReadEnvelope<Value>>],
    results: &[BatchOperationResult],
    limiting_resource: Option<LimitingResourceKind>,
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
    let has_budget_nonexecution = results
        .iter()
        .any(|result| result.status == BatchOperationStatus::NotRunBudget);
    let has_cancellation = results
        .iter()
        .any(|result| result.status == BatchOperationStatus::Cancelled);
    let has_policy_nonexecution = results.iter().any(|result| {
        matches!(
            result.status,
            BatchOperationStatus::SkippedDependency | BatchOperationStatus::NotRunFailFast
        )
    });
    if results.iter().any(|result| result.truncated) || has_budget_nonexecution {
        state = state.max(CompletenessState::Truncated);
        resources.push(LimitingResource::kind(
            limiting_resource.unwrap_or(LimitingResourceKind::Results),
        ));
        guidance.push(ContinuationGuidance::IncreaseBudgetWithinLimit);
    }
    if has_cancellation {
        state = CompletenessState::Indeterminate;
        resources.push(LimitingResource::kind(LimitingResourceKind::Cancellation));
    } else if has_policy_nonexecution {
        state = state.max(CompletenessState::Indeterminate);
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

fn fill_unresolved_slots(
    operations: &[PlannedBatchOperation],
    results: &mut [Option<BatchOperationResult>],
    status: BatchOperationStatus,
) {
    for operation in operations {
        if results[operation.request_index].is_none() {
            results[operation.request_index] = Some(planned_terminal_result(operation, status));
        }
    }
}

fn finalize_result_slots(
    operations: &[PlannedBatchOperation],
    results: Vec<Option<BatchOperationResult>>,
) -> Result<Vec<BatchOperationResult>, BatchOrchestrationError> {
    if operations.len() != results.len() {
        return Err(BatchOrchestrationError::Internal);
    }
    operations
        .iter()
        .zip(results)
        .map(|(operation, result)| {
            let result = result.ok_or(BatchOrchestrationError::Internal)?;
            if result.id != operation.id || result.tool != operation.tool {
                return Err(BatchOrchestrationError::Internal);
            }
            Ok(result)
        })
        .collect()
}

fn minimum_publication_charge(
    plan: &StaticBatchPlan,
    identity: &AgentResolvedIdentity,
    error: &PublicError,
) -> Result<BudgetCharge, BatchOrchestrationError> {
    let operation_results = plan
        .operations
        .iter()
        .map(|operation| planned_error_result(operation, error))
        .collect::<Vec<_>>();
    let envelope = ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: identity.repository.clone(),
        generation: identity.generation.clone(),
        coverage: identity.coverage.clone(),
        data: QueryBatchData {
            batch_status: BatchStatus::Error,
            generation_id: identity.generation.generation_id,
            operation_results,
            explanation: None,
        },
        truncated: true,
        completeness: ResultCompleteness::new(
            CompletenessState::Truncated,
            vec![LimitingResource::kind(
                LimitingResourceKind::EstimatedTokens,
            )],
            ContinuationAvailability::Unavailable,
            vec![
                ContinuationGuidance::SplitRequest,
                ContinuationGuidance::IncreaseBudgetWithinLimit,
            ],
        )
        .map_err(|_| BatchOrchestrationError::Internal)?,
        next_cursor: RequiredNullable(None),
        usage: empty_batch_usage(),
        warnings: identity.warnings.clone(),
        trust: TrustClassification::UntrustedRepositoryData,
    };
    let bytes = serde_json::to_vec(&envelope)
        .map_err(|_| BatchOrchestrationError::Internal)?
        .len();
    let bytes = u64::try_from(bytes)
        .map_err(|_| BatchOrchestrationError::Internal)?
        .saturating_add(PUBLICATION_COUNTER_TOLERANCE_BYTES);
    let publication_len = usize::try_from(bytes).map_err(|_| BatchOrchestrationError::Internal)?;
    let estimated_tokens = rootlight_mcp_contract::accounting::estimate_tokens(publication_len);
    Ok(BudgetCharge {
        tokens: estimated_tokens,
        json_bytes: bytes,
        ..BudgetCharge::default()
    })
}

fn empty_batch_usage() -> UsageSummary {
    UsageSummary {
        rows: 0,
        edges: 0,
        source_bytes: 0,
        json_bytes: 0,
        estimated_tokens: 0,
        wall_time_ms: 0,
        cache_status: CacheStatus::Miss,
        trace_id: "batch".to_owned(),
    }
}

struct BindingResolver<'a> {
    envelopes: &'a [Option<ReadEnvelope<Value>>],
    operations: &'a [ContractBatchOperation],
    declared: &'a [usize],
    target_tool: BatchTool,
    materialized_binding_paths: &'a mut Vec<String>,
    expected_repository: RepositoryId,
    expected_generation: GenerationId,
}

impl BindingResolver<'_> {
    fn resolve(&mut self, value: &Value, destination: &str) -> Result<Value, BatchExecutionError> {
        match value {
            Value::Object(map) => {
                if let Some(binding) = binding_reference(map)? {
                    let dependency = self
                        .declared
                        .iter()
                        .find(|&&index| self.operations[index].id == binding.from)
                        .ok_or(BatchExecutionError::InvalidBinding)?;
                    let envelope = self.envelopes[*dependency]
                        .as_ref()
                        .ok_or(BatchExecutionError::InvalidBinding)?;
                    if envelope.trust != TrustClassification::UntrustedRepositoryData {
                        return Err(BatchExecutionError::InvalidBinding);
                    }
                    let source_tool = self.operations[*dependency].tool;
                    let source =
                        translate_source_binding(source_tool, binding.source, binding.index)?;
                    let target = translate_target_binding(self.target_tool, destination)?;
                    validate_binding_pair(source.slot, target)?;
                    let resolved =
                        extract_typed_source(&envelope.data, source.slot, &source.indices)?.clone();
                    validate_runtime_binding(
                        &resolved,
                        source.slot,
                        target,
                        self.expected_repository,
                        self.expected_generation,
                    )?;
                    self.materialized_binding_paths
                        .try_reserve(1)
                        .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
                    self.materialized_binding_paths.push(destination.to_owned());
                    Ok(resolved)
                } else {
                    let mut resolved = Map::new();
                    for (key, inner) in map {
                        let child_destination = append_pointer(destination, key)?;
                        resolved.insert(key.clone(), self.resolve(inner, &child_destination)?);
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
                    resolved.push(self.resolve(inner, &child_destination)?);
                }
                Ok(Value::Array(resolved))
            }
            scalar => Ok(scalar.clone()),
        }
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
) -> Result<Option<BatchBinding>, BatchExecutionError> {
    if !map.contains_key("$from") {
        return Ok(None);
    }
    let binding = serde_json::from_value::<BatchBinding>(Value::Object(map.clone()))
        .map_err(|_| BatchExecutionError::InvalidBinding)?;
    if !batch_operation_id_is_valid(&binding.from) {
        return Err(BatchExecutionError::InvalidBinding);
    }
    Ok(Some(binding))
}

/// Validates every binding edge against the typed source and destination registry.
///
/// This check is independent of child results and therefore runs before
/// identity resolution or any child dispatch.
///
/// # Errors
///
/// Returns [`BatchExecutionError::InvalidBinding`] for undeclared sources,
/// unregistered compatibility paths, incompatible types, cardinalities, or
/// trust classes.
pub fn validate_typed_bindings(
    input: &QueryBatchInput,
    dependencies: &[Vec<usize>],
) -> Result<(), BatchExecutionError> {
    if dependencies.len() != input.operations.len() {
        return Err(BatchExecutionError::InvalidBinding);
    }
    for (index, operation) in input.operations.iter().enumerate() {
        for (key, value) in &operation.arguments {
            let destination = append_pointer("", key)?;
            validate_typed_binding_value(
                value,
                operation.tool,
                &destination,
                &input.operations,
                &dependencies[index],
            )?;
        }
    }
    Ok(())
}

fn validate_typed_binding_value(
    value: &Value,
    target_tool: BatchTool,
    destination: &str,
    operations: &[ContractBatchOperation],
    declared: &[usize],
) -> Result<(), BatchExecutionError> {
    match value {
        Value::Object(map) => {
            if let Some(binding) = binding_reference(map)? {
                let dependency = declared
                    .iter()
                    .find(|&&index| operations[index].id == binding.from)
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                let source = translate_source_binding(
                    operations[*dependency].tool,
                    binding.source,
                    binding.index,
                )?;
                let target = translate_target_binding(target_tool, destination)?;
                validate_binding_pair(source.slot, target)
            } else {
                for (key, inner) in map {
                    let child_destination = append_pointer(destination, key)?;
                    validate_typed_binding_value(
                        inner,
                        target_tool,
                        &child_destination,
                        operations,
                        declared,
                    )?;
                }
                Ok(())
            }
        }
        Value::Array(items) => {
            for (index, inner) in items.iter().enumerate() {
                let child_destination = append_pointer(destination, &index.to_string())?;
                validate_typed_binding_value(
                    inner,
                    target_tool,
                    &child_destination,
                    operations,
                    declared,
                )?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

struct TypedSourceBinding<'a> {
    slot: &'a BatchBindingSourceSlot,
    indices: Vec<usize>,
}

fn translate_source_binding(
    tool: BatchTool,
    source: rootlight_mcp_contract::batch::BatchBindingSource,
    index: Option<u16>,
) -> Result<TypedSourceBinding<'static>, BatchExecutionError> {
    for slot in batch_descriptor(tool).bindings.sources {
        if slot.source != source || !slot.composable {
            continue;
        }
        let mut indices = Vec::new();
        for segment in slot.path {
            if let BatchBindingPathSegment::Index { max_exclusive } = segment {
                if !indices.is_empty() {
                    return Err(BatchExecutionError::InvalidBinding);
                }
                let index = index.ok_or(BatchExecutionError::InvalidBinding)?;
                if index >= *max_exclusive {
                    return Err(BatchExecutionError::InvalidBinding);
                }
                indices.push(usize::from(index));
            }
        }
        if indices.is_empty() != index.is_none() {
            return Err(BatchExecutionError::InvalidBinding);
        }
        return Ok(TypedSourceBinding { slot, indices });
    }
    Err(BatchExecutionError::InvalidBinding)
}

fn translate_target_binding(
    tool: BatchTool,
    destination: &str,
) -> Result<&'static BatchBindingTargetSlot, BatchExecutionError> {
    let segments = compatibility_segments(destination, false)?;
    for target in batch_descriptor(tool).bindings.targets {
        if match_typed_path(target.path, &segments)?.is_some() {
            return Ok(target);
        }
    }
    Err(BatchExecutionError::InvalidBinding)
}

fn compatibility_segments(pointer: &str, source: bool) -> Result<Vec<&str>, BatchExecutionError> {
    if !(1..=1024).contains(&pointer.len()) || pointer.contains('~') {
        return Err(BatchExecutionError::InvalidBinding);
    }
    let mut raw = pointer.split('/');
    if raw.next() != Some("") {
        return Err(BatchExecutionError::InvalidBinding);
    }
    if source && raw.next() != Some("data") {
        return Err(BatchExecutionError::InvalidBinding);
    }
    let mut segments = Vec::new();
    segments
        .try_reserve(pointer.matches('/').count())
        .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
    for segment in raw {
        if segment.is_empty() {
            return Err(BatchExecutionError::InvalidBinding);
        }
        segments.push(segment);
    }
    Ok(segments)
}

fn match_typed_path(
    pattern: &[BatchBindingPathSegment],
    segments: &[&str],
) -> Result<Option<Vec<usize>>, BatchExecutionError> {
    if pattern.len() != segments.len() {
        return Ok(None);
    }
    let mut indices = Vec::new();
    indices
        .try_reserve(pattern.len())
        .map_err(|_| BatchExecutionError::MemoryUnavailable)?;
    for (expected, actual) in pattern.iter().zip(segments) {
        match expected {
            BatchBindingPathSegment::Field(field) if *field == *actual => {}
            BatchBindingPathSegment::Index { max_exclusive } => {
                if actual.len() > 1 && actual.starts_with('0') {
                    return Ok(None);
                }
                let Ok(index) = actual.parse::<usize>() else {
                    return Ok(None);
                };
                if index >= usize::from(*max_exclusive) {
                    return Ok(None);
                }
                indices.push(index);
            }
            BatchBindingPathSegment::Field(_) => return Ok(None),
        }
    }
    Ok(Some(indices))
}

fn validate_binding_pair(
    source: &BatchBindingSourceSlot,
    target: &BatchBindingTargetSlot,
) -> Result<(), BatchExecutionError> {
    if !source.composable
        || source.value_type != target.value_type
        || !target.accepted_trust.contains(&source.trust)
        || !matches!(
            (source.cardinality, target.cardinality),
            (
                BatchBindingCardinality::Scalar,
                BatchBindingCardinality::Scalar
            ) | (
                BatchBindingCardinality::Collection { .. },
                BatchBindingCardinality::Collection { .. }
            )
        )
    {
        return Err(BatchExecutionError::InvalidBinding);
    }
    Ok(())
}

fn extract_typed_source<'a>(
    data: &'a Value,
    slot: &BatchBindingSourceSlot,
    indices: &[usize],
) -> Result<&'a Value, BatchExecutionError> {
    let mut current = data;
    let mut index_cursor = 0;
    for segment in slot.path {
        current = match segment {
            BatchBindingPathSegment::Field(field) => current
                .as_object()
                .and_then(|object| object.get(*field))
                .ok_or(BatchExecutionError::MissingBindingValue)?,
            BatchBindingPathSegment::Index { .. } => {
                let index = *indices
                    .get(index_cursor)
                    .ok_or(BatchExecutionError::InvalidBinding)?;
                index_cursor += 1;
                current
                    .as_array()
                    .and_then(|array| array.get(index))
                    .ok_or(BatchExecutionError::MissingBindingValue)?
            }
        };
    }
    if current.is_null() {
        return Err(BatchExecutionError::MissingBindingValue);
    }
    Ok(current)
}

fn validate_runtime_binding(
    value: &Value,
    source: &BatchBindingSourceSlot,
    target: &BatchBindingTargetSlot,
    expected_repository: RepositoryId,
    expected_generation: GenerationId,
) -> Result<(), BatchExecutionError> {
    validate_cardinality(value, source.cardinality)?;
    validate_cardinality(value, target.cardinality)?;
    match source.value_type {
        BatchBindingValueType::SymbolId => {
            serde_json::from_value::<SymbolId>(value.clone())
                .map_err(|_| BatchExecutionError::BindingTypeMismatch)?;
        }
        BatchBindingValueType::SymbolIds => {
            serde_json::from_value::<Vec<SymbolId>>(value.clone())
                .map_err(|_| BatchExecutionError::BindingTypeMismatch)?;
        }
        BatchBindingValueType::SourceRef => {
            let source_ref = serde_json::from_value::<SourceRef>(value.clone())
                .map_err(|_| BatchExecutionError::BindingTypeMismatch)?;
            validate_source_identity(&source_ref, expected_repository, expected_generation)?;
        }
        BatchBindingValueType::SourceRefs => {
            let source_refs = serde_json::from_value::<Vec<SourceRef>>(value.clone())
                .map_err(|_| BatchExecutionError::BindingTypeMismatch)?;
            for source_ref in &source_refs {
                validate_source_identity(source_ref, expected_repository, expected_generation)?;
            }
        }
        BatchBindingValueType::TestId => {
            let value = value
                .as_str()
                .ok_or(BatchExecutionError::BindingTypeMismatch)?;
            if !(1..=512).contains(&value.len()) {
                return Err(BatchExecutionError::BindingTypeMismatch);
            }
        }
        BatchBindingValueType::PackId => {
            let value = value
                .as_str()
                .ok_or(BatchExecutionError::BindingTypeMismatch)?;
            if !(1..=128).contains(&value.len()) {
                return Err(BatchExecutionError::BindingTypeMismatch);
            }
        }
    }
    Ok(())
}

fn validate_cardinality(
    value: &Value,
    cardinality: BatchBindingCardinality,
) -> Result<(), BatchExecutionError> {
    match cardinality {
        BatchBindingCardinality::Scalar if !value.is_array() && !value.is_null() => Ok(()),
        BatchBindingCardinality::Scalar => Err(BatchExecutionError::BindingCardinalityMismatch),
        BatchBindingCardinality::Collection { min, max } => {
            let length = value
                .as_array()
                .map(Vec::len)
                .ok_or(BatchExecutionError::BindingCardinalityMismatch)?;
            if (usize::from(min)..=usize::from(max)).contains(&length) {
                Ok(())
            } else {
                Err(BatchExecutionError::BindingCardinalityMismatch)
            }
        }
    }
}

fn validate_source_identity(
    source_ref: &SourceRef,
    expected_repository: RepositoryId,
    expected_generation: GenerationId,
) -> Result<(), BatchExecutionError> {
    if source_ref.repository() != expected_repository
        || source_ref.generation() != expected_generation
    {
        return Err(BatchExecutionError::BindingIdentityMismatch);
    }
    Ok(())
}

/// Default aggregate token ceiling for a batch without an explicit budget.
pub const DEFAULT_BATCH_TOKENS: u16 = 3_000;

/// Default wall-clock ceiling for a batch without an explicit timeout.
pub const DEFAULT_BATCH_TIMEOUT_MS: u32 = 30_000;

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
        let plan = StaticBatchPlan::build(input, ExposureProfile::Developer)?;
        self.execute_plan(port, plan, repository, cancellation, errors)
            .await
    }

    /// Executes one fully admitted immutable plan.
    ///
    /// Transport boundaries must run standalone child-template validation
    /// before invoking this method.
    ///
    /// # Errors
    ///
    /// Returns [`BatchOrchestrationError`] for identity, runtime binding,
    /// cancellation, deadline, budget, or child-contract failures.
    pub async fn execute_plan<P, C>(
        &self,
        port: Arc<P>,
        plan: StaticBatchPlan,
        repository: RepositoryId,
        cancellation: C,
        errors: BatchPublicErrors,
    ) -> Result<ReadEnvelope<QueryBatchData>, BatchOrchestrationError>
    where
        P: AgentToolPort<C>,
        C: CancellationSignal + Clone + Send + Sync + 'static,
    {
        checkpoint(&cancellation)?;
        let parent_budget = plan.parent_budget.clone();
        let started_at = Instant::now();
        let parent_deadline = deadline_from(started_at, parent_budget.timeout_ms)?
            .ok_or(BatchOrchestrationError::Internal)?;
        let identity = resolve_identity(
            Arc::clone(&port),
            plan.repository.clone(),
            plan.generation.clone(),
            cancellation.clone(),
            parent_deadline,
        )
        .await?;
        if identity.repository.repository_id != repository {
            return Err(BatchOrchestrationError::InvalidResponse);
        }

        let mut parent_ledger = BudgetLedger::new(Some(parent_budget.clone()));
        parent_ledger
            .charge(minimum_publication_charge(
                &plan,
                &identity,
                &errors.operation_failed,
            )?)
            .map_err(map_policy_error)?;
        let fail_fast = matches!(plan.failure_policy, FailurePolicy::FailFast);
        let count = plan.operations.len();
        let mut results: Vec<Option<BatchOperationResult>> = vec![None; count];
        let mut binding_envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
        let mut observed_envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
        let mut failure_receipts: Vec<Option<UsageSummary>> = vec![None; count];
        let mut aggregate_limiting_resource = None;
        let mut stop_scheduling = false;

        for index in &plan.execution_order {
            let index = *index;
            if cancellation.is_cancelled() {
                fill_unresolved_slots(
                    &plan.operations,
                    &mut results,
                    BatchOperationStatus::Cancelled,
                );
                aggregate_limiting_resource = Some(LimitingResourceKind::Cancellation);
                break;
            }
            if Instant::now() >= parent_deadline {
                fill_unresolved_slots(
                    &plan.operations,
                    &mut results,
                    BatchOperationStatus::NotRunBudget,
                );
                aggregate_limiting_resource = Some(LimitingResourceKind::Deadline);
                break;
            }
            let operation = &plan.operations[index];
            if aggregate_limiting_resource.is_some() {
                results[index] = Some(planned_terminal_result(
                    operation,
                    BatchOperationStatus::NotRunBudget,
                ));
                continue;
            }
            if dependency_failed(&operation.dependencies, &results) {
                results[index] = Some(planned_terminal_result(
                    operation,
                    BatchOperationStatus::SkippedDependency,
                ));
                continue;
            }
            if stop_scheduling {
                results[index] = Some(planned_terminal_result(
                    operation,
                    BatchOperationStatus::NotRunFailFast,
                ));
                continue;
            }

            let mut resolved = match operation.materialize_arguments(
                &binding_envelopes,
                identity.repository.repository_id,
                identity.generation.generation_id,
            ) {
                Ok(resolved) => resolved,
                Err(
                    BatchExecutionError::InvalidBinding
                    | BatchExecutionError::MissingBindingValue
                    | BatchExecutionError::BindingTypeMismatch
                    | BatchExecutionError::BindingCardinalityMismatch
                    | BatchExecutionError::BindingIdentityMismatch,
                ) => {
                    results[index] = Some(planned_error_result(operation, &errors.binding_invalid));
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
            resolved.arguments.insert(
                "repository".to_owned(),
                serde_json::to_value(rootlight_mcp_contract::RepositorySelector::ById(
                    RepositoryIdSelector {
                        repository_id: identity.repository.repository_id,
                    },
                ))
                .map_err(|_| BatchOrchestrationError::Internal)?,
            );
            resolved.arguments.insert(
                "generation".to_owned(),
                serde_json::to_value(GenerationSelector::Explicit(
                    identity.generation.generation_id,
                ))
                .map_err(|_| BatchOrchestrationError::Internal)?,
            );
            let remaining = match remaining_parent_budget(
                &parent_budget,
                parent_ledger.consumed(),
                started_at.elapsed(),
            ) {
                Ok(remaining) => remaining,
                Err(BatchOrchestrationError::BudgetExceeded) => {
                    fill_unresolved_slots(
                        &plan.operations,
                        &mut results,
                        BatchOperationStatus::NotRunBudget,
                    );
                    aggregate_limiting_resource = Some(LimitingResourceKind::EstimatedTokens);
                    break;
                }
                Err(error) => return Err(error),
            };
            let tool_limits =
                BudgetLimits::server_ceiling().constrained_by_response_budget(Some(&remaining));
            let mut allocation =
                match parent_ledger.allocate_child(tool_limits, operation.local_budget.as_ref()) {
                    Ok(allocation) => allocation,
                    Err(ExecutionPolicyError::BudgetExceeded { resource }) => {
                        fill_unresolved_slots(
                            &plan.operations,
                            &mut results,
                            BatchOperationStatus::NotRunBudget,
                        );
                        aggregate_limiting_resource = Some(limiting_resource_kind(resource));
                        break;
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
            .with_response_profile(plan.response_profile)
            .with_pinned_identity(identity.clone())
            .with_local_deadline(effective_deadline.source == DeadlineSource::Local);
            let request = AgentToolRequest::new(operation.tool, resolved.arguments)
                .with_materialized_binding_paths(resolved.materialized_binding_paths);

            let response = port.execute(request, context).await;
            match response {
                Ok(envelope) => {
                    validate_child_identity(&envelope, &identity)?;

                    let charge = charge_for(operation.tool, &envelope)?;
                    let mut public_envelope = envelope.clone();
                    public_envelope.data = shape_batch_child_data(
                        operation.tool,
                        &envelope.data,
                        plan.response_profile,
                    )
                    .map_err(|error| match error {
                        BatchProfileProjectionError::UnsupportedProfile => {
                            BatchOrchestrationError::UnsupportedProfile
                        }
                        BatchProfileProjectionError::InvalidData(_) => {
                            BatchOrchestrationError::InvalidResponse
                        }
                    })?;
                    observed_envelopes[index] = Some(public_envelope.clone());

                    if allocation.ledger_mut().charge(charge).is_err() {
                        consume_allocation(allocation)?;
                        results[index] =
                            Some(planned_error_result(operation, &errors.budget_exceeded));
                        stop_scheduling |= fail_fast;
                        continue;
                    }
                    allocation.commit().map_err(map_policy_error)?;
                    results[index] = Some(planned_success_result(operation, &public_envelope));
                    binding_envelopes[index] = Some(envelope);
                }
                Err(error) => {
                    let (error, measured) = error.into_parts();
                    if let Some(usage) = measured.as_ref() {
                        failure_receipts[index] = Some(usage.clone());
                        let charge = charge_for_usage(usage);
                        if allocation.ledger_mut().charge(charge).is_err() {
                            consume_allocation(allocation)?;
                            results[index] = Some(planned_error_result_with_usage(
                                operation,
                                &errors.budget_exceeded,
                                measured,
                            ));
                            aggregate_limiting_resource =
                                Some(LimitingResourceKind::EstimatedTokens);
                            stop_scheduling |= fail_fast;
                            continue;
                        }
                        allocation.commit().map_err(map_policy_error)?;
                    }
                    match error {
                        AgentPortError::Public(error) => {
                            results[index] =
                                Some(planned_error_result_with_usage(operation, &error, measured));
                            stop_scheduling |= fail_fast;
                        }
                        AgentPortError::Cancelled => {
                            let mut result =
                                planned_terminal_result(operation, BatchOperationStatus::Cancelled);
                            result.usage = measured;
                            results[index] = Some(result);
                            fill_unresolved_slots(
                                &plan.operations,
                                &mut results,
                                BatchOperationStatus::Cancelled,
                            );
                            aggregate_limiting_resource = Some(LimitingResourceKind::Cancellation);
                            break;
                        }
                        AgentPortError::DeadlineExceeded => {
                            results[index] = Some(planned_error_result_with_usage(
                                operation,
                                &errors.budget_exceeded,
                                measured,
                            ));
                            fill_unresolved_slots(
                                &plan.operations,
                                &mut results,
                                BatchOperationStatus::NotRunBudget,
                            );
                            aggregate_limiting_resource = Some(LimitingResourceKind::Deadline);
                            break;
                        }
                        AgentPortError::LocalDeadlineExceeded => {
                            results[index] = Some(planned_error_result_with_usage(
                                operation,
                                &errors.budget_exceeded,
                                measured,
                            ));
                            stop_scheduling |= fail_fast;
                        }
                        AgentPortError::InvalidResponse => {
                            return Err(BatchOrchestrationError::InvalidResponse);
                        }
                        AgentPortError::Unavailable => {
                            results[index] = Some(planned_error_result_with_usage(
                                operation,
                                &errors.operation_failed,
                                measured,
                            ));
                            stop_scheduling |= fail_fast;
                        }
                        AgentPortError::Measured { .. } => {
                            return Err(BatchOrchestrationError::InvalidResponse);
                        }
                    }
                }
            }
        }

        let operation_results = finalize_result_slots(&plan.operations, results)?;
        let truncated = operation_results
            .iter()
            .any(|result| result.truncated || result.status == BatchOperationStatus::NotRunBudget);
        let completeness = aggregate_completeness(
            &observed_envelopes,
            &operation_results,
            aggregate_limiting_resource,
        )?;
        let usage = aggregate_usage_with_receipts(&observed_envelopes, &failure_receipts);
        let warnings = aggregate_warnings(&identity.warnings, &operation_results);
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
            warnings,
            trust: TrustClassification::UntrustedRepositoryData,
        })
    }
}

fn aggregate_warnings(
    identity: &[rootlight_mcp_contract::vertical::ResponseWarning],
    results: &[BatchOperationResult],
) -> Vec<rootlight_mcp_contract::vertical::ResponseWarning> {
    let mut warnings = identity.to_vec();
    for warning in results.iter().flat_map(|result| &result.warnings) {
        if warnings.len() == 32 {
            break;
        }
        if !warnings.contains(warning) {
            warnings.push(warning.clone());
        }
    }
    warnings
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
    repository: rootlight_mcp_contract::RepositorySelector,
    generation: Option<GenerationSelector>,
    cancellation: C,
    deadline: Instant,
) -> Result<AgentResolvedIdentity, BatchOrchestrationError>
where
    P: AgentToolPort<C>,
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    let requested_generation = generation.clone();
    let request = AgentIdentityRequest::new(repository, generation);
    let context = AgentResolutionContext::new(cancellation, deadline);
    match port.resolve_identity(request, context).await {
        Ok(identity)
            if matches!(
                requested_generation.as_ref(),
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
        Err(AgentPortError::Measured { .. }) => Err(BatchOrchestrationError::InvalidResponse),
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

fn charge_for_usage(usage: &UsageSummary) -> BudgetCharge {
    BudgetCharge {
        rows: usage.rows,
        tokens: usage.estimated_tokens,
        source_bytes: usage.source_bytes,
        traversal_facts: usage.edges,
        json_bytes: usage.json_bytes,
        time_ms: usage.wall_time_ms,
        ..BudgetCharge::default()
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
        BatchTool::PlanChange => "steps",
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
        | BatchExecutionError::InvalidBinding
        | BatchExecutionError::MissingBindingValue
        | BatchExecutionError::BindingTypeMismatch
        | BatchExecutionError::BindingCardinalityMismatch
        | BatchExecutionError::BindingIdentityMismatch => BatchOrchestrationError::InvalidArguments,
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
        resolve_dependencies, terminal_result, translate_target_binding, validate_binding_pair,
    };
    use crate::policy::BudgetLimits;
    use proptest::prelude::*;
    use rootlight_mcp_contract::{
        ExposureProfile, McpTool,
        batch::BatchBindingCardinality,
        context::{
            BatchOperation as ContractBatchOperation, BatchOperationStatus, BatchStatus, BatchTool,
        },
        vertical::ResponseBudget,
    };
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
            arguments: rootlight_mcp_contract::context::BatchArguments::new(),
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

    proptest! {
        #[test]
        fn valid_dependency_graphs_plan_deterministically(masks in prop::collection::vec(any::<u8>(), 1..=MAX_BATCH_OPERATIONS)) {
            let tools = vec![McpTool::CodeLocate; masks.len()];
            let dependencies = masks
                .iter()
                .enumerate()
                .map(|(index, mask)| {
                    (0..index.min(8))
                        .filter(|dependency| (*mask & (1_u8 << dependency)) != 0)
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();

            let first = BatchPlan::validate(&tools, &dependencies)
                .expect("backward-only dependencies form a bounded DAG");
            let second = BatchPlan::validate(&tools, &dependencies)
                .expect("the same bounded DAG remains valid");
            prop_assert_eq!(&first.execution_order, &second.execution_order);

            let positions = first
                .execution_order
                .iter()
                .enumerate()
                .map(|(position, operation)| (*operation, position))
                .collect::<std::collections::BTreeMap<_, _>>();
            for (operation, declared) in dependencies.iter().enumerate() {
                for dependency in declared {
                    prop_assert!(positions[dependency] < positions[&operation]);
                }
            }
        }

        #[test]
        fn arbitrary_target_paths_cannot_escape_the_binding_registry(segment in "[A-Za-z0-9_]{1,32}") {
            let path = format!("/unregistered_{segment}");
            for descriptor in rootlight_mcp_contract::batch::BATCH_TOOL_REGISTRY {
                prop_assert!(
                    translate_target_binding(descriptor.batch_tool, &path).is_err(),
                    "{} unexpectedly accepted {path}",
                    descriptor.batch_tool.name()
                );
            }
        }
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
    fn binding_registry_matrix_matches_the_typed_compatibility_rule() {
        let registry = rootlight_mcp_contract::batch::BATCH_TOOL_REGISTRY;
        let mut compatible_pairs = 0usize;
        for source_tool in registry {
            for source in source_tool.bindings.sources {
                let mut source_has_target = false;
                for target_tool in registry {
                    for target in target_tool.bindings.targets {
                        let same_cardinality_class = matches!(
                            (source.cardinality, target.cardinality),
                            (
                                BatchBindingCardinality::Scalar,
                                BatchBindingCardinality::Scalar
                            ) | (
                                BatchBindingCardinality::Collection { .. },
                                BatchBindingCardinality::Collection { .. }
                            )
                        );
                        let expected = source.composable
                            && source.value_type == target.value_type
                            && target.accepted_trust.contains(&source.trust)
                            && same_cardinality_class;
                        assert_eq!(
                            validate_binding_pair(source, target).is_ok(),
                            expected,
                            "registry compatibility drifted for {:?} -> {:?}",
                            source.source,
                            target_tool.batch_tool
                        );
                        source_has_target |= expected;
                        compatible_pairs += usize::from(expected);
                    }
                }
                assert_eq!(
                    source_has_target, source.composable,
                    "composable source {:?} must have a typed destination",
                    source.source
                );
            }
        }
        assert!(compatible_pairs > 0);
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
