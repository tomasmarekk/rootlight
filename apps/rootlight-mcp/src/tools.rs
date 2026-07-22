//! Bounded MCP tool discovery and invocation routing.
//!
//! This module validates both sides of the generic daemon executor boundary
//! and keeps MCP structured content identical to its JSON text mirror.

use std::{fmt, future::Future, io, pin::Pin, sync::Arc};

use jsonschema::{Validator, error::ValidationErrorKind};
use rootlight_mcp_contract::{
    CodeLocateInput, CodeLocateOutput, ContinuationCursor, DetailKey, ErrorCode, ErrorResponse,
    ExposureProfile, GenerationSelector, McpTool, OperationStatusInput, OperationStatusOutput,
    PublicError, PublicErrorBuildError, PublicValue, RepoIndexInput, RepoIndexOutput,
    RepositorySelector, SafeLabel, SchemaVersion, SourceReadInput, SourceReadOutput,
    SymbolExplainInput, SymbolExplainOutput, ToolResponse, TrustClassification, VerticalTool,
    capability::{
        CapabilityStatus, DISCOVERY_METADATA_KEY, ToolCapability, capability_for,
        discovery_metadata,
    },
    context::{BatchOperation, ContextPackInput, QueryAdvancedInput, QueryBatchInput},
    pagination::AuthenticatedCursor,
    repository::RepoListInput,
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
use crate::advanced::{AdvancedQueryPlan, MAX_ADVANCED_TRAVERSAL};
use crate::batch::{
    BatchPlan, is_batch_allowed, is_batch_allowed_under_profile, mcp_tool_for_batch,
};
use crate::error_mapping::{MappedDomainFailure, public_error, public_error_with_details};

#[cfg(test)]
use rootlight_mcp_contract::context::{BatchTool, QueryAstNode};

const INTERNAL_ERROR: i32 = -32_603;
const MAX_TOOL_NAME_BYTES: usize = 128;
const MAX_REPOSITORY_ROOT_BYTES: usize = 4_096;
const MAX_CONFIGURATION_PATCH_BYTES: usize = 64 * 1_024;
const MAX_LOCATE_QUERY_BYTES: usize = 2_048;
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
        exposure_profile: ExposureProfile,
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
        let invalid_arguments = public_error(MappedDomainFailure::invalid_argument("arguments"))
            .map_err(ToolRegistryError::BuildPublicError)?;
        let resource_exhausted = public_error(MappedDomainFailure::resource_exhausted())
            .map_err(ToolRegistryError::BuildPublicError)?;
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
        let typed_input = match validate_contract_input(contract, &arguments_value, profile) {
            Ok(input) => input,
            Err(error) => {
                let error = match error {
                    MaterializedInputError::Invalid { .. } => invalid_arguments,
                    MaterializedInputError::Public(error) => *error,
                };
                return cancel_or(
                    &cancellation,
                    tool_error(contract, error)
                        .unwrap_or_else(|_| internal_tool_error("tool error validation failed")),
                );
            }
        };
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
            .execute(contract.tool, arguments, profile, cancellation.clone())
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

/// Controls how unresolved batch bindings participate in capability admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityBindingPolicy {
    /// Validates concrete input values after bindings have been materialized.
    Materialized,
    /// Rejects a binding when its eventual value could select a restricted rule.
    RejectUnprovenRestrictedBindings,
}

/// Stable classification for a rejected capability field or value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapabilityRejectionReason {
    /// A field is declared unsupported.
    UnsupportedField,
    /// One concrete field value is declared unsupported.
    UnsupportedValue,
    /// A field is blocked pending a complete implementation.
    BlockedField,
    /// Individually supported values cannot be combined in one request.
    UnsupportedCombination,
    /// An unresolved binding could select a restricted value.
    UnprovenBoundValue,
    /// A binding reached validation before it was materialized.
    UnresolvedBinding,
}

impl CapabilityRejectionReason {
    const fn public_name(self) -> &'static str {
        match self {
            Self::UnsupportedField => "unsupported_field",
            Self::UnsupportedValue => "unsupported_value",
            Self::BlockedField => "blocked_field",
            Self::UnsupportedCombination => "unsupported_combination",
            Self::UnprovenBoundValue => "unproven_bound_value",
            Self::UnresolvedBinding => "unresolved_binding",
        }
    }
}

/// Source-free capability rejection produced before tool execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CapabilityAdmissionError {
    code: ErrorCode,
    registry_path: String,
    instance_path: String,
    reason: CapabilityRejectionReason,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "accessors support static batch preflight diagnostics and focused tests"
    )
)]
impl CapabilityAdmissionError {
    /// Returns the stable public error code selected by the capability registry.
    #[must_use]
    pub(crate) const fn code(&self) -> ErrorCode {
        self.code
    }

    /// Returns the generated-schema path used to resolve the capability rule.
    #[must_use]
    pub(crate) fn registry_path(&self) -> &str {
        &self.registry_path
    }

    /// Returns the concrete input path, including array indices.
    #[must_use]
    pub(crate) fn instance_path(&self) -> &str {
        &self.instance_path
    }

    /// Returns the stable rejection classification.
    #[must_use]
    pub(crate) const fn reason(&self) -> CapabilityRejectionReason {
        self.reason
    }

    /// Builds the checked public error, optionally beneath a caller-owned path.
    ///
    /// # Errors
    ///
    /// Returns an error if the combined diagnostic path exceeds public bounds.
    pub(crate) fn to_public_error(
        &self,
        path_prefix: Option<&str>,
    ) -> Result<PublicError, PublicErrorBuildError> {
        let field_path = match (path_prefix, self.instance_path.is_empty()) {
            (Some(prefix), false) => format!("{prefix}.{}", self.instance_path),
            (Some(prefix), true) => prefix.to_owned(),
            (None, false) => self.instance_path.clone(),
            (None, true) => "arguments".to_owned(),
        };
        let failure = match self.code {
            ErrorCode::UnsupportedCapability => {
                MappedDomainFailure::unsupported_capability("arguments")
            }
            ErrorCode::OperatorForbidden => MappedDomainFailure::operator_forbidden("arguments"),
            ErrorCode::BindingInvalid => MappedDomainFailure::binding_invalid(),
            _ => unreachable!("capability rules emit only mapped capability error families"),
        };
        public_error_with_details(
            failure,
            [
                (
                    DetailKey::parse("field_path").expect("static detail key is valid"),
                    PublicValue::Label(SafeLabel::parse(&field_path)?),
                ),
                (
                    DetailKey::parse("capability_reason").expect("static detail key is valid"),
                    PublicValue::Label(
                        SafeLabel::parse(self.reason.public_name())
                            .expect("static capability reason is valid"),
                    ),
                ),
            ],
        )
    }
}

#[derive(Debug)]
struct PendingCapabilityValue<'a> {
    value: &'a Value,
    registry_path: String,
    instance_path: String,
}

/// Validates all explicit input leaves against the canonical capability registry.
pub(crate) fn validate_capability_input(
    tool: VerticalTool,
    arguments: &Value,
    binding_policy: CapabilityBindingPolicy,
) -> Result<(), CapabilityAdmissionError> {
    validate_capability_invariants(tool, arguments)?;
    let capability = capability_for(catalog_tool(tool));
    let mut pending = vec![PendingCapabilityValue {
        value: arguments,
        registry_path: String::new(),
        instance_path: String::new(),
    }];

    while let Some(current) = pending.pop() {
        match current.value {
            Value::Object(object) if is_batch_binding(object) => match binding_policy {
                CapabilityBindingPolicy::Materialized => {
                    return Err(CapabilityAdmissionError {
                        code: ErrorCode::BindingInvalid,
                        registry_path: current.registry_path,
                        instance_path: current.instance_path,
                        reason: CapabilityRejectionReason::UnresolvedBinding,
                    });
                }
                CapabilityBindingPolicy::RejectUnprovenRestrictedBindings => {
                    validate_unresolved_binding(capability, &current)?;
                }
            },
            Value::Object(object) if object.is_empty() => {
                validate_capability_leaf(capability, &current, None)?;
            }
            Value::Object(object) => {
                let mut entries: Vec<_> = object.iter().collect();
                entries.sort_unstable_by_key(|(name, _)| *name);
                for (name, value) in entries.into_iter().rev() {
                    pending.push(PendingCapabilityValue {
                        value,
                        registry_path: join_object_path(&current.registry_path, name),
                        instance_path: join_object_path(&current.instance_path, name),
                    });
                }
            }
            Value::Array(items) if items.is_empty() => {
                validate_capability_leaf(capability, &current, None)?;
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate().rev() {
                    pending.push(PendingCapabilityValue {
                        value,
                        registry_path: format!("{}[]", current.registry_path),
                        instance_path: join_object_path(&current.instance_path, &index.to_string()),
                    });
                }
            }
            value => {
                let canonical = canonical_capability_value(value);
                validate_capability_leaf(capability, &current, canonical.as_deref())?;
            }
        }
    }

    Ok(())
}

fn validate_capability_invariants(
    tool: VerticalTool,
    arguments: &Value,
) -> Result<(), CapabilityAdmissionError> {
    if tool == VerticalTool::CodeLocate
        && arguments
            .get("search_modes")
            .and_then(Value::as_array)
            .is_some_and(|modes| {
                modes.len() > 1
                    && modes
                        .iter()
                        .all(|mode| matches!(mode.as_str(), Some("exact" | "lexical")))
            })
    {
        return Err(CapabilityAdmissionError {
            code: ErrorCode::UnsupportedCapability,
            registry_path: "search_modes".to_owned(),
            instance_path: "search_modes".to_owned(),
            reason: CapabilityRejectionReason::UnsupportedCombination,
        });
    }
    Ok(())
}

