//! Bounded MCP tool discovery and invocation routing.
//!
//! This module validates both sides of the generic daemon executor boundary
//! and keeps MCP structured content identical to its JSON text mirror.

use std::{fmt, future::Future, io, pin::Pin, sync::Arc};

use jsonschema::Validator;
use rootlight_mcp_contract::{
    CodeLocateInput, CodeLocateOutput, DetailKey, ErrorCode, ErrorResponse, ExposureProfile,
    GenerationSelector, McpTool, NextAction, OperationStatusInput, OperationStatusOutput,
    PublicError, RepoIndexInput, RepoIndexOutput, RepositorySelector, SchemaVersion,
    SourceReadInput, SourceReadOutput, SymbolExplainInput, SymbolExplainOutput, ToolResponse,
    TrustClassification, VerticalTool,
    context::{
        BatchOperation, BatchTool, ContextPackInput, QueryAdvancedInput, QueryAstNode,
        QueryBatchInput,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio::sync::watch;

use super::{
    DEFAULT_MAX_RESPONSE_BYTES, HandlerCapabilities, HandlerFuture, HandlerResponse,
    INVALID_PARAMS, MAX_REQUEST_ID_BYTES, METHOD_NOT_FOUND, OperatingRequest, RequestCancellation,
    RequestHandler, request_meta_is_valid,
};
use crate::advanced::{AdvancedQueryPlan, MAX_ADVANCED_TRAVERSAL, QueryOperator};
use crate::batch::{BatchPlan, is_batch_allowed_under_profile};

const INTERNAL_ERROR: i32 = -32_603;
const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_REPOSITORY_ROOT_BYTES: usize = 4_096;
const MAX_CONFIGURATION_PATCH_BYTES: usize = 64 * 1_024;
const MAX_LOCATE_QUERY_BYTES: usize = 2_048;
const INVALID_ARGUMENT_MESSAGE: &str = "tool arguments do not match the input schema";
const RESOURCE_EXHAUSTED_MESSAGE: &str = "tool result exceeds the mcp response limit";
const MAX_REPO_INDEX_ARGUMENT_BYTES: usize = 96 * 1_024;
const MAX_OPERATION_STATUS_ARGUMENT_BYTES: usize = 16 * 1_024;
const MAX_CODE_LOCATE_ARGUMENT_BYTES: usize = 64 * 1_024;
const MAX_SYMBOL_EXPLAIN_ARGUMENT_BYTES: usize = 64 * 1_024;
const MAX_SOURCE_READ_ARGUMENT_BYTES: usize = 64 * 1_024;
const MAX_JSON_RPC_RESPONSE_OVERHEAD: usize = (MAX_REQUEST_ID_BYTES * 6) + 256;
const MAX_TOOL_RESULT_BYTES: usize =
    DEFAULT_MAX_RESPONSE_BYTES - MAX_JSON_RPC_RESPONSE_OVERHEAD - 1;
const MAX_TOOL_RESULT_FIXED_BYTES: usize = 512;
const MAX_TOOL_STRUCTURED_BYTES: usize = (MAX_TOOL_RESULT_BYTES - MAX_TOOL_RESULT_FIXED_BYTES) / 3;

/// Future returned by a vertical tool executor.
pub type ToolExecutionFuture =
    Pin<Box<dyn Future<Output = Result<Map<String, Value>, ToolExecutionError>> + Send + 'static>>;

/// Daemon-backed implementation of the five first-slice tool operations.
///
/// The executor returns the complete tool-specific output envelope. The router
/// validates that object against the advertised output schema before exposing
/// it as MCP structured content.
pub trait ToolExecutor: Send + Sync + 'static {
    /// Executes one schema-validated tool request.
    fn execute(
        &self,
        tool: VerticalTool,
        arguments: Map<String, Value>,
        cancellation: RequestCancellation,
    ) -> ToolExecutionFuture;
}

/// Source-free failure returned by an MCP tool executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolExecutionError {
    kind: ToolExecutionErrorKind,
}

impl ToolExecutionError {
    /// Wraps one checked, source-redacted public error.
    #[must_use]
    pub fn new(error: PublicError) -> Self {
        Self {
            kind: ToolExecutionErrorKind::Public(Box::new(error)),
        }
    }

    /// Creates one source-free internal executor failure.
    #[must_use]
    pub const fn internal(failure: ToolExecutionFailure) -> Self {
        Self {
            kind: ToolExecutionErrorKind::Internal(failure),
        }
    }

    /// Returns the checked public error for an expected domain failure.
    #[must_use]
    pub const fn public_error(&self) -> Option<&PublicError> {
        match &self.kind {
            ToolExecutionErrorKind::Public(error) => Some(error),
            ToolExecutionErrorKind::Internal(_) => None,
        }
    }

