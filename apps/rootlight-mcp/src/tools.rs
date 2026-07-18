//! Bounded MCP tool discovery and invocation routing.

use std::{fmt, future::Future, pin::Pin, sync::Arc};

use jsonschema::Validator;
use rootlight_mcp_contract::VerticalTool;
use serde::Serialize;
use serde_json::{Map, Value, json};
use thiserror::Error;

use super::{
    HandlerCapabilities, HandlerFuture, HandlerResponse, INVALID_PARAMS, METHOD_NOT_FOUND,
    OperatingRequest, RequestCancellation, RequestHandler,
};

const INTERNAL_ERROR: i32 = -32_603;
const MAX_TOOL_NAME_BYTES: usize = 128;
const INVALID_ARGUMENT_CODE: &str = "INVALID_ARGUMENT";
const INVALID_ARGUMENT_MESSAGE: &str = "tool arguments do not match the input schema";

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

/// Source-free domain failure returned as an MCP tool execution error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolExecutionError {
    code: &'static str,
    message: &'static str,
}

impl ToolExecutionError {
    /// Creates a source-free stable tool error.
    #[must_use]
    pub const fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }

    /// Stable public error code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        self.code
    }

    /// Static source-free message.
    #[must_use]
    pub const fn message(self) -> &'static str {
        self.message
    }
}

/// Handler that advertises and routes the strict first-slice tool catalog.
pub struct ToolRouter<E> {
    executor: Arc<E>,
    contracts: Arc<[ToolContract]>,
    list_result: Map<String, Value>,
}

impl<E> ToolRouter<E>
where
    E: ToolExecutor,
{
    /// Compiles every checked input and output schema before the session starts.
    ///
    /// # Errors
    ///
    /// Returns [`ToolRegistryError`] when a checked server-owned schema cannot
    /// be parsed, compiled, or represented as an MCP tool definition.
    pub fn new(executor: E) -> Result<Self, ToolRegistryError> {
        let mut contracts = Vec::new();
        contracts
            .try_reserve_exact(VerticalTool::ALL.len())
            .map_err(|_| ToolRegistryError::MemoryUnavailable)?;
        for tool in VerticalTool::ALL {
            contracts.push(ToolContract::compile(tool)?);
        }

        let mut definitions = Vec::new();
        definitions
            .try_reserve_exact(contracts.len())
            .map_err(|_| ToolRegistryError::MemoryUnavailable)?;
        for contract in &contracts {
            definitions.push(
                serde_json::to_value(&contract.definition)
                    .map_err(ToolRegistryError::SerializeDefinition)?,
            );
        }
        let list_result = Map::from_iter([("tools".to_owned(), Value::Array(definitions))]);

        Ok(Self {
            executor: Arc::new(executor),
            contracts: contracts.into(),
            list_result,
        })
    }

    fn list_tools(&self, params: Option<Value>) -> HandlerResponse {
        if !list_params_are_valid(params.as_ref()) {
            return HandlerResponse::error(INVALID_PARAMS, "invalid tools/list parameters");
        }
        HandlerResponse::Success(self.list_result.clone())
    }

    async fn call_tool(
        executor: Arc<E>,
        contracts: Arc<[ToolContract]>,
        params: Option<Value>,
        cancellation: RequestCancellation,
    ) -> HandlerResponse {
        let (name, arguments) = match decode_call_params(params) {
            Ok(decoded) => decoded,
            Err(()) => {
                return HandlerResponse::error(INVALID_PARAMS, "invalid tools/call parameters");
            }
        };
        let Some(contract) = contracts
            .iter()
            .find(|contract| contract.tool.name() == name)
        else {
            return HandlerResponse::error(METHOD_NOT_FOUND, "tool is not available");
        };
        let arguments_value = Value::Object(arguments);
        if !contract.input_validator.is_valid(&arguments_value) {
            return tool_error(INVALID_ARGUMENT_CODE, INVALID_ARGUMENT_MESSAGE);
        }
        let Value::Object(arguments) = arguments_value else {
            return HandlerResponse::error(INTERNAL_ERROR, "tool input invariant failed");
        };
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }

        let execution = executor
            .execute(contract.tool, arguments, cancellation.clone())
            .await;
        if cancellation.is_cancelled() {
            return HandlerResponse::Cancelled;
        }
        let output = match execution {
            Ok(output) => output,
            Err(error) => return tool_error(error.code(), error.message()),
        };
        let output_value = Value::Object(output);
        if !contract.output_validator.is_valid(&output_value) {
            return HandlerResponse::error(INTERNAL_ERROR, "tool output failed validation");
        }
        tool_success(output_value)
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
                Box::pin(
                    async move { Self::call_tool(executor, contracts, params, cancellation).await },
                )
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
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolAnnotations {
    read_only_hint: bool,
    destructive_hint: bool,
    idempotent_hint: bool,
    open_world_hint: bool,
}

fn parse_object_schema(
    _tool: VerticalTool,
    _direction: &'static str,
    schema: &'static str,
) -> Result<Map<String, Value>, serde_json::Error> {
    serde_json::from_str(schema)
}

fn list_params_are_valid(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return true;
    };
    let Some(params) = params.as_object() else {
        return false;
    };
    params.keys().all(|key| key == "_meta")
        && params.get("_meta").is_none_or(serde_json::Value::is_object)
}