fn validate_unresolved_binding(
    capability: &ToolCapability,
    current: &PendingCapabilityValue<'_>,
) -> Result<(), CapabilityAdmissionError> {
    let field_rule = capability.disposition(&current.registry_path, None);
    let value_rules: Vec<_> = capability
        .rules
        .iter()
        .filter(|rule| rule.path == current.registry_path && rule.value.is_some())
        .collect();
    let field_is_restricted = matches!(
        field_rule.status,
        CapabilityStatus::UnsupportedStableError | CapabilityStatus::Blocked
    );
    if !value_rules.is_empty()
        && (field_is_restricted
            || value_rules
                .iter()
                .any(|rule| rule.status != CapabilityStatus::Implemented))
    {
        return Err(CapabilityAdmissionError {
            code: ErrorCode::UnsupportedCapability,
            registry_path: current.registry_path.clone(),
            instance_path: current.instance_path.clone(),
            reason: CapabilityRejectionReason::UnprovenBoundValue,
        });
    }
    if let Some(error) =
        capability_rejection(current, field_rule.status, field_rule.error_code, false)
    {
        return Err(error);
    }

    Ok(())
}

fn validate_capability_leaf(
    capability: &ToolCapability,
    current: &PendingCapabilityValue<'_>,
    value: Option<&str>,
) -> Result<(), CapabilityAdmissionError> {
    if current.registry_path.is_empty() {
        return Ok(());
    }
    let rule = capability.disposition(&current.registry_path, value);
    match capability_rejection(current, rule.status, rule.error_code, rule.value.is_some()) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn capability_rejection(
    current: &PendingCapabilityValue<'_>,
    status: CapabilityStatus,
    error_code: Option<ErrorCode>,
    value_specific: bool,
) -> Option<CapabilityAdmissionError> {
    let (code, reason) = match status {
        CapabilityStatus::Implemented | CapabilityStatus::FallbackLimited => return None,
        CapabilityStatus::UnsupportedStableError => (
            error_code.expect("unsupported capability rules declare a stable error code"),
            if value_specific {
                CapabilityRejectionReason::UnsupportedValue
            } else {
                CapabilityRejectionReason::UnsupportedField
            },
        ),
        CapabilityStatus::Blocked => (
            ErrorCode::UnsupportedCapability,
            CapabilityRejectionReason::BlockedField,
        ),
    };
    Some(CapabilityAdmissionError {
        code,
        registry_path: current.registry_path.clone(),
        instance_path: current.instance_path.clone(),
        reason,
    })
}

fn is_batch_binding(object: &Map<String, Value>) -> bool {
    object.len() == 2 && object.contains_key("$from") && object.contains_key("pointer")
}

fn join_object_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_owned()
    } else {
        format!("{parent}.{child}")
    }
}

fn canonical_capability_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => Some("null".to_owned()),
        Value::Bool(value) => Some(value.to_string()),
        Value::Number(value) => Some(value.to_string()),
        Value::String(value) => Some(value.clone()),
        Value::Array(_) | Value::Object(_) => None,
    }
}

/// Reusable pre-execution validator for dynamic batch child requests.
///
/// It compiles the same checked contracts and invokes the same byte, JSON
/// Schema, typed decoding, and cross-field checks as the direct `tools/call`
/// path.
pub(crate) struct MaterializedToolValidator {
    contracts: Arc<[ToolContract]>,
}

impl MaterializedToolValidator {
    /// Compiles the checked catalog used by dynamic child validation.
    pub(crate) fn compile() -> Result<Self, ToolRegistryError> {
        let mut contracts = Vec::new();
        contracts
            .try_reserve_exact(VerticalTool::ALL.len())
            .map_err(|_| ToolRegistryError::MemoryUnavailable)?;
        for tool in VerticalTool::ALL {
            contracts.push(ToolContract::compile(tool)?);
        }
        Ok(Self {
            contracts: contracts.into(),
        })
    }

    /// Validates one materialized child request through the direct-call path.
    pub(crate) fn validate(
        &self,
        tool: VerticalTool,
        arguments: &Map<String, Value>,
        profile: ExposureProfile,
    ) -> Result<(), MaterializedInputError> {
        let contract = self
            .contracts
            .iter()
            .find(|contract| contract.tool == tool)
            .ok_or(MaterializedInputError::Invalid {
                instance_path: None,
            })?;
        validate_contract_input(contract, &Value::Object(arguments.clone()), profile).map(|_| ())
    }
}

#[derive(Debug)]
pub(crate) enum MaterializedInputError {
    /// The request failed strict input validation.
    Invalid {
        /// Exact failing instance path when JSON Schema identified one.
        instance_path: Option<String>,
    },
    /// Validation produced a more precise checked domain error.
    Public(Box<PublicError>),
}

fn validate_contract_input(
    contract: &ToolContract,
    arguments: &Value,
    profile: ExposureProfile,
) -> Result<TypedInput, MaterializedInputError> {
    if !tool_argument_bytes_are_valid(contract.tool, arguments)
        || !tool_specific_input_limits_are_valid(contract.tool, arguments)
    {
        return Err(classify_schema_error(contract.tool, arguments).map_or(
            MaterializedInputError::Invalid {
                instance_path: None,
            },
            |error| MaterializedInputError::Public(Box::new(error)),
        ));
    }
    if let Err(validation_error) = contract.input_validator.validate(arguments) {
        return Err(classify_schema_error(contract.tool, arguments).map_or_else(
            || MaterializedInputError::Invalid {
                instance_path: schema_error_instance_path(&validation_error),
            },
            |error| MaterializedInputError::Public(Box::new(error)),
        ));
    }
    let typed_input = decode_typed_input(contract.tool, arguments).map_err(|()| {
        classify_typed_decode_error(contract.tool, arguments).map_or(
            MaterializedInputError::Invalid {
                instance_path: None,
            },
            |error| MaterializedInputError::Public(Box::new(error)),
        )
    })?;
    let invariant_error = (!typed_input_invariants_are_valid(contract.tool, &typed_input, profile))
        .then(|| {
            classify_typed_invariant_error(contract.tool, &typed_input, profile).map_or(
                MaterializedInputError::Invalid {
                    instance_path: None,
                },
                |error| MaterializedInputError::Public(Box::new(error)),
            )
        });
    let capability_error = validate_capability_input(
        contract.tool,
        arguments,
        CapabilityBindingPolicy::Materialized,
    )
    .err();

    // Typed decoding and malformed invariants retain precedence. When both
    // admission layers select the same domain code, the registry error is more
    // actionable because it identifies the exact restricted input leaf.
    if let Some(error) = capability_error.as_ref()
        && invariant_error.as_ref().is_none_or(|invariant| {
            matches!(
                invariant,
                MaterializedInputError::Public(public) if public.code() == error.code()
            )
        })
    {
        return Err(materialized_capability_error(error));
    }
    if let Some(error) = invariant_error {
        return Err(error);
    }
    if let Some(error) = capability_error.as_ref() {
        return Err(materialized_capability_error(error));
    }
    Ok(typed_input)
}

fn materialized_capability_error(error: &CapabilityAdmissionError) -> MaterializedInputError {
    let public_error = error
        .to_public_error(None)
        .expect("generated capability paths satisfy public diagnostic bounds");
    MaterializedInputError::Public(Box::new(public_error))
}

fn schema_error_instance_path(error: &jsonschema::ValidationError<'_>) -> Option<String> {
    let parent = error.instance_path().to_string();
    match error.kind() {
        ValidationErrorKind::AdditionalProperties { unexpected } => unexpected
            .first()
            .map(|property| json_pointer_child(&parent, property)),
        ValidationErrorKind::AnyOf { context } | ValidationErrorKind::OneOfNotValid { context } => {
            context
                .iter()
                .flatten()
                .find_map(schema_error_instance_path)
        }
        _ if parent.is_empty() => None,
        _ => Some(parent),
    }
}

fn json_pointer_child(parent: &str, segment: &str) -> String {
    let escaped = segment.replace('~', "~0").replace('/', "~1");
    format!("{parent}/{escaped}")
}