    /// Returns the static internal failure classification.
    #[must_use]
    pub const fn failure(&self) -> Option<ToolExecutionFailure> {
        match self.kind {
            ToolExecutionErrorKind::Public(_) => None,
            ToolExecutionErrorKind::Internal(failure) => Some(failure),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolExecutionErrorKind {
    Public(Box<PublicError>),
    Internal(ToolExecutionFailure),
}

/// Static executor failure classes that must not expose causal text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ToolExecutionFailure {
    /// The local daemon transport failed.
    Transport,
    /// A daemon response could not be mapped without inventing data.
    InvalidResponse,
    /// The executor itself failed before producing a checked response.
    Executor,
}

impl ToolExecutionFailure {
    const fn message(self) -> &'static str {
        match self {
            Self::Transport => "tool transport failed",
            Self::InvalidResponse => "tool response mapping failed",
            Self::Executor => "tool executor failed",
        }
    }
}

/// Handler that advertises and routes the strict first-slice tool catalog.
pub struct ToolRouter<E> {
    executor: Arc<E>,
    contracts: Arc<[ToolContract]>,
    /// Precomputed `tools/list` payloads, one per profile, indexed by
    /// [`profile_index`]. Building all three once keeps per-request discovery
    /// allocation-free while the negotiated profile stays dynamic.
    list_results: [Map<String, Value>; 3],
    /// Server policy ceiling; the negotiated profile is never served above it.
    ceiling: ExposureProfile,
    /// Read handle to the session-negotiated current profile.
    profile: watch::Receiver<ExposureProfile>,
    invalid_arguments: PublicError,
    resource_exhausted: PublicError,
}

impl<E> ToolRouter<E>
where
    E: ToolExecutor,
{
    /// Compiles every checked input and output schema before the session starts
    /// and serves a single fixed exposure profile.
    ///
    /// This convenience constructor is for transports that do not negotiate a
    /// profile dynamically; it fixes the current profile to `profile` under a
    /// fully permissive [`ExposureProfile::Developer`] ceiling. Use
    /// [`ToolRouter::with_shared_profile`] to serve a session-negotiated profile
    /// under a server policy ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRegistryError`] when a checked server-owned schema cannot
    /// be parsed, compiled, or represented as an MCP tool definition.
    pub fn new(executor: E, profile: ExposureProfile) -> Result<Self, ToolRegistryError> {
        let (_sender, receiver) = watch::channel(profile);
        Self::with_shared_profile(executor, receiver, ExposureProfile::Developer)
    }

    /// Compiles every checked input and output schema before the session starts.
    ///
    /// The router serves the profile currently held by `profile`, re-clamped to
    /// the server policy `ceiling` on every request. The exposure profile
    /// filters which tools appear in `tools/list` discovery and which calls are
    /// authorized; it never changes tool semantics, limits, or permission
    /// policy. Discovery payloads for all three profiles are precomputed once so
    /// a negotiated profile change does not recompile contracts.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRegistryError`] when a checked server-owned schema cannot
    /// be parsed, compiled, or represented as an MCP tool definition.
    pub fn with_shared_profile(
        executor: E,
        profile: watch::Receiver<ExposureProfile>,
        ceiling: ExposureProfile,
    ) -> Result<Self, ToolRegistryError> {
        let invalid_arguments = checked_public_error(
            ErrorCode::InvalidArgument,
            INVALID_ARGUMENT_MESSAGE,
            "arguments",
        )?;
        let resource_exhausted = checked_public_error(
            ErrorCode::ResourceExhausted,
            RESOURCE_EXHAUSTED_MESSAGE,
            "budget",
        )?;
        let mut contracts = Vec::new();
        contracts
            .try_reserve_exact(VerticalTool::ALL.len())
            .map_err(|_| ToolRegistryError::MemoryUnavailable)?;
        for tool in VerticalTool::ALL {
            contracts.push(ToolContract::compile(tool)?);
        }

        // Precompute one discovery payload per profile so a negotiated profile
        // change never recompiles contracts or reallocates definitions.
        let mut list_results = Vec::new();
        list_results
            .try_reserve_exact(ExposureProfile::ALL.len())
            .map_err(|_| ToolRegistryError::MemoryUnavailable)?;
        for candidate in ExposureProfile::ALL {
            let mut definitions = Vec::new();
            definitions
                .try_reserve_exact(contracts.len())
                .map_err(|_| ToolRegistryError::MemoryUnavailable)?;
            for contract in &contracts {
                if profile_exposes_tool(candidate, contract.tool.name()) {
                    definitions.push(
                        serde_json::to_value(&contract.definition)
                            .map_err(ToolRegistryError::SerializeDefinition)?,
                    );
                }
            }
            list_results.push(Map::from_iter([(
                "tools".to_owned(),
                Value::Array(definitions),
            )]));
        }
        let list_results: [Map<String, Value>; 3] = list_results
            .try_into()
            .map_err(|_| ToolRegistryError::MemoryUnavailable)?;

        Ok(Self {
            executor: Arc::new(executor),
            contracts: contracts.into(),
            list_results,
            ceiling,
            profile,
            invalid_arguments,
            resource_exhausted,
        })
    }

    /// Reads the negotiated profile, defensively re-clamping to the ceiling so a
    /// stale or out-of-band write can never widen discovery past server policy.
    fn current_profile(&self) -> ExposureProfile {
        let current = *self.profile.borrow();
        current.clamped_to(self.ceiling)
    }

    fn list_tools(&self, params: Option<Value>) -> HandlerResponse {
        if !list_params_are_valid(params.as_ref()) {
            return HandlerResponse::error(INVALID_PARAMS, "invalid tools/list parameters");
        }
        let index = profile_index(self.current_profile());
        HandlerResponse::Success(self.list_results[index].clone())
    }

    async fn call_tool(
        executor: Arc<E>,
        contracts: Arc<[ToolContract]>,
        profile: ExposureProfile,
        invalid_arguments: PublicError,
        resource_exhausted: PublicError,
        params: Option<Value>,
        cancellation: RequestCancellation,
    ) -> HandlerResponse {
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }
        let (name, arguments) = match decode_call_params(params) {
            Ok(decoded) => decoded,
            Err(CallParamsError::Invalid) => {
                return cancel_or(
                    &cancellation,
                    HandlerResponse::error(INVALID_PARAMS, "invalid tools/call parameters"),
                );
            }
            Err(CallParamsError::TaskUnsupported) => {
                return cancel_or(
                    &cancellation,
                    HandlerResponse::error(
                        METHOD_NOT_FOUND,
                        "task augmented tool calls are not supported",
                    ),
                );
            }
        };
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }
        let Some(contract) = contracts
            .iter()
            .find(|contract| contract.tool.name() == name)
        else {
            return cancel_or(
                &cancellation,
                HandlerResponse::error(INVALID_PARAMS, "tool is not available"),
            );
        };
        if !profile_exposes_tool(profile, &name) {
            return cancel_or(
                &cancellation,
                HandlerResponse::error(INVALID_PARAMS, "tool is not available"),
            );
        }
        let arguments_value = Value::Object(arguments);
        if !tool_argument_bytes_are_valid(contract.tool, &arguments_value)
            || !contract.input_validator.is_valid(&arguments_value)
            || !tool_specific_input_limits_are_valid(contract.tool, &arguments_value)
        {
            return cancel_or(
                &cancellation,
                tool_error(contract, invalid_arguments)
                    .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
            );
        }
        let typed_input = match decode_typed_input(contract.tool, &arguments_value) {
            Ok(input) => input,
            Err(()) => {
                return cancel_or(
                    &cancellation,
                    tool_error(contract, invalid_arguments)
                        .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
                );
            }
        };
        if !typed_input_invariants_are_valid(contract.tool, &typed_input, profile) {
            return cancel_or(
                &cancellation,
                tool_error(contract, invalid_arguments)
                    .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
            );
        }
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }
        let Value::Object(arguments) = arguments_value else {
            return cancel_or(
                &cancellation,
                internal_tool_error("tool input invariant failed"),
            );
        };

        let execution = executor
            .execute(contract.tool, arguments, cancellation.clone())
            .await;
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }
        let output = match execution {
            Ok(output) => output,
            Err(ToolExecutionError {
                kind: ToolExecutionErrorKind::Public(error),
            }) => {
                return cancel_or(
                    &cancellation,
                    tool_error(contract, *error)
                        .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
                );
            }
            Err(ToolExecutionError {
                kind: ToolExecutionErrorKind::Internal(failure),
            }) => {
                return cancel_or(&cancellation, internal_tool_error(failure.message()));
            }
        };
        let output_value = Value::Object(output);
        if !serialized_json_fits(&output_value, MAX_TOOL_STRUCTURED_BYTES) {
            return cancel_or(
                &cancellation,
                tool_error(contract, resource_exhausted)
                    .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
            );
        }
        if !contract.output_validator.is_valid(&output_value)
            || !typed_output_is_valid(contract.tool, &typed_input, &output_value)
        {
            return cancel_or(
                &cancellation,
                internal_tool_error("tool output failed validation"),
            );
        }
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }
        match tool_success(output_value) {
            Ok(response) => cancel_or(&cancellation, response),
            Err(ToolResultError::Limit) => cancel_or(
                &cancellation,
                tool_error(contract, resource_exhausted)
                    .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
            ),
            Err(ToolResultError::Serialize) => cancel_or(
                &cancellation,
                internal_tool_error("tool output serialization failed"),
            ),
        }
    }
}

impl<E> RequestHandler for ToolRouter<E>
where
    E: ToolExecutor,
{
    fn capabilities(&self) -> HandlerCapabilities {
        HandlerCapabilities::tools()
    }

    fn handle(
        &self,
        request: OperatingRequest,
        cancellation: RequestCancellation,
    ) -> HandlerFuture {
        let (method, params) = request.into_method_params();
        match method.as_str() {
            "tools/list" => {
                let response = self.list_tools(params);
                Box::pin(async move { response })
            }
            "tools/call" => {
                let executor = Arc::clone(&self.executor);
                let contracts = Arc::clone(&self.contracts);
                let profile = self.current_profile();
                let invalid_arguments = self.invalid_arguments.clone();
                let resource_exhausted = self.resource_exhausted.clone();
                Box::pin(async move {
                    Self::call_tool(
                        executor,
                        contracts,
                        profile,
                        invalid_arguments,
                        resource_exhausted,
                        params,
                        cancellation,
                    )
                    .await
                })
            }
            _ => Box::pin(async {
                HandlerResponse::error(METHOD_NOT_FOUND, "method is not available")
            }),
        }
    }
}

impl<E> fmt::Debug for ToolRouter<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ToolRouter")
            .field("tool_count", &self.contracts.len())
            .finish_non_exhaustive()
    }
}

struct ToolContract {
    tool: VerticalTool,
    definition: ToolDefinition,
    input_validator: Validator,
    output_validator: Validator,
}