fn decode_call_params(params: Option<Value>) -> Result<(String, Map<String, Value>), ()> {
    let Some(Value::Object(mut params)) = params else {
        return Err(());
    };
    if params
        .keys()
        .any(|key| !matches!(key.as_str(), "_meta" | "name" | "arguments"))
        || params.get("_meta").is_some_and(|value| !value.is_object())
    {
        return Err(());
    }
    let Some(Value::String(name)) = params.remove("name") else {
        return Err(());
    };
    if name.is_empty() || name.len() > MAX_TOOL_NAME_BYTES {
        return Err(());
    }
    let arguments = match params.remove("arguments") {
        None => Map::new(),
        Some(Value::Object(arguments)) => arguments,
        Some(_) => return Err(()),
    };
    Ok((name, arguments))
}

fn tool_success(structured: Value) -> HandlerResponse {
    let text = match serde_json::to_string(&structured) {
        Ok(text) => text,
        Err(_) => {
            return HandlerResponse::error(INTERNAL_ERROR, "tool output serialization failed");
        }
    };
    HandlerResponse::Success(Map::from_iter([
        ("content".to_owned(), text_content(text)),
        ("structuredContent".to_owned(), structured),
        ("isError".to_owned(), Value::Bool(false)),
    ]))
}

fn tool_error(code: &'static str, message: &'static str) -> HandlerResponse {
    let structured = json!({"error": {"code": code, "message": message}});
    let text = match serde_json::to_string(&structured) {
        Ok(text) => text,
        Err(_) => {
            return HandlerResponse::error(INTERNAL_ERROR, "tool error serialization failed");
        }
    };
    HandlerResponse::Success(Map::from_iter([
        ("content".to_owned(), text_content(text)),
        ("structuredContent".to_owned(), structured),
        ("isError".to_owned(), Value::Bool(true)),
    ]))
}

fn text_content(text: String) -> Value {
    json!([{"type": "text", "text": text}])
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
                            "operation_id": "op1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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

    fn cancellation() -> RequestCancellation {
        let (_sender, receiver) = watch::channel(false);
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

    #[tokio::test]
    async fn tools_list_is_fixed_strict_and_truthfully_annotated() {
        let router = ToolRouter::new(FixtureExecutor::default()).expect("registry compiles");
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
        assert_eq!(tools[4]["name"], "source.read");
        for tool in tools {
            assert_eq!(tool["inputSchema"]["type"], "object");
            assert_eq!(tool["outputSchema"]["type"], "object");
            assert_eq!(tool["annotations"]["openWorldHint"], false);
            assert_eq!(tool["annotations"]["destructiveHint"], false);
        }
        assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
        assert_eq!(tools[2]["annotations"]["readOnlyHint"], true);
    }

    #[tokio::test]
    async fn tools_call_validates_output_and_mirrors_exact_structured_content() {
        let router = ToolRouter::new(FixtureExecutor::default()).expect("registry compiles");
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
        let router = ToolRouter::new(FixtureExecutor::default()).expect("registry compiles");
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
            INVALID_ARGUMENT_CODE
        );
        assert_eq!(router.executor.calls.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn invalid_server_output_fails_as_a_protocol_internal_error() {
        let router = ToolRouter::new(FixtureExecutor::default()).expect("registry compiles");
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
                                "symbol_id": "sym1_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
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
}