impl ToolContract {
    fn compile(tool: VerticalTool) -> Result<Self, ToolRegistryError> {
        let catalog_tool = catalog_tool(tool);
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
                name: catalog_tool.name(),
                title: catalog_tool.title(),
                description: catalog_tool.description(),
                input_schema,
                output_schema,
                annotations: ToolAnnotations {
                    read_only_hint: catalog_tool.read_only(),
                    destructive_hint: catalog_tool.destructive(),
                    idempotent_hint: catalog_tool.idempotent(),
                    open_world_hint: false,
                },
                execution: ToolExecution {
                    task_support: "forbidden",
                },
                metadata: tool_metadata(catalog_tool),
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
    #[serde(rename = "_meta")]
    metadata: Map<String, Value>,
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

fn catalog_tool(tool: VerticalTool) -> McpTool {
    McpTool::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.name() == tool.name())
        .expect("every vertical tool has a catalog entry")
}

fn tool_metadata(tool: McpTool) -> Map<String, Value> {
    let mut metadata = Map::new();
    metadata.insert(
        DISCOVERY_METADATA_KEY.to_owned(),
        serde_json::to_value(discovery_metadata(tool))
            .expect("built-in capability metadata serializes"),
    );
    metadata
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
    RepoList(RepoListInput),
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
        VerticalTool::RepoList => RepoListInput::deserialize(input)
            .map(TypedInput::RepoList)
            .map_err(|_| ()),
        VerticalTool::RepoStatus
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
        (VerticalTool::RepoList, TypedInput::RepoList(input)) => input
            .cursor
            .as_ref()
            .is_none_or(|cursor| AuthenticatedCursor::from_wire(cursor.as_str()).is_ok()),
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

fn classify_schema_error(tool: VerticalTool, input: &Value) -> Option<PublicError> {
    if repo_list_cursor_is_invalid(tool, input) {
        return Some(mapped_public_error(MappedDomainFailure::invalid_cursor()));
    }
    if tool == VerticalTool::QueryAdvanced
        && input
            .get("query")
            .is_some_and(query_contains_forbidden_operator)
    {
        return Some(mapped_public_error(
            MappedDomainFailure::operator_forbidden("query"),
        ));
    }
    None
}

fn classify_typed_decode_error(tool: VerticalTool, input: &Value) -> Option<PublicError> {
    repo_list_cursor_is_invalid(tool, input)
        .then(|| mapped_public_error(MappedDomainFailure::invalid_cursor()))
}

fn repo_list_cursor_is_invalid(tool: VerticalTool, input: &Value) -> bool {
    if tool != VerticalTool::RepoList {
        return false;
    }
    let Some(cursor) = input.get("cursor") else {
        return false;
    };
    cursor
        .as_str()
        .is_none_or(|cursor| ContinuationCursor::parse(cursor).is_err())
}

fn query_contains_forbidden_operator(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(operator)) = object.get("op")
                && !matches!(
                    operator.as_str(),
                    "scan"
                        | "filter"
                        | "project"
                        | "join"
                        | "aggregate"
                        | "traverse"
                        | "sort"
                        | "limit"
                )
            {
                return true;
            }
            object.values().any(query_contains_forbidden_operator)
        }
        Value::Array(values) => values.iter().any(query_contains_forbidden_operator),
        _ => false,
    }
}

fn classify_typed_invariant_error(
    tool: VerticalTool,
    input: &TypedInput,
    profile: ExposureProfile,
) -> Option<PublicError> {
    match (tool, input) {
        (VerticalTool::RepoList, TypedInput::RepoList(_)) => {
            Some(mapped_public_error(MappedDomainFailure::invalid_cursor()))
        }
        (VerticalTool::QueryAdvanced, TypedInput::QueryAdvanced(input)) => {
            advanced_invariant_error(input)
        }
        (VerticalTool::QueryBatch, TypedInput::QueryBatch(input)) => {
            batch_invariant_error(input, profile)
        }
        _ => None,
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
    advanced_invariant_error(input).is_none()
}

fn advanced_invariant_error(input: &QueryAdvancedInput) -> Option<PublicError> {
    let max_rows = usize::from(input.max_results.unwrap_or(100));
    let plan =
        match AdvancedQueryPlan::from_ast(&input.query, max_rows, MAX_ADVANCED_TRAVERSAL, None) {
            Ok(plan) => plan,
            Err(error) => return Some(mapped_public_error(error.into())),
        };
    (!input
        .cost_limit
        .is_none_or(|limit| plan.estimated_cost <= limit))
    .then(|| mapped_public_error(MappedDomainFailure::cost_limit("cost_limit")))
}

/// Validates the public batch invariants: unique operation ids, an acyclic
/// dependency graph of bounded depth, the closed allowlist intersected with the
/// active exposure profile, and bindings that only reference declared
/// dependencies.
fn batch_invariants_are_valid(input: &QueryBatchInput, profile: ExposureProfile) -> bool {
    batch_invariant_error(input, profile).is_none()
}

fn batch_invariant_error(input: &QueryBatchInput, profile: ExposureProfile) -> Option<PublicError> {
    let operations = &input.operations;

    let mut ids: Vec<&str> = operations.iter().map(|op| op.id.as_str()).collect();
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair[0] == pair[1]) {
        return Some(mapped_public_error(MappedDomainFailure::invalid_argument(
            "operations",
        )));
    }

    let tools: Vec<McpTool> = operations
        .iter()
        .map(|op| mcp_tool_for_batch(op.tool))
        .collect();
    for tool in &tools {
        if !is_batch_allowed(*tool) {
            return Some(mapped_public_error(
                MappedDomainFailure::operator_forbidden("operations"),
            ));
        }
        if !is_batch_allowed_under_profile(*tool, profile) {
            return Some(mapped_public_error(
                MappedDomainFailure::unsupported_capability("operations"),
            ));
        }
    }

    let mut dependencies: Vec<Vec<usize>> = Vec::with_capacity(operations.len());
    for operation in operations {
        let mut resolved = Vec::new();
        if let Some(declared) = &operation.depends_on {
            for name in declared {
                let Some(index) = operations.iter().position(|other| other.id == *name) else {
                    return Some(mapped_public_error(MappedDomainFailure::invalid_argument(
                        "depends_on",
                    )));
                };
                resolved.push(index);
            }
        }
        dependencies.push(resolved);
    }

    if let Err(error) = BatchPlan::validate(&tools, &dependencies) {
        return Some(mapped_public_error(error.into()));
    }

    for (index, operation) in operations.iter().enumerate() {
        if !bindings_reference_declared_dependencies(
            &operation.arguments,
            &dependencies[index],
            operations,
        ) {
            return Some(mapped_public_error(MappedDomainFailure::binding_invalid()));
        }
    }

    None
}