impl ToolContract {
    fn compile(tool: VerticalTool) -> Result<Self, ToolRegistryError> {
        let input_schema =
            parse_object_schema(tool, "input", tool.input_schema_json()).map_err(|source| {
                ToolRegistryError::ParseSchema {
                    tool,
                    direction: "input",
                    source,
                }
            })?;
        let output_schema = parse_object_schema(tool, "output", tool.output_schema_json())
            .map_err(|source| ToolRegistryError::ParseSchema {
                tool,
                direction: "output",
                source,
            })?;
        let input_validator = jsonschema::draft202012::new(&Value::Object(input_schema.clone()))
            .map_err(|source| ToolRegistryError::CompileSchema {
                tool,
                direction: "input",
                detail: source.to_string(),
            })?;
        let output_validator = jsonschema::draft202012::new(&Value::Object(output_schema.clone()))
            .map_err(|source| ToolRegistryError::CompileSchema {
                tool,
                direction: "output",
                detail: source.to_string(),
            })?;
        Ok(Self {
            tool,
            definition: ToolDefinition {
                name: tool.name(),
                title: tool.title(),
                description: tool.description(),
                input_schema,
                output_schema,
                annotations: ToolAnnotations {
                    read_only_hint: tool.read_only(),
                    destructive_hint: tool.destructive(),
                    idempotent_hint: tool.idempotent(),
                    open_world_hint: false,
                },
                execution: ToolExecution {
                    task_support: "forbidden",
                },
            },
            input_validator,
            output_validator,
        })
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDefinition {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    input_schema: Map<String, Value>,
    output_schema: Map<String, Value>,
    annotations: ToolAnnotations,
    execution: ToolExecution,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolAnnotations {
    read_only_hint: bool,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolExecution {
    task_support: &'static str,
}

fn parse_object_schema(
    _tool: VerticalTool,
    _direction: &'static str,
    schema: &'static str,
) -> Result<Map<String, Value>, serde_json::Error> {
    serde_json::from_str(schema)
}

enum TypedInput {
    Other,
    SourceRead(SourceReadInput),
    ContextPack(ContextPackInput),
    QueryAdvanced(QueryAdvancedInput),
    QueryBatch(QueryBatchInput),
}

fn decode_typed_input(tool: VerticalTool, input: &Value) -> Result<TypedInput, ()> {
    // JSON Schema cannot express cross-field range invariants. Reapplying the
    // Rust wire contract keeps malformed SourceRefs behind the MCP boundary.
    match tool {
        VerticalTool::RepoIndex => RepoIndexInput::deserialize(input)
            .map(|_| TypedInput::Other)
            .map_err(|_| ()),
        VerticalTool::RepoStatus
        | VerticalTool::RepoList
        | VerticalTool::SymbolRelationships
        | VerticalTool::FlowTrace
        | VerticalTool::ChangeImpact
        | VerticalTool::TestsSelect
        | VerticalTool::ArchitectureOverview
        | VerticalTool::ArchitectureCycles
        | VerticalTool::CodeDead
        | VerticalTool::HistoryCompare
        | VerticalTool::PlanChange => Ok(TypedInput::Other),
        VerticalTool::ContextPack => ContextPackInput::deserialize(input)
            .map(TypedInput::ContextPack)
            .map_err(|_| ()),
        VerticalTool::QueryAdvanced => QueryAdvancedInput::deserialize(input)
            .map(TypedInput::QueryAdvanced)
            .map_err(|_| ()),
        VerticalTool::QueryBatch => QueryBatchInput::deserialize(input)
            .map(TypedInput::QueryBatch)
            .map_err(|_| ()),
        VerticalTool::OperationStatus => OperationStatusInput::deserialize(input)
            .map(|_| TypedInput::Other)
            .map_err(|_| ()),
        VerticalTool::CodeLocate => CodeLocateInput::deserialize(input)
            .map(|_| TypedInput::Other)
            .map_err(|_| ()),
        VerticalTool::SymbolExplain => SymbolExplainInput::deserialize(input)
            .map(|_| TypedInput::Other)
            .map_err(|_| ()),
        VerticalTool::SourceRead => SourceReadInput::deserialize(input)
            .map(TypedInput::SourceRead)
            .map_err(|_| ()),
    }
}

/// Enforces the cross-field invariants JSON Schema cannot express for the
/// intent tools that carry a typed wire contract.
///
/// These checks run on the public path before execution so malformed batch
/// plans, advanced queries, and context packs are rejected with a checked
/// argument error instead of reaching an executor.
fn typed_input_invariants_are_valid(
    tool: VerticalTool,
    input: &TypedInput,
    profile: ExposureProfile,
) -> bool {
    match (tool, input) {
        (VerticalTool::ContextPack, TypedInput::ContextPack(input)) => {
            context_pack_invariants_are_valid(input)
        }
        (VerticalTool::QueryAdvanced, TypedInput::QueryAdvanced(input)) => {
            advanced_invariants_are_valid(input)
        }
        (VerticalTool::QueryBatch, TypedInput::QueryBatch(input)) => {
            batch_invariants_are_valid(input, profile)
        }
        _ => true,
    }
}

/// A context pack must anchor to at least one non-empty seed kind.
fn context_pack_invariants_are_valid(input: &ContextPackInput) -> bool {
    let seeds = &input.seeds;
    seeds
        .symbols
        .as_ref()
        .is_some_and(|values| !values.is_empty())
        || seeds
            .paths
            .as_ref()
            .is_some_and(|values| !values.is_empty())
        || seeds
            .routes
            .as_ref()
            .is_some_and(|values| !values.is_empty())
        || seeds
            .tests
            .as_ref()
            .is_some_and(|values| !values.is_empty())
        || seeds.located.is_some()
        || seeds.change.is_some()
        || seeds.plan.is_some()
}

/// Validates the safe query AST: bounded depth, allow-listed operators, static
/// cost within the hard ceiling, and within any client-supplied cost limit.
fn advanced_invariants_are_valid(input: &QueryAdvancedInput) -> bool {
    let mut operators = Vec::new();
    let mut depth = 0usize;
    collect_ast_operators(&input.query, &mut operators, &mut depth, 1);
    let max_rows = usize::from(input.max_results.unwrap_or(100));
    let Ok(plan) = AdvancedQueryPlan::validate(&operators, max_rows, MAX_ADVANCED_TRAVERSAL, depth)
    else {
        return false;
    };
    input
        .cost_limit
        .is_none_or(|limit| plan.estimated_cost <= limit)
}

/// Walks the query AST, recording the operator sequence and maximum nesting
/// depth used for static cost estimation.
fn collect_ast_operators(
    node: &QueryAstNode,
    operators: &mut Vec<QueryOperator>,
    depth: &mut usize,
    current: usize,
) {
    *depth = (*depth).max(current);
    let (operator, children): (QueryOperator, Vec<&QueryAstNode>) = match node {
        QueryAstNode::Scan { .. } => (QueryOperator::Scan, Vec::new()),
        QueryAstNode::Filter { input, .. } => (QueryOperator::Filter, vec![input]),
        QueryAstNode::Project { input, .. } => (QueryOperator::Project, vec![input]),
        QueryAstNode::Join { left, right, .. } => (QueryOperator::Join, vec![left, right]),
        QueryAstNode::Aggregate { input, .. } => (QueryOperator::Aggregate, vec![input]),
        QueryAstNode::Traverse { .. } => (QueryOperator::Traverse, Vec::new()),
        QueryAstNode::Sort { input, .. } => (QueryOperator::Sort, vec![input]),
        QueryAstNode::Limit { input, .. } => (QueryOperator::Limit, vec![input]),
    };
    operators.push(operator);
    for child in children {
        collect_ast_operators(child, operators, depth, current + 1);
    }
}

/// Maps a public batch subtool to its catalog counterpart.
pub(crate) const fn mcp_tool_for_batch(tool: BatchTool) -> McpTool {
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

/// Validates the public batch invariants: unique operation ids, an acyclic
/// dependency graph of bounded depth, the closed allowlist intersected with the
/// active exposure profile, and bindings that only reference declared
/// dependencies.
fn batch_invariants_are_valid(input: &QueryBatchInput, profile: ExposureProfile) -> bool {
    let operations = &input.operations;

    let mut ids: Vec<&str> = operations.iter().map(|op| op.id.as_str()).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return false;
    }

    let tools: Vec<McpTool> = operations
        .iter()
        .map(|op| mcp_tool_for_batch(op.tool))
        .collect();
    if !tools
        .iter()
        .all(|tool| is_batch_allowed_under_profile(*tool, profile))
    {
        return false;
    }

    let mut dependencies: Vec<Vec<usize>> = Vec::with_capacity(operations.len());
    for operation in operations {
        let mut resolved = Vec::new();
        if let Some(declared) = &operation.depends_on {
            for name in declared {
                let Some(index) = operations.iter().position(|other| other.id == *name) else {
                    return false;
                };
                resolved.push(index);
            }
        }
        dependencies.push(resolved);
    }

    if BatchPlan::validate(&tools, &dependencies).is_err() {
        return false;
    }

    for (index, operation) in operations.iter().enumerate() {
        if !bindings_reference_declared_dependencies(
            &operation.arguments,
            &dependencies[index],
            operations,
        ) {
            return false;
        }
    }

    true
}

/// Checks that every `$from` binding leaf names a declared dependency of its
/// operation. Wildcards and references to undeclared operations are rejected.
fn bindings_reference_declared_dependencies(
    arguments: &Map<String, Value>,
    declared: &[usize],
    operations: &[BatchOperation],
) -> bool {
    let declared_ids: Vec<&str> = declared
        .iter()
        .map(|&index| operations[index].id.as_str())
        .collect();
    let mut stack: Vec<&Value> = arguments.values().collect();
    while let Some(value) = stack.pop() {
        match value {
            Value::Object(map) => {
                if let Some(from) = map.get("$from") {
                    if !from
                        .as_str()
                        .is_some_and(|name| declared_ids.contains(&name))
                    {
                        return false;
                    }
                } else {
                    stack.extend(map.values());
                }
            }
            Value::Array(items) => stack.extend(items),
            _ => {}
        }
    }
    true
}

/// Reports whether a tool name is exposed by the given profile.
///
/// Profile filtering applies only to discovery and invocation authorization.
/// It never changes tool semantics, limits, trust, or permission policy.
fn profile_exposes_tool(profile: ExposureProfile, tool_name: &str) -> bool {
    profile.tools().iter().any(|tool| tool.name() == tool_name)
}

/// Indexes the precomputed discovery payloads by profile privilege rank.
///
/// The order matches [`ExposureProfile::ALL`], so the payload built for each
/// profile lines up with its rank.
const fn profile_index(profile: ExposureProfile) -> usize {
    match profile {
        ExposureProfile::Scout => 0,
        ExposureProfile::Analysis => 1,
        ExposureProfile::Developer => 2,
    }
}

fn tool_argument_bytes_are_valid(tool: VerticalTool, input: &Value) -> bool {
    let maximum = match tool {
        VerticalTool::RepoIndex => MAX_REPO_INDEX_ARGUMENT_BYTES,
        VerticalTool::OperationStatus => MAX_OPERATION_STATUS_ARGUMENT_BYTES,
        VerticalTool::CodeLocate => MAX_CODE_LOCATE_ARGUMENT_BYTES,
        VerticalTool::SymbolExplain => MAX_SYMBOL_EXPLAIN_ARGUMENT_BYTES,
        VerticalTool::SourceRead => MAX_SOURCE_READ_ARGUMENT_BYTES,
        _ => MAX_CODE_LOCATE_ARGUMENT_BYTES,
    };
    serialized_json_fits(input, maximum)
}

fn tool_specific_input_limits_are_valid(tool: VerticalTool, input: &Value) -> bool {
    // JSON Schema maxLength counts characters, while these public contracts
    // bound serialized UTF-8 bytes. The configuration patch is counted without
    // materializing a second attacker-controlled buffer.
    match tool {
        VerticalTool::RepoIndex => {
            input
                .get("root")
                .and_then(Value::as_str)
                .is_none_or(|root| root.len() <= MAX_REPOSITORY_ROOT_BYTES)
                && input
                    .get("configuration_patch")
                    .is_none_or(|patch| serialized_json_fits(patch, MAX_CONFIGURATION_PATCH_BYTES))
        }
        VerticalTool::CodeLocate => input
            .get("query")
            .and_then(Value::as_str)
            .is_some_and(|query| query.len() <= MAX_LOCATE_QUERY_BYTES),
        VerticalTool::RepoStatus
        | VerticalTool::RepoList
        | VerticalTool::OperationStatus
        | VerticalTool::SymbolExplain
        | VerticalTool::SourceRead
        | VerticalTool::SymbolRelationships
        | VerticalTool::FlowTrace
        | VerticalTool::ChangeImpact
        | VerticalTool::TestsSelect
        | VerticalTool::ArchitectureOverview
        | VerticalTool::ArchitectureCycles
        | VerticalTool::CodeDead
        | VerticalTool::HistoryCompare
        | VerticalTool::PlanChange
        | VerticalTool::ContextPack
        | VerticalTool::QueryAdvanced
        | VerticalTool::QueryBatch => true,
    }
}

fn serialized_json_fits<T>(value: &T, maximum: usize) -> bool
where
    T: Serialize + ?Sized,
{
    serde_json::to_writer(ByteLimitWriter { remaining: maximum }, value).is_ok()
}

struct ByteLimitWriter {
    remaining: usize,
}

impl io::Write for ByteLimitWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other("serialized JSON exceeds its byte limit"));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn typed_output_is_valid(tool: VerticalTool, input: &TypedInput, output: &Value) -> bool {
    // The Rust output types also reapply source-free PublicError invariants
    // that intentionally cannot be represented by generated JSON Schema.
    match tool {
        VerticalTool::RepoIndex => RepoIndexOutput::deserialize(output).is_ok(),
        VerticalTool::RepoStatus
        | VerticalTool::RepoList
        | VerticalTool::SymbolRelationships
        | VerticalTool::FlowTrace
        | VerticalTool::ChangeImpact
        | VerticalTool::TestsSelect
        | VerticalTool::ArchitectureOverview
        | VerticalTool::ArchitectureCycles
        | VerticalTool::CodeDead
        | VerticalTool::HistoryCompare
        | VerticalTool::PlanChange
        | VerticalTool::ContextPack
        | VerticalTool::QueryAdvanced
        | VerticalTool::QueryBatch => true,
        VerticalTool::OperationStatus => OperationStatusOutput::deserialize(output).is_ok(),
        VerticalTool::CodeLocate => CodeLocateOutput::deserialize(output).is_ok(),
        VerticalTool::SymbolExplain => SymbolExplainOutput::deserialize(output).is_ok(),
        VerticalTool::SourceRead => {
            let Ok(output) = SourceReadOutput::deserialize(output) else {
                return false;
            };
            let TypedInput::SourceRead(input) = input else {
                return false;
            };
            source_read_output_invariants_are_valid(input, &output)
        }
    }
}

fn source_read_output_invariants_are_valid(
    input: &SourceReadInput,
    output: &SourceReadOutput,
) -> bool {
    let ToolResponse::Success(output) = output else {
        return true;
    };
    if output.trust != TrustClassification::UntrustedRepositoryData
        || output.usage.source_bytes != u64::from(output.data.total_source_bytes)
    {
        return false;
    }

    if let RepositorySelector::ById(selector) = &input.repository
        && selector.repository_id != output.repository.repository_id
    {
        return false;
    }
    if let Some(GenerationSelector::Explicit(generation)) = input.generation.as_ref()
        && *generation != output.generation.generation_id
    {
        return false;
    }

    let requested_source_bytes = input
        .max_source_bytes
        .into_iter()
        .chain(
            input
                .budget
                .as_ref()
                .and_then(|budget| budget.max_source_bytes),
        )
        .min();
    if requested_source_bytes.is_some_and(|maximum| output.data.total_source_bytes > maximum) {
        return false;
    }

    output.data.chunks.iter().all(|chunk| {
        chunk.source_ref.repository() == output.repository.repository_id
            && chunk.source_ref.generation() == output.generation.generation_id
            && chunk.trust == output.trust
    })
}

fn list_params_are_valid(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return true;
    };
    let Some(params) = params.as_object() else {
        return false;
    };
    params.keys().all(|key| key == "_meta") && params.get("_meta").is_none_or(request_meta_is_valid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallParamsError {
    Invalid,
    TaskUnsupported,
}

fn decode_call_params(
    params: Option<Value>,
) -> Result<(String, Map<String, Value>), CallParamsError> {
    let Some(Value::Object(mut params)) = params else {
        return Err(CallParamsError::Invalid);
    };
    if params
        .keys()
        .any(|key| !matches!(key.as_str(), "_meta" | "name" | "arguments" | "task"))
        || params
            .get("_meta")
            .is_some_and(|value| !request_meta_is_valid(value))
    {
        return Err(CallParamsError::Invalid);
    }
    if params.contains_key("task") {
        return Err(CallParamsError::TaskUnsupported);
    }
    let Some(Value::String(name)) = params.remove("name") else {
        return Err(CallParamsError::Invalid);
    };
    if name.is_empty() || name.len() > MAX_TOOL_NAME_BYTES {
        return Err(CallParamsError::Invalid);
    }
    let arguments = match params.remove("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments,
        Some(_) => return Err(CallParamsError::Invalid),
    };
    Ok((name, arguments))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolResultError {
    Limit,
    Serialize,
}

fn tool_success(structured: Value) -> Result<HandlerResponse, ToolResultError> {
    tool_result(structured, false)
}

fn tool_error(
    contract: &ToolContract,
    error: PublicError,
) -> Result<HandlerResponse, ToolResultError> {
    let structured = serde_json::to_value(ErrorResponse {
        schema_version: SchemaVersion::V1_0,
        error,
    })
    .map_err(|_| ToolResultError::Serialize)?;
    if !contract.output_validator.is_valid(&structured)
        || !typed_error_output_is_valid(contract.tool, &structured)
    {
        return Err(ToolResultError::Serialize);
    }
    tool_result(structured, true)
}

fn typed_error_output_is_valid(tool: VerticalTool, output: &Value) -> bool {
    match tool {
        VerticalTool::RepoIndex => RepoIndexOutput::deserialize(output).is_ok(),
        VerticalTool::RepoStatus
        | VerticalTool::RepoList
        | VerticalTool::SymbolRelationships
        | VerticalTool::FlowTrace
        | VerticalTool::ChangeImpact
        | VerticalTool::TestsSelect
        | VerticalTool::ArchitectureOverview
        | VerticalTool::ArchitectureCycles
        | VerticalTool::CodeDead
        | VerticalTool::HistoryCompare
        | VerticalTool::PlanChange
        | VerticalTool::ContextPack
        | VerticalTool::QueryAdvanced
        | VerticalTool::QueryBatch => true,
        VerticalTool::OperationStatus => OperationStatusOutput::deserialize(output).is_ok(),
        VerticalTool::CodeLocate => CodeLocateOutput::deserialize(output).is_ok(),
        VerticalTool::SymbolExplain => SymbolExplainOutput::deserialize(output).is_ok(),
        VerticalTool::SourceRead => SourceReadOutput::deserialize(output).is_ok(),
    }
}

fn tool_result(structured: Value, is_error: bool) -> Result<HandlerResponse, ToolResultError> {
    // The conservative one-third cap accounts for the structured object, its
    // text mirror, worst-case JSON string escaping, and the JSON-RPC ID reserve.
    if !serialized_json_fits(&structured, MAX_TOOL_STRUCTURED_BYTES) {
        return Err(ToolResultError::Limit);
    }
    let text = serde_json::to_string(&structured).map_err(|_| ToolResultError::Serialize)?;
    let result = Map::from_iter([
        ("content".to_owned(), text_content(text)),
        ("structuredContent".to_owned(), structured),
        ("isError".to_owned(), Value::Bool(is_error)),
    ]);
    if !serialized_json_fits(&result, MAX_TOOL_RESULT_BYTES) {
        return Err(ToolResultError::Limit);
    }
    Ok(HandlerResponse::Success(result))
}

fn text_content(text: String) -> Value {
    json!([{"type": "text", "text": text}])
}

fn cancel_or(cancellation: &RequestCancellation, response: HandlerResponse) -> HandlerResponse {
    if cancellation.is_cancelled() {
        HandlerResponse::Cancelled
    } else {
        response
    }
}

const fn internal_tool_error(message: &'static str) -> HandlerResponse {
    HandlerResponse::error(INTERNAL_ERROR, message)
}

fn checked_public_error(
    code: ErrorCode,
    message: &'static str,
    field: &'static str,
) -> Result<PublicError, ToolRegistryError> {
    let field = DetailKey::parse(field).map_err(ToolRegistryError::BuildPublicError)?;
    PublicError::builder(code, message)
        .next_action(NextAction::CorrectField { field })
        .build()
        .map_err(ToolRegistryError::BuildPublicError)
}

/// Failure while constructing the server-owned tool registry.
#[derive(Debug, Error)]
pub enum ToolRegistryError {
    /// A checked schema artifact is not valid JSON object syntax.
    #[error("checked MCP {direction} schema for {tool:?} is invalid")]
    ParseSchema {
        /// Affected tool.
        tool: VerticalTool,
        /// Input or output.
        direction: &'static str,
        /// JSON parse failure.
        #[source]
        source: serde_json::Error,
    },
    /// A checked schema artifact is not valid JSON Schema 2020-12.
    #[error("checked MCP {direction} schema for {tool:?} does not compile: {detail}")]
    CompileSchema {
        /// Affected tool.
        tool: VerticalTool,
        /// Input or output.
        direction: &'static str,
        /// Source-free compiler detail for server diagnostics.
        detail: String,
    },
    /// A tool definition could not be serialized.
    #[error("MCP tool definition serialization failed")]
    SerializeDefinition(#[source] serde_json::Error),
    /// A bounded registry allocation could not be reserved.
    #[error("MCP tool registry memory is unavailable")]
    MemoryUnavailable,
    /// A built-in checked public error could not be constructed.
    #[error("built-in MCP public error is invalid")]
    BuildPublicError(#[source] rootlight_mcp_contract::PublicErrorBuildError),
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use tokio::sync::watch;

    use super::*;
    use crate::{RequestCancellation, RequestId};

    #[derive(Debug, Default)]
    struct FixtureExecutor {
        calls: AtomicUsize,
    }

    impl ToolExecutor for FixtureExecutor {
        fn execute(
            &self,
            tool: VerticalTool,
            _arguments: Map<String, Value>,
            _cancellation: RequestCancellation,
        ) -> ToolExecutionFuture {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                if tool == VerticalTool::RepoIndex {
                    let Value::Object(output) = json!({
                        "schema_version": "1.0",
                        "data": {
                            "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v",
                            "operation_id": "op1_aaaaaaaaaaaaaaaaaaaaaaaaadujjxgv",
                            "accepted_plan": {
                                "scope": "repository",
                                "mode": "auto",
                                "providers": [],
                                "parent_generation": null,
                                "estimated_disk_bytes": 0
                            },
                            "state": "queued",
                            "published_generation": null,
                            "diagnostics": []
                        }
                    }) else {
                        panic!("fixture output is an object");
                    };
                    Ok(output)
                } else {
                    Ok(Map::new())
                }
            })
        }
    }

    #[derive(Debug, Clone)]
    struct StaticExecutor {
        result: Result<Map<String, Value>, ToolExecutionError>,
    }

    impl ToolExecutor for StaticExecutor {
        fn execute(
            &self,
            _tool: VerticalTool,
            _arguments: Map<String, Value>,
            _cancellation: RequestCancellation,
        ) -> ToolExecutionFuture {
            let result = self.result.clone();
            Box::pin(async move { result })
        }
    }

    #[derive(Debug)]
    struct CancellingExecutor {
        sender: watch::Sender<bool>,
        error: ToolExecutionError,
    }

    impl ToolExecutor for CancellingExecutor {
        fn execute(
            &self,
            _tool: VerticalTool,
            _arguments: Map<String, Value>,
            _cancellation: RequestCancellation,
        ) -> ToolExecutionFuture {
            let sender = self.sender.clone();
            let error = self.error.clone();
            Box::pin(async move {
                let _ = sender.send(true);
                Err(error)
            })
        }
    }

    fn cancellation() -> RequestCancellation {
        let (_sender, receiver) = watch::channel(false);
        RequestCancellation { receiver }
    }

    fn cancelled() -> RequestCancellation {
        let (_sender, receiver) = watch::channel(true);
        RequestCancellation { receiver }
    }

    fn request(method: &str, params: Value) -> OperatingRequest {
        OperatingRequest {
            id: RequestId::Number(serde_json::Number::from(1)),
            method: method.to_owned(),
            params: Some(params),
        }
    }

    fn success(response: HandlerResponse) -> Map<String, Value> {
        match response {
            HandlerResponse::Success(result) => result,
            other => panic!("expected success, got {other:?}"),
        }
    }

    fn retained_output(name: &str) -> Map<String, Value> {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
        ))
        .expect("retained tool contracts are valid JSON");
        fixture["tools"]
            .as_array()
            .expect("tool contracts contain an array")
            .iter()
            .find(|entry| entry["tool"] == name)
            .unwrap_or_else(|| panic!("retained tool contract {name} exists"))["output"]
            .as_object()
            .expect("retained output is an object")
            .clone()
    }

    fn checked_not_found() -> PublicError {
        PublicError::builder(ErrorCode::NotFound, "requested entity was not found")
            .build()
            .expect("test public error is checked")
    }

    #[tokio::test]
    async fn tools_list_is_fixed_strict_and_truthfully_annotated() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        assert_eq!(router.capabilities(), HandlerCapabilities::tools());

        let response = router
            .handle(request("tools/list", json!({})), cancellation())
            .await;
        let result = success(response);
        assert!(
            serde_json::to_vec(&result)
                .expect("tool catalog serializes")
                .len()
                < crate::DEFAULT_MAX_RESPONSE_BYTES
        );
        let tools = result["tools"].as_array().expect("tools is an array");
        assert_eq!(tools.len(), VerticalTool::ALL.len());
        assert_eq!(tools[0]["name"], "repo.index");
        assert_eq!(tools[16]["name"], "source.read");
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["outputSchema"]["type"], "object");
            assert_eq!(tool["annotations"]["openWorldHint"], false);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
            assert_eq!(tool["execution"]["taskSupport"], "forbidden");
        }
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
        assert_eq!(tools[2]["annotations"]["readOnlyHint"], true);
        let operation_status = tools
            .iter()
            .find(|tool| tool["name"] == "operation.status")
            .expect("operation.status is listed");
        assert_eq!(
            operation_status["annotations"]["readOnlyHint"], false,
            "operation.status can cancel and must not be advertised as read-only"
        );
    }