fn mapped_public_error(failure: MappedDomainFailure) -> PublicError {
    public_error(failure).expect("authoritative domain error mapping satisfies public bounds")
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
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rootlight_mcp_contract::{
        NextAction,
        accounting::tool_list_payload,
        capability::{CAPABILITIES, CapabilityRule, CapabilityStatus},
    };
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
            _exposure_profile: ExposureProfile,
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
            _exposure_profile: ExposureProfile,
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
            _exposure_profile: ExposureProfile,
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

    fn retained_input(name: &str) -> Map<String, Value> {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
        ))
        .expect("retained tool contracts are valid JSON");
        fixture["tools"]
            .as_array()
            .expect("tool contracts contain an array")
            .iter()
            .find(|entry| entry["tool"] == name)
            .unwrap_or_else(|| panic!("retained tool contract {name} exists"))["input"]
            .as_object()
            .expect("retained input is an object")
            .clone()
    }

    fn vertical_tool(name: &str) -> VerticalTool {
        VerticalTool::ALL
            .into_iter()
            .find(|tool| tool.name() == name)
            .unwrap_or_else(|| panic!("vertical tool {name} exists"))
    }

    fn dereference_schema(root: &Value, schema: &Value) -> Value {
        let mut resolved = schema.clone();
        while let Some(reference) = resolved.get("$ref").and_then(Value::as_str) {
            let pointer = reference
                .strip_prefix('#')
                .unwrap_or_else(|| panic!("generated schema reference is local: {reference}"));
            resolved = root
                .pointer(pointer)
                .unwrap_or_else(|| panic!("generated schema reference exists: {reference}"))
                .clone();
        }
        resolved
    }

    fn schema_property(root: &Value, schema: &Value, name: &str) -> Option<Value> {
        let resolved = dereference_schema(root, schema);
        if let Some(property) = resolved
            .get("properties")
            .and_then(|properties| properties.get(name))
        {
            return Some(property.clone());
        }
        for keyword in ["anyOf", "oneOf", "allOf"] {
            if let Some(variants) = resolved.get(keyword).and_then(Value::as_array) {
                for variant in variants {
                    if let Some(property) = schema_property(root, variant, name) {
                        return Some(property);
                    }
                }
            }
        }
        None
    }

    fn schema_array_items(root: &Value, schema: &Value) -> Option<Value> {
        let resolved = dereference_schema(root, schema);
        if let Some(items) = resolved.get("items") {
            return Some(items.clone());
        }
        for keyword in ["anyOf", "oneOf", "allOf"] {
            if let Some(variants) = resolved.get(keyword).and_then(Value::as_array) {
                for variant in variants {
                    if let Some(items) = schema_array_items(root, variant) {
                        return Some(items);
                    }
                }
            }
        }
        None
    }

    fn split_capability_path(path: &str) -> Vec<(&str, bool)> {
        path.split('.')
            .map(|segment| {
                segment
                    .strip_suffix("[]")
                    .map_or((segment, false), |name| (name, true))
            })
            .collect()
    }

    fn schema_at_capability_path(root: &Value, path: &str) -> Option<Value> {
        let mut schema = root.clone();
        for (name, array_item) in split_capability_path(path) {
            schema = schema_property(root, &schema, name)?;
            if array_item {
                schema = schema_array_items(root, &schema)?;
            }
        }
        Some(schema)
    }

    fn sample_string(schema: &Value) -> String {
        let pattern = schema.get("pattern").and_then(Value::as_str).unwrap_or("");
        if pattern.starts_with("^repo1_") {
            return "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".to_owned();
        }
        if pattern.starts_with("^gen1_") {
            return "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by".to_owned();
        }
        if pattern.starts_with("^sym1_") {
            return "sym1_cecigxytq5fdpxizkjlxeqzrbmtnd2odobb4eey".to_owned();
        }
        if pattern.starts_with("^file1_") {
            return "file1_cukrkfivcukrkfivcukrkfivcukrkfivpyrmidq".to_owned();
        }
        if pattern.starts_with("^op1_") {
            return "op1_aaaaaaaaaaaaaaaaaaaaaaaaadujjxgv".to_owned();
        }
        let minimum = schema.get("minLength").and_then(Value::as_u64).unwrap_or(1);
        "x".repeat(usize::try_from(minimum.max(1)).expect("schema string bound fits usize"))
    }

    fn sample_schema_values(root: &Value, schema: &Value) -> Vec<Value> {
        let resolved = dereference_schema(root, schema);
        if let Some(constant) = resolved.get("const") {
            return vec![constant.clone()];
        }
        if let Some(values) = resolved.get("enum").and_then(Value::as_array) {
            return values.clone();
        }
        for keyword in ["anyOf", "oneOf"] {
            if let Some(variants) = resolved.get(keyword).and_then(Value::as_array) {
                return variants
                    .iter()
                    .flat_map(|variant| sample_schema_values(root, variant))
                    .collect();
            }
        }
        let sample = match resolved.get("type").and_then(Value::as_str) {
            Some("object") => {
                let mut object = Map::new();
                if let Some(required) = resolved.get("required").and_then(Value::as_array) {
                    for name in required.iter().filter_map(Value::as_str) {
                        let property = resolved
                            .get("properties")
                            .and_then(|properties| properties.get(name))
                            .unwrap_or_else(|| panic!("required generated property {name} exists"));
                        object.insert(name.to_owned(), sample_schema_value(root, property));
                    }
                }
                Value::Object(object)
            }
            Some("array") => {
                let count = resolved
                    .get("minItems")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .max(1);
                let Some(items) = resolved.get("items") else {
                    return vec![Value::Array(Vec::new())];
                };
                Value::Array(
                    (0..count)
                        .map(|_| sample_schema_value(root, items))
                        .collect(),
                )
            }
            Some("boolean") => Value::Bool(false),
            Some("integer") => {
                Value::from(resolved.get("minimum").and_then(Value::as_i64).unwrap_or(0))
            }
            Some("number") => resolved
                .get("minimum")
                .cloned()
                .unwrap_or_else(|| Value::from(0)),
            Some("null") => Value::Null,
            Some("string") => Value::String(sample_string(&resolved)),
            other => panic!("generated schema has a sampleable type: {other:?}"),
        };
        vec![sample]
    }

    fn sample_schema_value(root: &Value, schema: &Value) -> Value {
        sample_schema_values(root, schema)
            .into_iter()
            .next()
            .expect("generated schema has a sample value")
    }

    fn force_schema_path(
        root: &Value,
        schema: &Value,
        path: &[(&str, bool)],
        leaf: &Value,
    ) -> Option<Value> {
        if path.is_empty() {
            return Some(leaf.clone());
        }
        let resolved = dereference_schema(root, schema);
        for keyword in ["anyOf", "oneOf"] {
            if let Some(variants) = resolved.get(keyword).and_then(Value::as_array) {
                for variant in variants {
                    if let Some(value) = force_schema_path(root, variant, path, leaf) {
                        return Some(value);
                    }
                }
                return None;
            }
        }
        let (name, array_item) = path[0];
        let property = resolved
            .get("properties")
            .and_then(|properties| properties.get(name))?;
        let forced = if array_item {
            let items = schema_array_items(root, property)?;
            Value::Array(vec![force_schema_path(root, &items, &path[1..], leaf)?])
        } else {
            force_schema_path(root, property, &path[1..], leaf)?
        };
        let mut object = match sample_schema_value(root, &resolved) {
            Value::Object(object) => object,
            _ => return None,
        };
        object.insert(name.to_owned(), forced);
        Some(Value::Object(object))
    }

    fn rule_value(value: &str) -> Value {
        serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()))
    }

    fn admitted_retained_input(
        capability: &ToolCapability,
        tool: VerticalTool,
        contract: &ToolContract,
        schema: &Value,
    ) -> Map<String, Value> {
        let mut input = retained_input(capability.tool.name());
        loop {
            let value = Value::Object(input.clone());
            match validate_capability_input(tool, &value, CapabilityBindingPolicy::Materialized) {
                Ok(()) => return input,
                Err(error) => {
                    if let Some(implemented) = capability.rules.iter().find(|rule| {
                        rule.path == error.registry_path()
                            && rule.value.is_some()
                            && rule.status == CapabilityStatus::Implemented
                    }) {
                        let path = split_capability_path(implemented.path);
                        let (top_level, _) = *path.first().expect("implemented path is non-empty");
                        let Value::Object(forced) = force_schema_path(
                            schema,
                            schema,
                            &path,
                            &rule_value(
                                implemented
                                    .value
                                    .expect("implemented value rule has a value"),
                            ),
                        )
                        .expect("implemented registry value exists in the generated schema") else {
                            panic!("implemented path builds an input object");
                        };
                        input.insert(
                            top_level.to_owned(),
                            forced
                                .get(top_level)
                                .expect("forced input contains its top-level field")
                                .clone(),
                        );
                        continue;
                    }
                    let top_level = error
                        .instance_path()
                        .split('.')
                        .next()
                        .expect("capability instance path is non-empty");
                    assert!(
                        input.remove(top_level).is_some(),
                        "{} retained input cannot remove restricted path {}",
                        capability.tool.name(),
                        error.instance_path()
                    );
                    assert!(
                        contract
                            .input_validator
                            .is_valid(&Value::Object(input.clone())),
                        "{} retained input requires restricted path {}",
                        capability.tool.name(),
                        error.instance_path()
                    );
                }
            }
        }
    }

    fn generated_rule_input(
        capability: &ToolCapability,
        rule: &CapabilityRule,
        tool: VerticalTool,
        schema: &Value,
    ) -> Option<(Map<String, Value>, CapabilityAdmissionError)> {
        let leaf_schema = schema_at_capability_path(schema, rule.path)?;
        let candidates = rule.value.map_or_else(
            || sample_schema_values(schema, &leaf_schema),
            |value| vec![rule_value(value)],
        );
        let path = split_capability_path(rule.path);
        let (top_name, top_array_item) = *path.first()?;
        let top_schema = schema_property(schema, schema, top_name)?;
        let contract = ToolContract::compile(tool).expect("generated tool contract compiles");

        for candidate in candidates {
            let top_value = if top_array_item {
                let items = schema_array_items(schema, &top_schema)?;
                Value::Array(vec![force_schema_path(
                    schema,
                    &items,
                    &path[1..],
                    &candidate,
                )?])
            } else {
                force_schema_path(schema, &top_schema, &path[1..], &candidate)?
            };
            let mut input = admitted_retained_input(capability, tool, &contract, schema);
            input.insert(top_name.to_owned(), top_value);
            if capability.tool == McpTool::RepoIndex && top_name == "repository_id" {
                input.remove("root");
            }
            let value = Value::Object(input.clone());
            if !contract.input_validator.is_valid(&value) {
                continue;
            }
            let Ok(()) =
                validate_capability_input(tool, &value, CapabilityBindingPolicy::Materialized)
            else {
                let error =
                    validate_capability_input(tool, &value, CapabilityBindingPolicy::Materialized)
                        .expect_err("restricted generated case is rejected");
                return Some((input, error));
            };
        }
        None
    }

    #[test]
    fn capability_traversal_reports_the_first_path_deterministically() {
        let mut query_first = Map::new();
        query_first.insert("coverage_detail".to_owned(), json!("project"));
        query_first.insert("require_freshness".to_owned(), json!("structural"));
        query_first.insert(
            "repository".to_owned(),
            serde_json::to_value(selector()).expect("fixture selector serializes"),
        );
        let mut states_first = Map::new();
        states_first.insert("require_freshness".to_owned(), json!("structural"));
        states_first.insert(
            "repository".to_owned(),
            serde_json::to_value(selector()).expect("fixture selector serializes"),
        );
        states_first.insert("coverage_detail".to_owned(), json!("project"));

        let first = validate_capability_input(
            VerticalTool::RepoStatus,
            &Value::Object(query_first),
            CapabilityBindingPolicy::Materialized,
        )
        .expect_err("unsupported project coverage projection is rejected");
        let second = validate_capability_input(
            VerticalTool::RepoStatus,
            &Value::Object(states_first),
            CapabilityBindingPolicy::Materialized,
        )
        .expect_err("unsupported projection is rejected regardless of insertion order");

        assert_eq!(first, second);
        assert_eq!(first.registry_path(), "coverage_detail");
        assert_eq!(first.instance_path(), "coverage_detail");
        assert_eq!(first.reason(), CapabilityRejectionReason::UnsupportedValue);
        assert_eq!(first.code(), ErrorCode::UnsupportedCapability);
    }

    #[test]
    fn capability_traversal_checks_explicit_empty_containers() {
        let error = validate_capability_input(
            VerticalTool::QueryBatch,
            &json!({"budget": {}}),
            CapabilityBindingPolicy::Materialized,
        )
        .expect_err("an explicit blocked budget is rejected even when empty");

        assert_eq!(error.registry_path(), "budget");
        assert_eq!(error.instance_path(), "budget");
        assert_eq!(error.reason(), CapabilityRejectionReason::BlockedField);
    }

    #[test]
    fn capability_traversal_prefers_values_and_descendants_over_ancestors() {
        validate_capability_input(
            VerticalTool::QueryBatch,
            &json!({
                "generation": "active",
                "operations": [{
                    "local_budget": {"timeout_ms": 50}
                }]
            }),
            CapabilityBindingPolicy::Materialized,
        )
        .expect("implemented values and descendants override limited ancestors");

        let error = validate_capability_input(
            VerticalTool::QueryBatch,
            &json!({"operations": [{"tool": "plan.change"}]}),
            CapabilityBindingPolicy::Materialized,
        )
        .expect_err("the restricted batch tool value is rejected");
        assert_eq!(error.code(), ErrorCode::OperatorForbidden);
        assert_eq!(error.registry_path(), "operations[].tool");
        assert_eq!(error.instance_path(), "operations.0.tool");
        assert_eq!(error.reason(), CapabilityRejectionReason::UnsupportedValue);
    }

    #[test]
    fn unresolved_bindings_fail_closed_only_for_restricted_targets() {
        let binding = json!({"$from": "find", "pointer": "/data/matches/0/source_ref"});
        let error = validate_capability_input(
            VerticalTool::SourceRead,
            &json!({"response_profile": binding}),
            CapabilityBindingPolicy::RejectUnprovenRestrictedBindings,
        )
        .expect_err("a binding cannot prove that it avoids restricted profile values");
        assert_eq!(error.registry_path(), "response_profile");
        assert_eq!(error.instance_path(), "response_profile");
        assert_eq!(
            error.reason(),
            CapabilityRejectionReason::UnprovenBoundValue
        );

        validate_capability_input(
            VerticalTool::SourceRead,
            &json!({
                "references": [{
                    "source_ref": {
                        "$from": "find",
                        "pointer": "/data/matches/0/source_ref"
                    }
                }]
            }),
            CapabilityBindingPolicy::RejectUnprovenRestrictedBindings,
        )
        .expect("an unrestricted binding remains eligible for materialization");
    }

    #[test]
    fn unresolved_binding_over_an_implemented_exception_is_unproven() {
        const RULES: &[CapabilityRule] = &[
            CapabilityRule {
                path: "selector",
                value: None,
                status: CapabilityStatus::UnsupportedStableError,
                error_code: Some(ErrorCode::UnsupportedCapability),
                summary: "fixture field is restricted",
            },
            CapabilityRule {
                path: "selector",
                value: Some("active"),
                status: CapabilityStatus::Implemented,
                error_code: None,
                summary: "fixture value is implemented",
            },
        ];
        let capability = ToolCapability {
            rules: RULES,
            ..*capability_for(McpTool::QueryBatch)
        };
        let binding = json!({"$from": "find", "pointer": "/data/selector"});
        let current = PendingCapabilityValue {
            value: &binding,
            registry_path: "selector".to_owned(),
            instance_path: "selector".to_owned(),
        };

        let error = validate_unresolved_binding(&capability, &current)
            .expect_err("the binding cannot prove it selects the implemented exception");
        assert_eq!(
            error.reason(),
            CapabilityRejectionReason::UnprovenBoundValue
        );
    }

    #[test]
    fn materialized_policy_rejects_an_unresolved_binding_object() {
        let error = validate_capability_input(
            VerticalTool::SourceRead,
            &json!({
                "references": [{
                    "source_ref": {
                        "$from": "find",
                        "pointer": "/data/matches/0/source_ref"
                    }
                }]
            }),
            CapabilityBindingPolicy::Materialized,
        )
        .expect_err("materialized validation cannot accept a binding placeholder");

        assert_eq!(error.code(), ErrorCode::BindingInvalid);
        assert_eq!(error.registry_path(), "references[].source_ref");
        assert_eq!(error.instance_path(), "references.0.source_ref");
        assert_eq!(error.reason(), CapabilityRejectionReason::UnresolvedBinding);
    }

    #[test]
    #[should_panic(expected = "unsupported capability rules declare a stable error code")]
    fn unsupported_rule_without_error_code_violates_the_registry_invariant() {
        let current = PendingCapabilityValue {
            value: &Value::Null,
            registry_path: "fixture".to_owned(),
            instance_path: "fixture".to_owned(),
        };

        let _ = capability_rejection(
            &current,
            CapabilityStatus::UnsupportedStableError,
            None,
            false,
        );
    }

    #[test]
    fn capability_values_use_canonical_scalar_spellings() {
        assert_eq!(
            canonical_capability_value(&json!("active")).as_deref(),
            Some("active")
        );
        assert_eq!(
            canonical_capability_value(&Value::Bool(true)).as_deref(),
            Some("true")
        );
        assert_eq!(
            canonical_capability_value(&json!(42.5)).as_deref(),
            Some("42.5")
        );
        assert_eq!(
            canonical_capability_value(&Value::Null).as_deref(),
            Some("null")
        );
        assert_eq!(canonical_capability_value(&json!([])), None);
        assert_eq!(canonical_capability_value(&json!({})), None);
    }

    #[test]
    fn capability_rejection_builds_bounded_field_details() {
        let error = validate_capability_input(
            VerticalTool::QueryBatch,
            &json!({"operations": [{"tool": "plan.change"}]}),
            CapabilityBindingPolicy::Materialized,
        )
        .expect_err("the restricted value is rejected")
        .to_public_error(Some("operations.3.arguments"))
        .expect("generated capability paths fit public diagnostic bounds");

        assert_eq!(error.code(), ErrorCode::OperatorForbidden);
        assert_eq!(
            error
                .details()
                .get(&DetailKey::parse("field_path").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("operations.3.arguments.operations.0.tool")
                    .expect("fixture path is valid")
            ))
        );
        assert_eq!(
            error
                .details()
                .get(&DetailKey::parse("capability_reason").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("unsupported_value").expect("fixture reason is valid")
            ))
        );
        assert_eq!(
            error.next_actions(),
            &[NextAction::CorrectField {
                field: DetailKey::parse("arguments").expect("static detail key is valid")
            }]
        );
    }

    #[tokio::test]
    async fn generated_restricted_rules_reject_direct_calls_without_execution() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let materialized_validator =
            MaterializedToolValidator::compile().expect("checked contracts compile once");
        let field_key = DetailKey::parse("field_path").expect("static detail key is valid");
        let reason_key = DetailKey::parse("capability_reason").expect("static detail key is valid");
        let mut declared = 0usize;
        let mut covered = 0usize;
        let mut exclusions = Vec::new();

        for capability in &CAPABILITIES {
            let tool = vertical_tool(capability.tool.name());
            let schema: Value =
                serde_json::from_str(tool.input_schema_json()).expect("input schema is valid JSON");
            for rule in capability.rules.iter().filter(|rule| {
                matches!(
                    rule.status,
                    CapabilityStatus::UnsupportedStableError | CapabilityStatus::Blocked
                )
            }) {
                declared += 1;
                assert!(
                    schema_at_capability_path(&schema, rule.path).is_some(),
                    "{} capability path {} exists in its generated schema",
                    capability.tool.name(),
                    rule.path
                );
                let Some((arguments, admission)) =
                    generated_rule_input(capability, rule, tool, &schema)
                else {
                    exclusions.push(format!(
                        "{}:{}={}",
                        capability.tool.name(),
                        rule.path,
                        rule.value.unwrap_or("*")
                    ));
                    continue;
                };
                let expected_code = match rule.status {
                    CapabilityStatus::UnsupportedStableError => rule
                        .error_code
                        .expect("unsupported rule declares a stable code"),
                    CapabilityStatus::Blocked => ErrorCode::UnsupportedCapability,
                    CapabilityStatus::Implemented | CapabilityStatus::FallbackLimited => {
                        unreachable!("test filters to rejected rules")
                    }
                };
                let expected_reason = match (rule.status, rule.value) {
                    (CapabilityStatus::UnsupportedStableError, Some(_)) => {
                        CapabilityRejectionReason::UnsupportedValue
                    }
                    (CapabilityStatus::UnsupportedStableError, None) => {
                        CapabilityRejectionReason::UnsupportedField
                    }
                    (CapabilityStatus::Blocked, _) => CapabilityRejectionReason::BlockedField,
                    (CapabilityStatus::Implemented | CapabilityStatus::FallbackLimited, _) => {
                        unreachable!("test filters to rejected rules")
                    }
                };
                assert_eq!(
                    admission.code(),
                    expected_code,
                    "{}:{} resolved to the wrong code",
                    capability.tool.name(),
                    rule.path
                );
                assert_eq!(
                    admission.reason(),
                    expected_reason,
                    "{}:{} resolved to the wrong reason",
                    capability.tool.name(),
                    rule.path
                );
                let resolves_through_rule = admission.registry_path() == rule.path
                    || admission
                        .registry_path()
                        .strip_prefix(rule.path)
                        .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with("[]"));
                let co_required_source_selector = capability.tool == McpTool::SourceRead
                    && matches!(
                        rule.path,
                        "references[].file_id" | "references[].start_byte"
                    )
                    && admission.registry_path() == "references[].end_byte";
                assert!(
                    resolves_through_rule || co_required_source_selector,
                    "{}:{} resolved through unrelated registry path {}",
                    capability.tool.name(),
                    rule.path,
                    admission.registry_path()
                );
                if co_required_source_selector {
                    let reference = arguments
                        .get("references")
                        .and_then(Value::as_array)
                        .and_then(|references| references.first())
                        .and_then(Value::as_object)
                        .expect("generated source selector contains one reference");
                    assert!(
                        ["file_id", "start_byte", "end_byte"]
                            .into_iter()
                            .all(|field| reference.contains_key(field)),
                        "{}:{} structural exclusion must exercise the complete source selector",
                        capability.tool.name(),
                        rule.path
                    );
                    let contract =
                        ToolContract::compile(tool).expect("generated tool contract compiles");
                    assert!(
                        contract
                            .input_validator
                            .is_valid(&Value::Object(arguments.clone())),
                        "{}:{} structural exclusion must remain schema-valid",
                        capability.tool.name(),
                        rule.path
                    );
                    for required_field in ["file_id", "start_byte", "end_byte"] {
                        let mut incomplete = arguments.clone();
                        incomplete
                            .get_mut("references")
                            .and_then(Value::as_array_mut)
                            .and_then(|references| references.first_mut())
                            .and_then(Value::as_object_mut)
                            .expect("generated source selector contains one reference")
                            .remove(required_field);
                        assert!(
                            !contract
                                .input_validator
                                .is_valid(&Value::Object(incomplete)),
                            "{}:{} source selector must require {required_field}",
                            capability.tool.name(),
                            rule.path
                        );
                    }
                    assert_eq!(admission.registry_path(), "references[].end_byte");
                    assert_eq!(admission.instance_path(), "references.0.end_byte");
                }

                let calls_before = router.executor.calls.load(Ordering::Relaxed);
                let response = router
                    .handle(
                        request(
                            "tools/call",
                            json!({
                                "name": capability.tool.name(),
                                "arguments": arguments.clone()
                            }),
                        ),
                        cancellation(),
                    )
                    .await;
                let result = success(response);
                let direct: ErrorResponse =
                    serde_json::from_value(result["structuredContent"].clone())
                        .expect("capability rejection uses the checked error contract");
                assert_eq!(
                    direct.error.code(),
                    expected_code,
                    "{}:{}={}",
                    capability.tool.name(),
                    rule.path,
                    rule.value.unwrap_or("*")
                );
                assert_eq!(
                    direct.error.details().get(&field_key),
                    Some(&PublicValue::Label(
                        SafeLabel::parse(admission.instance_path())
                            .expect("generated instance path is a safe label")
                    ))
                );
                assert_eq!(
                    direct.error.details().get(&reason_key),
                    Some(&PublicValue::Label(
                        SafeLabel::parse(expected_reason.public_name())
                            .expect("stable reason is a safe label")
                    ))
                );
                let materialized = materialized_validator
                    .validate(tool, &arguments, ExposureProfile::Developer)
                    .expect_err("materialized request shares capability admission");
                let MaterializedInputError::Public(materialized) = materialized else {
                    panic!("capability rejection must remain a checked public error");
                };
                assert_eq!(
                    *materialized,
                    direct.error,
                    "{}:{} materialized admission diverged from the direct router",
                    capability.tool.name(),
                    rule.path
                );
                assert_eq!(
                    router.executor.calls.load(Ordering::Relaxed),
                    calls_before,
                    "{}:{} reached the executor",
                    capability.tool.name(),
                    rule.path
                );
                if co_required_source_selector {
                    exclusions.push(format!(
                        "{}:{}=*:co_required_selector",
                        capability.tool.name(),
                        rule.path
                    ));
                } else {
                    covered += 1;
                }
            }
        }

        assert_eq!(
            exclusions,
            [
                "source.read:references[].file_id=*:co_required_selector",
                "source.read:references[].start_byte=*:co_required_selector",
            ],
            "review any new generated-rule exclusion"
        );
        assert_eq!((declared, covered, exclusions.len()), (131, 129, 2));
    }

    #[tokio::test]
    async fn every_tool_rejects_malformed_and_unknown_fields_without_execution() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");

        for capability in &CAPABILITIES {
            let tool = vertical_tool(capability.tool.name());
            let contract = ToolContract::compile(tool).expect("generated tool contract compiles");
            let retained = retained_input(capability.tool.name());
            let mut cases = Vec::with_capacity(2);

            let mut unknown = retained.clone();
            unknown.insert("unknown_field".to_owned(), Value::Bool(true));
            cases.push(("unknown", unknown));

            let mut malformed = retained;
            if let Some(name) = malformed.keys().next().cloned() {
                malformed.insert(name, Value::Null);
            } else {
                malformed.insert("max_results".to_owned(), Value::Null);
            }
            cases.push(("malformed", malformed));

            for (label, arguments) in cases {
                assert!(
                    !contract
                        .input_validator
                        .is_valid(&Value::Object(arguments.clone())),
                    "{} {label} fixture must fail the generated schema",
                    capability.tool.name()
                );
                let calls_before = router.executor.calls.load(Ordering::Relaxed);
                let response = router
                    .handle(
                        request(
                            "tools/call",
                            json!({
                                "name": capability.tool.name(),
                                "arguments": arguments
                            }),
                        ),
                        cancellation(),
                    )
                    .await;
                let result = success(response);
                let error: ErrorResponse =
                    serde_json::from_value(result["structuredContent"].clone())
                        .expect("schema rejection uses the checked error contract");
                assert_eq!(
                    error.error.code(),
                    ErrorCode::InvalidArgument,
                    "{} {label} input used the wrong code",
                    capability.tool.name()
                );
                assert_eq!(
                    router.executor.calls.load(Ordering::Relaxed),
                    calls_before,
                    "{} {label} input reached the executor",
                    capability.tool.name()
                );
            }
        }
    }

    #[tokio::test]
    async fn locate_mode_combinations_match_direct_and_materialized_admission() {
        for mode in ["exact", "lexical"] {
            validate_capability_input(
                VerticalTool::CodeLocate,
                &json!({"search_modes": [mode]}),
                CapabilityBindingPolicy::Materialized,
            )
            .unwrap_or_else(|error| panic!("single {mode} mode remains admitted: {error:?}"));
        }

        let mut arguments = retained_input("code.locate");
        arguments.insert("search_modes".to_owned(), json!(["exact", "lexical"]));
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "code.locate", "arguments": arguments}),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
            .expect("combination rejection uses the checked error contract");
        assert_eq!(direct.error.code(), ErrorCode::UnsupportedCapability);
        assert_eq!(
            direct
                .error
                .details()
                .get(&DetailKey::parse("field_path").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("search_modes").expect("static field path is valid")
            ))
        );
        assert_eq!(
            direct
                .error
                .details()
                .get(&DetailKey::parse("capability_reason").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("unsupported_combination")
                    .expect("static capability reason is valid")
            ))
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);

        let validator =
            MaterializedToolValidator::compile().expect("checked contracts compile once");
        let materialized_arguments = Map::from_iter([
            (
                "repository".to_owned(),
                json!({"repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"}),
            ),
            ("query".to_owned(), json!("fixture")),
            ("search_modes".to_owned(), json!(["exact", "lexical"])),
        ]);
        let MaterializedInputError::Public(materialized) = validator
            .validate(
                VerticalTool::CodeLocate,
                &materialized_arguments,
                ExposureProfile::Developer,
            )
            .expect_err("materialized mode combination is rejected")
        else {
            panic!("materialized combination must retain the capability error");
        };
        assert_eq!(*materialized, direct.error);
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
            let metadata = &tool["_meta"][DISCOVERY_METADATA_KEY];
            let capability = CAPABILITIES
                .iter()
                .find(|capability| tool["name"] == capability.tool.name())
                .expect("listed tool has a capability entry");
            assert_eq!(
                metadata["contractVersion"],
                capability.tool.contract_version()
            );
            assert_eq!(tool["title"], capability.tool.title());
            assert_eq!(tool["description"], capability.tool.description());
            assert_eq!(
                tool["annotations"]["readOnlyHint"],
                capability.tool.read_only()
            );
            assert_eq!(
                tool["annotations"]["destructiveHint"],
                capability.tool.destructive()
            );
            assert_eq!(
                tool["annotations"]["idempotentHint"],
                capability.tool.idempotent()
            );
            assert_eq!(metadata["status"], capability.status.name());
            assert!(
                metadata["profiles"]
                    .as_array()
                    .is_some_and(|profiles| !profiles.is_empty())
            );
            assert!(metadata["inputShapeHash"].as_str().is_some());
        }
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
        assert_eq!(tools[0]["annotations"]["idempotentHint"], false);
        assert_eq!(
            tools[0]["_meta"][DISCOVERY_METADATA_KEY]["lifecycle"],
            json!({
                "version": "1.0",
                "updateByRepositoryId": false,
                "acceptedModes": ["auto", "structural"],
                "scope": "whole_repository",
                "synchronousTerminal": true,
                "maxWaitMs": 30_000,
                "detached": false,
                "publicIdempotency": "none",
                "internalOperationRetry": true,
                "statePersistence": "process_local",
                "restartBehavior": "reindex_required",
                "publication": "atomic_on_terminal_success"
            })
        );
        assert_eq!(tools[2]["annotations"]["readOnlyHint"], true);
        let operation_status = tools
            .iter()
            .find(|tool| tool["name"] == "operation.status")
            .expect("operation.status is listed");
        assert_eq!(
            operation_status["annotations"]["readOnlyHint"], false,
            "operation.status can cancel and must not be advertised as read-only"
        );
        assert_eq!(operation_status["annotations"]["idempotentHint"], true);
        let repo_list = tools
            .iter()
            .find(|tool| tool["name"] == "repo.list")
            .expect("repo.list is listed");
        assert_eq!(
            repo_list["_meta"][DISCOVERY_METADATA_KEY]["pagination"],
            "authenticated_cursor"
        );
        let expected_pagination = [
            ("repo.index", "not_applicable"),
            ("operation.status", "bounded_complete"),
            ("repo.list", "authenticated_cursor"),
            ("repo.status", "bounded_complete"),
            ("code.locate", "authenticated_cursor"),
            ("symbol.explain", "progressive_handle"),
            ("symbol.relationships", "authenticated_cursor"),
            ("flow.trace", "explicit_truncation"),
            ("change.impact", "explicit_truncation"),
            ("tests.select", "explicit_truncation"),
            ("architecture.overview", "explicit_truncation"),
            ("architecture.cycles", "explicit_truncation"),
            ("code.dead", "explicit_truncation"),
            ("context.pack", "progressive_handle"),
            ("query.advanced", "authenticated_cursor"),
            ("query.batch", "child_continuations"),
            ("history.compare", "explicit_truncation"),
            ("plan.change", "explicit_truncation"),
            ("source.read", "explicit_truncation"),
        ];
        for (name, semantics) in expected_pagination {
            let tool = tools
                .iter()
                .find(|tool| tool["name"] == name)
                .expect("every catalog tool is listed");
            assert_eq!(
                tool["_meta"][DISCOVERY_METADATA_KEY]["pagination"], semantics,
                "{name} has stale pagination discovery"
            );
        }
        let batch = tools
            .iter()
            .find(|tool| tool["name"] == "query.batch")
            .expect("query.batch is listed");
        assert_eq!(
            batch["_meta"][DISCOVERY_METADATA_KEY]["batchSharedBudget"],
            true
        );
    }

    #[tokio::test]
    async fn registry_entries_reach_a_handler_or_checked_pre_execution_error() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let discovery = success(
            router
                .handle(request("tools/list", json!({})), cancellation())
                .await,
        );
        let listed = discovery["tools"]
            .as_array()
            .expect("tools/list returns an array");
        for intent in [
            "code.locate",
            "change.impact",
            "architecture.overview",
            "history.compare",
            "context.pack",
            "query.batch",
        ] {
            let metadata = listed
                .iter()
                .find(|tool| tool["name"] == intent)
                .unwrap_or_else(|| panic!("{intent} is discoverable"))["_meta"]
                [DISCOVERY_METADATA_KEY]
                .clone();
            assert_eq!(metadata["status"], "fallback_limited");
            assert!(
                metadata["fallbackSummary"]
                    .as_str()
                    .is_some_and(|summary| summary.starts_with("bounded"))
            );
            assert!(metadata["limitations"].as_array().is_some());
        }

        let mut routed = BTreeSet::new();
        for capability in &CAPABILITIES {
            let calls_before = router.executor.calls.load(Ordering::Relaxed);
            let response = router
                .handle(
                    request(
                        "tools/call",
                        json!({"name": capability.tool.name(), "arguments": {}}),
                    ),
                    cancellation(),
                )
                .await;
            let calls_after = router.executor.calls.load(Ordering::Relaxed);
            if calls_after == calls_before {
                let HandlerResponse::Success(result) = response else {
                    panic!(
                        "{} was neither routed nor rejected by its checked tool contract",
                        capability.tool.name()
                    );
                };
                assert_eq!(result["isError"], true);
                let error: ErrorResponse =
                    serde_json::from_value(result["structuredContent"].clone())
                        .expect("pre-execution rejection uses the checked error contract");
                assert_eq!(error.error.code(), ErrorCode::InvalidArgument);
            }
            assert!(
                routed.insert(capability.tool.name()),
                "registry tool is unique"
            );
        }
        assert_eq!(routed.len(), CAPABILITIES.len());
    }

    #[tokio::test]
    async fn tools_list_payloads_match_profile_goldens() {
        let profiles = [
            ExposureProfile::Scout,
            ExposureProfile::Analysis,
            ExposureProfile::Developer,
        ];
        let mut observed = Vec::with_capacity(profiles.len());
        for profile in profiles {
            let router =
                ToolRouter::new(FixtureExecutor::default(), profile).expect("registry compiles");
            let response = router
                .handle(request("tools/list", json!({})), cancellation())
                .await;
            let result = success(response);
            assert_eq!(
                Value::Object(result.clone()),
                tool_list_payload(profile),
                "{} server payload drifted from canonical accounting",
                profile.name()
            );
            let encoded = serde_json::to_vec(&result).expect("tools/list serializes");
            observed.push((encoded.len(), blake3::hash(&encoded).to_hex().to_string()));
        }
        assert_eq!(
            observed,
            [
                (
                    223_183,
                    "b51cfdda6a6e6614594eea1e519a7eaf10e969bb333fe316531b9af0b3210b03".to_owned(),
                ),
                (
                    485_915,
                    "ff7aa3d7f4e34c56a1a2eb91019c1a0af59edd4e27c2808494bca979f40382e9".to_owned(),
                ),
                (
                    655_577,
                    "59a294fdbb2d5b65bf157f21fc6f45ac986b2346f2a0699729e3dcfd398fa0f0".to_owned(),
                ),
            ],
            "update the reviewed Scout, Analysis, and Developer tools/list goldens"
        );
    }

    #[tokio::test]
    async fn discovery_copy_and_guidance_are_source_free() {
        for profile in ExposureProfile::ALL {
            let router =
                ToolRouter::new(FixtureExecutor::default(), profile).expect("registry compiles");
            let result = success(
                router
                    .handle(request("tools/list", json!({})), cancellation())
                    .await,
            );
            for tool in result["tools"].as_array().expect("tools is an array") {
                let copy = serde_json::to_string(&json!({
                    "title": tool["title"],
                    "description": tool["description"],
                    "capabilities": tool["_meta"][DISCOVERY_METADATA_KEY],
                }))
                .expect("discovery copy serializes");
                let lowercase = copy.to_ascii_lowercase();
                for forbidden in [
                    "rootlight_prompt_sentinel",
                    "ignore previous instructions",
                    "rootlight-mcp::",
                    "c:\\",
                    "c:/users/",
                    "/home/",
                    "/users/",
                    "file://",
                ] {
                    assert!(
                        !lowercase.contains(forbidden),
                        "{} discovery contains forbidden source or prompt text: {forbidden}",
                        tool["name"]
                    );
                }
                for private_label in [["TASK", "-"].concat(), ["GATE", "-"].concat()] {
                    assert!(!copy.contains(&private_label));
                }
            }
        }
    }

    #[tokio::test]
    async fn representative_intents_route_to_six_truthful_discovery_entries() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let result = success(
            router
                .handle(request("tools/list", json!({})), cancellation())
                .await,
        );
        let listed = result["tools"].as_array().expect("tools is an array");
        let cases = [
            (
                "find an exact identifier",
                "code.locate",
                "exact-identifier and lexical matching",
                "search_modes[]",
            ),
            (
                "map an explicit symbol change",
                "change.impact",
                "symbol-or-path change mapping",
                "change.revision_range",
            ),
            (
                "summarize file architecture",
                "architecture.overview",
                "file-granularity architecture map",
                "views[]",
            ),
            (
                "compare retained generations",
                "history.compare",
                "retained generation identifiers",
                "base.git",
            ),
            (
                "assemble focused evidence",
                "context.pack",
                "evidence assembly from explicit symbol or file identifiers",
                "seeds.paths",
            ),
            (
                "dispatch several generation-pinned reads",
                "query.batch",
                "active-generation batch dispatch",
                "generation",
            ),
        ];
        for (intent, expected, routing_phrase, limitation_field) in cases {
            let matches: Vec<&Value> = listed
                .iter()
                .filter(|tool| {
                    tool["description"]
                        .as_str()
                        .is_some_and(|description| description.contains(routing_phrase))
                })
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "intent {intent:?} must have one unambiguous discovery match"
            );
            let selected = matches[0];
            assert_eq!(selected["name"], expected, "intent {intent:?} misrouted");
            let capability = CAPABILITIES
                .iter()
                .find(|capability| capability.tool.name() == expected)
                .expect("routing tool has a capability");
            assert_eq!(capability.status, CapabilityStatus::FallbackLimited);
            assert!(
                selected["_meta"][DISCOVERY_METADATA_KEY]["limitations"]
                    .as_array()
                    .is_some_and(|limitations| limitations
                        .iter()
                        .any(|limitation| limitation["field"] == limitation_field)),
                "intent {intent:?} lacks a visible fallback for {limitation_field}"
            );
        }
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
    async fn schema_and_typed_failures_use_authoritative_mapping() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.list",
                        "arguments": {
                            "query": "needle",
                            "unknown": true
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
            .expect("schema rejection uses the checked error contract");
        assert_eq!(
            direct.error,
            public_error(MappedDomainFailure::invalid_argument("arguments"))
                .expect("authoritative schema mapping builds")
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);

        let validator =
            MaterializedToolValidator::compile().expect("checked contracts compile once");
        let arguments = Map::from_iter([
            ("query".to_owned(), json!("needle")),
            ("unknown".to_owned(), Value::Bool(true)),
        ]);
        assert!(matches!(
            validator.validate(
                VerticalTool::RepoList,
                &arguments,
                ExposureProfile::Developer
            ),
            Err(MaterializedInputError::Invalid { .. })
        ));

        let typed_arguments = json!({
            "repository": selector(),
            "operations": [
                {"id": "duplicate", "tool": "code.locate", "arguments": {"query": "first"}},
                {"id": "duplicate", "tool": "code.locate", "arguments": {"query": "second"}}
            ]
        })
        .as_object()
        .expect("typed fixture is an object")
        .clone();
        let typed_response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "query.batch", "arguments": typed_arguments.clone()}),
                ),
                cancellation(),
            )
            .await;
        let typed_result = success(typed_response);
        let typed_direct: ErrorResponse =
            serde_json::from_value(typed_result["structuredContent"].clone())
                .expect("typed rejection uses the checked error contract");
        let authoritative = public_error(MappedDomainFailure::invalid_argument("operations"))
            .expect("authoritative typed mapping builds");
        assert_eq!(typed_direct.error, authoritative);

        let MaterializedInputError::Public(typed_materialized) = validator
            .validate(
                VerticalTool::QueryBatch,
                &typed_arguments,
                ExposureProfile::Developer,
            )
            .expect_err("materialized typed fixture is rejected")
        else {
            panic!("typed invariant must retain its public domain error");
        };
        assert_eq!(*typed_materialized, authoritative);
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn direct_and_materialized_capability_rejections_match_without_execution() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "repo.list",
                        "arguments": {"response_profile": "evidence"}
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
            .expect("capability rejection uses the checked error contract");
        let authoritative = public_error(MappedDomainFailure::unsupported_capability("arguments"))
            .expect("authoritative capability mapping builds");
        assert_eq!(direct.error.code(), authoritative.code());
        assert_eq!(direct.error.message(), authoritative.message());
        assert_eq!(direct.error.retryable(), authoritative.retryable());
        assert_eq!(direct.error.next_actions(), authoritative.next_actions());
        assert_eq!(direct.error.code(), ErrorCode::UnsupportedCapability);
        assert_eq!(
            direct
                .error
                .details()
                .get(&DetailKey::parse("field_path").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("response_profile").expect("fixture path is valid")
            ))
        );
        assert_eq!(
            direct
                .error
                .details()
                .get(&DetailKey::parse("capability_reason").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("unsupported_value").expect("fixture reason is valid")
            ))
        );
        assert_eq!(
            direct.error.next_actions(),
            &[NextAction::CorrectField {
                field: DetailKey::parse("arguments").expect("static detail key is valid")
            }]
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);

        let validator =
            MaterializedToolValidator::compile().expect("checked contracts compile once");
        let arguments = Map::from_iter([("response_profile".to_owned(), json!("evidence"))]);
        let materialized = validator
            .validate(
                VerticalTool::RepoList,
                &arguments,
                ExposureProfile::Developer,
            )
            .expect_err("materialized requests share capability admission");
        let MaterializedInputError::Public(materialized) = materialized else {
            panic!("capability rejection must not become a binding type mismatch");
        };
        assert_eq!(*materialized, direct.error);
    }

    #[tokio::test]
    async fn restricted_batch_tool_has_precise_direct_and_materialized_admission() {
        let arguments = json!({
            "repository": selector(),
            "operations": [{
                "id": "plan",
                "tool": "plan.change",
                "arguments": {}
            }]
        })
        .as_object()
        .expect("batch arguments are an object")
        .clone();
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "query.batch", "arguments": arguments.clone()}),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
            .expect("batch capability rejection uses the checked error contract");

        assert_eq!(direct.error.code(), ErrorCode::OperatorForbidden);
        assert_eq!(
            direct
                .error
                .details()
                .get(&DetailKey::parse("field_path").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("operations.0.tool").expect("fixture field path is valid")
            ))
        );
        assert_eq!(
            direct
                .error
                .details()
                .get(&DetailKey::parse("capability_reason").expect("static detail key is valid")),
            Some(&PublicValue::Label(
                SafeLabel::parse("unsupported_value").expect("fixture reason is valid")
            ))
        );
        assert_eq!(
            direct.error.next_actions(),
            &[NextAction::CorrectField {
                field: DetailKey::parse("arguments").expect("static detail key is valid")
            }]
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);

        let validator =
            MaterializedToolValidator::compile().expect("checked contracts compile once");
        let MaterializedInputError::Public(materialized) = validator
            .validate(
                VerticalTool::QueryBatch,
                &arguments,
                ExposureProfile::Developer,
            )
            .expect_err("materialized batch shares capability admission")
        else {
            panic!("batch capability rejection must remain a checked public error");
        };
        assert_eq!(*materialized, direct.error);
    }

    #[tokio::test]
    async fn malformed_batch_invariant_precedes_restricted_tool_admission() {
        let arguments = json!({
            "repository": selector(),
            "operations": [
                {"id": "duplicate", "tool": "plan.change", "arguments": {}},
                {"id": "duplicate", "tool": "plan.change", "arguments": {}}
            ]
        })
        .as_object()
        .expect("batch arguments are an object")
        .clone();
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "query.batch", "arguments": arguments.clone()}),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
            .expect("typed invariant rejection uses the checked error contract");

        assert_eq!(direct.error.code(), ErrorCode::InvalidArgument);
        assert!(direct.error.details().is_empty());
        assert_eq!(
            direct.error.next_actions(),
            &[NextAction::CorrectField {
                field: DetailKey::parse("operations").expect("static detail key is valid")
            }]
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);

        let validator =
            MaterializedToolValidator::compile().expect("checked contracts compile once");
        let MaterializedInputError::Public(materialized) = validator
            .validate(
                VerticalTool::QueryBatch,
                &arguments,
                ExposureProfile::Developer,
            )
            .expect_err("materialized batch preserves malformed invariant precedence")
        else {
            panic!("typed invariant rejection must remain a checked public error");
        };
        assert_eq!(*materialized, direct.error);
    }

    #[tokio::test]
    async fn cursor_failures_use_authoritative_restart_mapping() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let authoritative = public_error(MappedDomainFailure::invalid_cursor())
            .expect("authoritative cursor mapping builds");
        let invalid_cursors = [
            String::new(),
            "A".repeat(4_097),
            "\u{1f4a1}".repeat(1_025),
            "c2.A".to_owned(),
        ];

        for cursor in invalid_cursors {
            let response = router
                .handle(
                    request(
                        "tools/call",
                        json!({"name": "repo.list", "arguments": {"cursor": cursor}}),
                    ),
                    cancellation(),
                )
                .await;
            let result = success(response);
            assert_eq!(result["isError"], true);
            let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
                .expect("cursor rejection uses the checked error contract");
            assert_eq!(direct.error, authoritative);
        }

        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn repo_list_non_cursor_schema_failures_remain_invalid_arguments() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({"name": "repo.list", "arguments": {"max_results": 0}}),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);

        assert_eq!(
            result["structuredContent"]["error"]["code"],
            "INVALID_ARGUMENT"
        );
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
                        "arguments": retained_input("source.read")
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
                        "arguments": retained_input("source.read")
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
        let mut input = retained_input("source.read");
        input["references"][0]["source_ref"]["span"]["end_byte"] = json!(source_bytes);
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "source.read",
                        "arguments": input
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
            explain: None,
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
        assert_eq!(
            advanced_invariant_error(&tight).map(|error| error.code()),
            Some(ErrorCode::CostLimit)
        );
        let mut generous = advanced_input(scan());
        generous.cost_limit = Some(1_000);
        assert!(advanced_invariants_are_valid(&generous));
    }

    #[tokio::test]
    async fn forbidden_operator_uses_authoritative_mapping() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "query.advanced",
                        "arguments": {
                            "repository": selector(),
                            "query": {"op": "execute", "command": "ignored"}
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        let direct: ErrorResponse = serde_json::from_value(result["structuredContent"].clone())
            .expect("forbidden operator uses the checked error contract");
        assert_eq!(
            direct.error,
            public_error(MappedDomainFailure::operator_forbidden("query"))
                .expect("authoritative operator mapping builds")
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn advanced_cost_limit_has_a_stable_domain_code() {
        let router = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let response = router
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "query.advanced",
                        "arguments": {
                            "repository": selector(),
                            "query": {"op": "scan", "entity": "function"},
                            "cost_limit": 1
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let result = success(response);
        assert_eq!(result["structuredContent"]["error"]["code"], "COST_LIMIT");
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn batch_plan_and_profile_failures_remain_top_level_domain_errors() {
        let developer = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Developer)
            .expect("registry compiles");
        let invalid_plan = developer
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "query.batch",
                        "arguments": {
                            "repository": selector(),
                            "operations": [
                                {
                                    "id": "first",
                                    "tool": "code.locate",
                                    "depends_on": ["second"],
                                    "arguments": {"query": "publish"}
                                },
                                {
                                    "id": "second",
                                    "tool": "code.locate",
                                    "depends_on": ["first"],
                                    "arguments": {"query": "stage"}
                                }
                            ]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let invalid_plan = success(invalid_plan);
        let invalid_plan: ErrorResponse =
            serde_json::from_value(invalid_plan["structuredContent"].clone())
                .expect("batch plan failure uses the checked error contract");
        assert_eq!(
            invalid_plan.error,
            public_error(MappedDomainFailure::invalid_argument("operations"))
                .expect("authoritative batch plan mapping builds")
        );

        let scout = ToolRouter::new(FixtureExecutor::default(), ExposureProfile::Scout)
            .expect("registry compiles");
        let hidden = scout
            .handle(
                request(
                    "tools/call",
                    json!({
                        "name": "query.batch",
                        "arguments": {
                            "repository": selector(),
                            "operations": [
                                {
                                    "id": "relations",
                                    "tool": "symbol.relationships",
                                    "arguments": {}
                                }
                            ]
                        }
                    }),
                ),
                cancellation(),
            )
            .await;
        let hidden = success(hidden);
        let hidden: ErrorResponse = serde_json::from_value(hidden["structuredContent"].clone())
            .expect("batch profile failure uses the checked error contract");
        assert_eq!(
            hidden.error,
            public_error(MappedDomainFailure::unsupported_capability("operations"))
                .expect("authoritative profile mapping builds")
        );
        assert_eq!(developer.executor.calls.load(Ordering::Relaxed), 0);
        assert_eq!(scout.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn versioned_error_goldens_are_checked_source_free_envelopes() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/errors/mcp-error-goldens-1.0.json"
        ))
        .expect("checked error goldens are valid JSON");
        let observed: Vec<ErrorCode> = fixture["envelopes"]
            .as_array()
            .expect("golden envelopes are an array")
            .iter()
            .map(|envelope| {
                serde_json::from_value::<ErrorResponse>(envelope.clone())
                    .expect("golden envelope satisfies the checked Rust contract")
                    .error
                    .code()
            })
            .collect();
        assert_eq!(
            observed,
            [
                ErrorCode::InvalidCursor,
                ErrorCode::TypeMismatch,
                ErrorCode::BudgetExceeded,
                ErrorCode::CostLimit,
                ErrorCode::OperatorForbidden,
                ErrorCode::BindingInvalid,
                ErrorCode::BindingTypeMismatch,
            ]
        );

        let additive: ErrorResponse = serde_json::from_str(include_str!(
            "../../../tests/fixtures/errors/mcp-error-envelope-1.0-additive-details.json"
        ))
        .expect("additive detail fixture remains compatible");
        assert_eq!(additive.error.code(), ErrorCode::InvalidCursor);
        assert_eq!(additive.error.details().len(), 2);

        let encoded = serde_json::to_string(&fixture).expect("goldens serialize");
        for forbidden in ["C:\\", "/home/", "BEGIN PRIVATE KEY", "gho_"] {
            assert!(!encoded.contains(forbidden));
        }
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