    #[tokio::test]
    async fn tools_call_validates_output_and_mirrors_exact_structured_content() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "repo.index", "arguments": {"root": "C:/fixture"}}),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("text mirror exists");
        let mirror: Value = serde_json::from_str(text).expect("text mirror is JSON");
        assert_eq!(mirror, result["structuredContent"]);
    }

    #[tokio::test]
    async fn invalid_tool_arguments_are_model_visible_without_execution() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.index",
                        "arguments": {
                            "root": "C:/fixture",
                            "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"]
            .as_str()
            .expect("error text mirror exists");
        let mirror: Value = serde_json::from_str(text).expect("error text mirror is JSON");
        assert_eq!(mirror, result["structuredContent"]);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "INVALID_ARGUMENT"
        );
        assert_eq!(result["structuredContent"]["schema_version"], "1.0");
        serde_json::from_value::<RepoIndexOutput>(result["structuredContent"].clone())
            .expect("invalid input uses the advertised checked error envelope");
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn executor_domain_errors_use_the_checked_advertised_contract() {
        let router = ToolRouter::new(
            StaticExecutor {
                result: Err(ToolExecutionError::new(checked_not_found())),
            },
            ExposureProfile::Developer,
        )
        .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "operation.status",
                        "arguments": {
                            "operation_id": "op1_aaaaaaaaaaaaaaaaaaaaaaaaadujjxgv"
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        assert_eq!(result["isError"], true);
        assert_eq!(result["structuredContent"]["schema_version"], "1.0");
        assert_eq!(result["structuredContent"]["error"]["code"], "NOT_FOUND");
        serde_json::from_value::<OperationStatusOutput>(result["structuredContent"].clone())
            .expect("domain error uses the advertised typed envelope");
        let contract =
            ToolContract::compile(VerticalTool::OperationStatus).expect("contract compiles");
        assert!(
            contract
                .output_validator
                .is_valid(&result["structuredContent"])
        );
    }

    #[tokio::test]
    async fn semantic_source_range_failure_does_not_reach_the_executor() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "source.read",
                        "arguments": {
                            "repository": {
                                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                            },
                            "references": [{
                                "source_ref": {
                                    "repository": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v",
                                    "generation": "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by",
                                    "span": {
                                        "file": "file1_cukrkfivcukrkfivcukrkfivcukrkfivpyrmidq",
                                        "start_byte": 9,
                                        "end_byte": 4
                                    },
                                    "content_hash": "b3_rc6zkrxh5srdoiia2cydtoqh5ug2jyctujxicstuvgf2yz377y5zl6hbcu"
                                }
                            }]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;

        let result = success(response);
        assert_eq!(result["isError"], true);
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn inverted_direct_file_range_does_not_reach_the_executor() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "source.read",
                        "arguments": {
                            "repository": {
                                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                            },
                            "references": [{
                                "file_id": "file1_cukrkfivcukrkfivcukrkfivcukrkfivpyrmidq",
                                "start_byte": 9,
                                "end_byte": 4
                            }]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;

        assert_eq!(success(response)["isError"], true);
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn utf8_byte_limit_failures_do_not_reach_the_executor() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let oversized_root = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.index",
                        "arguments": {"root": "é".repeat(2_049)}
                    }),
                ),
                cancellation(),
            )
            .await;
        let oversized_query = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "code.locate",
                        "arguments": {
                            "repository": {
                                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                            },
                            "query": "é".repeat(1_025)
                        }
                    }),
                ),
                cancellation(),
            )
            .await;

        assert_eq!(success(oversized_root)["isError"], true);
        assert_eq!(success(oversized_query)["isError"], true);
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn oversized_configuration_patch_does_not_reach_the_executor() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.index",
                        "arguments": {
                            "root": "C:/fixture",
                            "configuration_patch": {
                                "entry": "a".repeat(MAX_CONFIGURATION_PATCH_BYTES)
                            }
                        }
                    }),
                ),
                cancellation(),
            )
            .await;

        assert_eq!(success(response)["isError"], true);
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn exact_configuration_patch_byte_limit_is_valid() {
        let framing_bytes = br#"{"entry":""}"#.len();
        let input = json!({
            "root": "C:/fixture",
            "configuration_patch": {
                "entry": "a".repeat(MAX_CONFIGURATION_PATCH_BYTES - framing_bytes)
            }
        });

        assert!(tool_specific_input_limits_are_valid(
            VerticalTool::RepoIndex,
            &input
        ));
        assert_eq!(
            serde_json::to_vec(&input["configuration_patch"])
                .expect("configuration patch serializes")
                .len(),
            MAX_CONFIGURATION_PATCH_BYTES
        );
    }

    #[tokio::test]
    async fn unknown_tool_is_an_invalid_params_protocol_error_without_execution() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "unknown.tool", "arguments": {}}),
                ),
                cancellation(),
            )
            .await;

        assert!(matches!(
            response,
            HandlerResponse::Error {
                code: INVALID_PARAMS,
                ..
            }
        ));
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn progress_tokens_and_forbidden_tasks_share_transport_validation() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let list = router
            .handle(
                request(
                    "tools/list",
                    json!({"_meta": {"progressToken": 7, "vendor.example/trace": true}}),
                ),
                cancellation(),
            )
            .await;
        assert!(matches!(list, HandlerResponse::Success(_)));

        let invalid_meta = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.index",
                        "arguments": {"root": "C:/fixture"},
                        "_meta": {"progressToken": {}}
                    }),
                ),
                cancellation(),
            )
            .await;
        assert!(matches!(
            invalid_meta,
            HandlerResponse::Error {
                code: INVALID_PARAMS,
                ..
            }
        ));

        let task = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.index",
                        "arguments": {"root": "C:/fixture"},
                        "task": {"ttl": 1000}
                    }),
                ),
                cancellation(),
            )
            .await;
        assert!(matches!(
            task,
            HandlerResponse::Error {
                code: METHOD_NOT_FOUND,
                ..
            }
        ));
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn cancellation_wins_entry_and_post_execution_early_responses() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(request("tools/call", Value::Null), cancelled())
            .await;
        assert!(matches!(response, HandlerResponse::Cancelled));
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);

        let (sender, receiver) = watch::channel(false);
        let router = ToolRouter::new(
            CancellingExecutor {
                sender,
                error: ToolExecutionError::new(checked_not_found()),
            },
            ExposureProfile::Developer,
        )
        .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "repo.index", "arguments": {"root": "C:/fixture"}}),
                ),
                RequestCancellation { receiver },
            )
            .await;
        assert!(matches!(response, HandlerResponse::Cancelled));
    }

    #[test]
    fn typed_output_validation_rejects_source_shaped_public_errors() {
        let contract =
            ToolContract::compile(VerticalTool::OperationStatus).expect("contract compiles");
        let output = json!({
            "schema_version": "1.0",
            "data": {
                "operation": {
                    "kind": "repository_index",
                    "state": "failed",
                    "stage": "failed",
                    "progress": {
                        "completed_units": 0,
                        "total_units": null
                    },
                    "revision": 1,
                    "started_at": "2026-07-18T00:00:00Z",
                    "resources": {
                        "peak_rss_bytes": 0,
                        "written_bytes": 0,
                        "files_examined": 0
                    }
                },
                "published_generation": null,
                "error": {
                    "code": "INTERNAL",
                    "message": "C:\\Users\\person\\secret.rs",
                    "retryable": false,
                    "retry_after_ms": null,
                    "repository": null,
                    "operation": null,
                    "generation": null,
                    "details": {},
                    "next_actions": []
                },
                "retry_after_ms": null
            }
        });

        assert!(contract.output_validator.is_valid(&output));
        assert!(!typed_output_is_valid(
            VerticalTool::OperationStatus,
            &TypedInput::Other,
            &output
        ));
    }

    #[test]
    fn repo_index_fixture_decodes_as_the_typed_output() {
        serde_json::from_value::<RepoIndexOutput>(json!({
            "schema_version": "1.0",
            "data": {
                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v",
                "operation_id": "op1_aaaaaaaaaaaaaaaaaaaaaaaaadujjxgv",
                "accepted_plan": {
                    "scope": "repository",
                    "mode": "auto",
                    "providers": [],
                    "parent_generation": null,
                    "estimated_disk_bytes": 0
                },
                "state": "queued",
                "published_generation": null,
                "diagnostics": []
            }
        }))
        .expect("fixture satisfies the typed repo.index output");
    }

    #[tokio::test]
    async fn invalid_server_output_fails_as_a_protocol_internal_error() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "source.read",
                        "arguments": {
                            "repository": {
                                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                            },
                            "references": [{
                                "symbol_id": "sym1_cecigxytq5fdpxizkjlxeqzrbmtnd2odobb4eey"
                            }]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        assert!(matches!(
            response,
            HandlerResponse::Error {
                code: INTERNAL_ERROR,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn source_aggregate_mismatch_is_a_protocol_internal_error() {
        let mut output = retained_output("source.read");
        output["data"]["total_source_bytes"] = json!(9);
        let router = ToolRouter::new(
            StaticExecutor { result: Ok(output) },
            ExposureProfile::Developer,
        )
        .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "source.read",
                        "arguments": {
                            "repository": {
                                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                            },
                            "references": [{
                                "file_id": "file1_cukrkfivcukrkfivcukrkfivcukrkfivpyrmidq",
                                "start_byte": 0,
                                "end_byte": 10
                            }]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        assert!(matches!(
            response,
            HandlerResponse::Error {
                code: INTERNAL_ERROR,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn oversized_valid_source_output_becomes_a_bounded_tool_error() {
        let mut output = retained_output("source.read");
        let source_bytes = 200_000usize;
        output["data"]["chunks"][0]["content"] = json!("\"".repeat(source_bytes));
        output["data"]["chunks"][0]["end_byte"] = json!(source_bytes);
        output["data"]["chunks"][0]["source_ref"]["span"]["end_byte"] = json!(source_bytes);
        output["data"]["total_source_bytes"] = json!(source_bytes);
        output["usage"]["source_bytes"] = json!(source_bytes);
        assert!(
            !serialized_json_fits(&Value::Object(output.clone()), MAX_TOOL_STRUCTURED_BYTES),
            "fixture crosses the mirror-safe structured budget"
        );
        serde_json::from_value::<SourceReadOutput>(Value::Object(output.clone()))
            .expect("oversized fixture remains a valid typed source response");

        let router = ToolRouter::new(
            StaticExecutor { result: Ok(output) },
            ExposureProfile::Developer,
        )
        .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "source.read",
                        "arguments": {
                            "repository": {
                                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                            },
                            "references": [{
                                "file_id": "file1_cukrkfivcukrkfivcukrkfivcukrkfivpyrmidq",
                                "start_byte": 0,
                                "end_byte": 200000
                            }]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        assert_eq!(result["isError"], true);
        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "RESOURCE_EXHAUSTED"
        );
        serde_json::from_value::<SourceReadOutput>(result["structuredContent"].clone())
            .expect("resource error uses the source.read output contract");
        assert!(serialized_json_fits(&result, MAX_TOOL_RESULT_BYTES));
        let mirror: Value = serde_json::from_str(
            result["content"][0]["text"]
                .as_str()
                .expect("tool error has a text mirror"),
        )
        .expect("tool error mirror is JSON");
        assert_eq!(mirror, result["structuredContent"]);
    }

    fn listed_tool_names(result: &Map<String, Value>) -> Vec<&str> {
        result["tools"]
            .as_array()
            .expect("tools/list returns an array")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool has a name"))
            .collect()
    }

    #[tokio::test]
    async fn scout_session_tools_list_exposes_exactly_the_six_scout_tools() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Scout)
            .expect("registry compiles");
        let response = router
            .handle(request("tools/list", json!({})), cancellation())
            .await;
        assert_eq!(
            listed_tool_names(&success(response)),
            [
                "repo.status",
                "code.locate",
                "symbol.explain",
                "context.pack",
                "source.read",
                "query.batch",
            ]
        );
    }

    #[tokio::test]
    async fn scout_session_rejects_a_developer_only_call() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Scout)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "query.advanced", "arguments": {}}),
                ),
                cancellation(),
            )
            .await;
        assert!(matches!(
            response,
            HandlerResponse::Error {
                code: INVALID_PARAMS,
                ..
            }
        ));
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn negotiated_profile_change_updates_tools_list() {
        let (sender, receiver) = watch::channel(ExposureProfile::Developer);
        let router = ToolRouter::with_shared_profile(
            FixtureExecutor::default(),
            receiver,
            ExposureProfile::Developer,
        )
        .expect("registry compiles");

        let developer = router
            .handle(request("tools/list", json!({})), cancellation())
            .await;
        assert_eq!(listed_tool_names(&success(developer)).len(), 19);

        // A negotiated profile change is observed without recompiling contracts.
        sender.send_replace(ExposureProfile::Scout);
        let scout = router
            .handle(request("tools/list", json!({})), cancellation())
            .await;
        assert_eq!(listed_tool_names(&success(scout)).len(), 6);
    }

    #[tokio::test]
    async fn ceiling_clamps_a_higher_negotiated_profile() {
        // The shared state holds Developer, but the server policy ceiling is
        // Scout; discovery must never widen past the ceiling.
        let (_sender, receiver) = watch::channel(ExposureProfile::Developer);
        let router = ToolRouter::with_shared_profile(
            FixtureExecutor::default(),
            receiver,
            ExposureProfile::Scout,
        )
        .expect("registry compiles");
        let response = router
            .handle(request("tools/list", json!({})), cancellation())
            .await;
        assert_eq!(listed_tool_names(&success(response)).len(), 6);
    }

    use rootlight_mcp_contract::context::{ContextSeedSelector, QueryPredicate, QueryValue};
    use rootlight_mcp_contract::vertical::{EntityKind, RepositoryIdSelector};

    fn selector() -> RepositorySelector {
        RepositorySelector::ById(RepositoryIdSelector {
            repository_id: "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
                .parse()
                .expect("valid repository id"),
        })
    }

    fn batch_operation(id: &str, tool: BatchTool, depends_on: Option<Vec<&str>>) -> BatchOperation {
        BatchOperation {
            id: id.to_owned(),
            tool,
            depends_on: depends_on.map(|names| names.into_iter().map(str::to_owned).collect()),
            arguments: Map::new(),
            local_budget: None,
        }
    }

    fn batch_input(operations: Vec<BatchOperation>) -> QueryBatchInput {
        QueryBatchInput {
            repository: selector(),
            generation: None,
            operations,
            failure_policy: None,
            budget: None,
            response_profile: None,
        }
    }

    #[test]
    fn batch_with_unique_ids_and_acyclic_graph_is_valid() {
        let input = batch_input(vec![
            batch_operation("find", BatchTool::CodeLocate, None),
            batch_operation("explain", BatchTool::SymbolExplain, Some(vec!["find"])),
        ]);
        assert!(batch_invariants_are_valid(
            &input,
            ExposureProfile::Developer
        ));
    }

    #[test]
    fn batch_with_duplicate_operation_ids_is_rejected() {
        let input = batch_input(vec![
            batch_operation("dup", BatchTool::CodeLocate, None),
            batch_operation("dup", BatchTool::SymbolExplain, None),
        ]);
        assert!(!batch_invariants_are_valid(
            &input,
            ExposureProfile::Developer
        ));
    }

    #[test]
    fn batch_with_dependency_cycle_is_rejected() {
        let input = batch_input(vec![
            batch_operation("a", BatchTool::CodeLocate, Some(vec!["b"])),
            batch_operation("b", BatchTool::SymbolExplain, Some(vec!["a"])),
        ]);
        assert!(!batch_invariants_are_valid(
            &input,
            ExposureProfile::Developer
        ));
    }

    #[test]
    fn batch_with_unknown_dependency_is_rejected() {
        let input = batch_input(vec![batch_operation(
            "a",
            BatchTool::CodeLocate,
            Some(vec!["missing"]),
        )]);
        assert!(!batch_invariants_are_valid(
            &input,
            ExposureProfile::Developer
        ));
    }

    #[test]
    fn batch_subtool_hidden_by_profile_is_rejected() {
        // symbol.relationships is batch-allowed but not exposed under scout.
        let input = batch_input(vec![batch_operation(
            "rel",
            BatchTool::SymbolRelationships,
            None,
        )]);
        assert!(!batch_invariants_are_valid(&input, ExposureProfile::Scout));
        assert!(batch_invariants_are_valid(
            &input,
            ExposureProfile::Developer
        ));
    }

    #[test]
    fn batch_binding_must_reference_declared_dependency() {
        let mut arguments = Map::new();
        arguments.insert(
            "symbol_ids".to_owned(),
            json!([{ "$from": "find", "pointer": "/data/matches/0/symbol_id" }]),
        );
        let mut declared = batch_operation("explain", BatchTool::SymbolExplain, Some(vec!["find"]));
        declared.arguments = arguments.clone();
        let valid = batch_input(vec![
            batch_operation("find", BatchTool::CodeLocate, None),
            declared,
        ]);
        assert!(batch_invariants_are_valid(
            &valid,
            ExposureProfile::Developer
        ));

        let mut undeclared = batch_operation("explain", BatchTool::SymbolExplain, None);
        undeclared.arguments = arguments;
        let invalid = batch_input(vec![
            batch_operation("find", BatchTool::CodeLocate, None),
            undeclared,
        ]);
        assert!(!batch_invariants_are_valid(
            &invalid,
            ExposureProfile::Developer
        ));
    }

    fn advanced_input(query: QueryAstNode) -> QueryAdvancedInput {
        QueryAdvancedInput {
            repository: selector(),
            generation: None,
            query,
            parameters: None,
            explain: None,
            max_results: None,
            max_depth: None,
            cost_limit: None,
            cursor: None,
        }
    }

    fn scan() -> QueryAstNode {
        QueryAstNode::Scan {
            entity: EntityKind::Function,
            filter: None,
        }
    }

    fn nested_filters(depth: usize) -> QueryAstNode {
        let mut node = scan();
        for _ in 0..depth {
            node = QueryAstNode::Filter {
                input: Box::new(node),
                predicate: QueryPredicate::Equals {
                    field: "name".to_owned(),
                    value: QueryValue::Boolean(true),
                },
            };
        }
        node
    }

    #[test]
    fn advanced_simple_scan_is_valid() {
        assert!(advanced_invariants_are_valid(&advanced_input(scan())));
    }

    #[test]
    fn advanced_ast_exceeding_max_depth_is_rejected() {
        // Scan plus five filters nests to depth six, above the ceiling of five.
        assert!(!advanced_invariants_are_valid(&advanced_input(
            nested_filters(5)
        )));
        assert!(advanced_invariants_are_valid(&advanced_input(
            nested_filters(4)
        )));
    }

    #[test]
    fn advanced_cost_limit_bounds_the_static_estimate() {
        let mut tight = advanced_input(scan());
        tight.cost_limit = Some(1);
        assert!(!advanced_invariants_are_valid(&tight));
        let mut generous = advanced_input(scan());
        generous.cost_limit = Some(1_000);
        assert!(advanced_invariants_are_valid(&generous));
    }

    fn pack_input(seeds: ContextSeedSelector) -> ContextPackInput {
        ContextPackInput {
            repository: selector(),
            generation: None,
            task: "fix duplicate payment creation".to_owned(),
            seeds,
            token_budget: 4500,
            source_policy: None,
            sections: None,
            diversity: None,
            min_confidence: None,
            continuation: None,
            explain: None,
        }
    }

    #[test]
    fn context_pack_requires_at_least_one_seed() {
        let empty = ContextSeedSelector {
            symbols: None,
            paths: None,
            routes: None,
            tests: None,
            located: None,
            change: None,
            plan: None,
        };
        assert!(!context_pack_invariants_are_valid(&pack_input(empty)));

        let symbol = ContextSeedSelector {
            symbols: Some(vec![
                "sym1_cecigxytq5fdpxizkjlxeqzrbmtnd2odobb4eey"
                    .parse()
                    .expect("valid symbol id"),
            ]),
            paths: None,
            routes: None,
            tests: None,
            located: None,
            change: None,
            plan: None,
        };
        assert!(context_pack_invariants_are_valid(&pack_input(symbol)));
    }
}
