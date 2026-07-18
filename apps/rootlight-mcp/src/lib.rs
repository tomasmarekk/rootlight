//! Bounded MCP 2025-11-25 standard-stream transport and first-slice bridge.
//!
//! This crate owns JSON-RPC framing, lifecycle state, cancellation routing,
//! bounded transport orchestration, and the source-redacted adapter between
//! the five public tools and Rootlight's asynchronous daemon client.

#![forbid(unsafe_code)]

mod client_port;
mod executor;
mod json;
mod tools;

use std::{cmp::Ordering, collections::BTreeMap, fmt, future::Future, io, pin::Pin, sync::Arc};

use json::{JsonIssue, JsonLimits, ParseFailure, parse_bounded};
use rootlight_mcp_contract::MCP_SPECIFICATION_DATE;
use serde::Serialize;
use serde_json::{Map, Number, Value};
use thiserror::Error;
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt},
    sync::{Semaphore, mpsc, watch},
    task::{JoinHandle, JoinSet},
};

pub use client_port::{NativeFirstSliceClientPort, UnavailableFirstSliceClientPort};
pub use executor::{
    ClientPortError, ClientPortFuture, CodeLocatePortRequest, CodeLocatePortResponse,
    FirstSliceClientPort, FirstSliceToolExecutor, OperationStatusPortRequest, ReadResponseMetadata,
    RepositoryIndexPortRequest, RepositoryIndexPortResponse, SourceReadPortRequest,
    SourceReadPortResponse, SymbolExplainPortRequest, SymbolExplainPortResponse,
    ToolExecutorBuildError,
};
pub use tools::{
    ToolExecutionError, ToolExecutionFailure, ToolExecutionFuture, ToolExecutor, ToolRegistryError,
    ToolRouter,
};

const JSON_RPC_VERSION: &str = "2.0";
const PARSE_ERROR: i32 = -32_700;
const INVALID_REQUEST: i32 = -32_600;
const METHOD_NOT_FOUND: i32 = -32_601;
const INVALID_PARAMS: i32 = -32_602;
const SERVER_BUSY: i32 = -32_000;
const SERVER_NOT_INITIALIZED: i32 = -32_002;
const REQUEST_CANCELLED: i32 = -32_800;

/// Default maximum bytes in one newline-delimited standard-stream message.
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Default maximum bytes emitted for one JSON-RPC response, including newline.
pub const DEFAULT_MAX_RESPONSE_BYTES: usize = 1024 * 1024;
/// Default maximum JSON object or array nesting below the top-level value.
pub const DEFAULT_MAX_JSON_DEPTH: usize = 32;
/// Maximum configured JSON depth supported by the bounded recursive visitor.
pub const MAX_SUPPORTED_JSON_DEPTH: usize = 64;
/// Default maximum UTF-8 bytes in one JSON string or object key.
pub const DEFAULT_MAX_STRING_BYTES: usize = 64 * 1024;
/// Default maximum raw properties accepted in one JSON object.
pub const DEFAULT_MAX_OBJECT_PROPERTIES: usize = 128;
/// Default maximum values accepted in one JSON array.
pub const DEFAULT_MAX_ARRAY_ITEMS: usize = 256;
/// Default maximum aggregate JSON values accepted in one message.
pub const DEFAULT_MAX_JSON_NODES: usize = 16 * 1024;
/// Default invalid-message count tolerated before closing the session.
pub const DEFAULT_MAX_INVALID_MESSAGES: usize = 8;
/// Default maximum concurrently executing MCP requests.
pub const DEFAULT_MAX_IN_FLIGHT_REQUESTS: usize = 8;
/// Default number of fully encoded responses that may await emission.
pub const DEFAULT_RESPONSE_CHANNEL_CAPACITY: usize = 16;
/// Default maximum blocking workers available to a service integration.
pub const DEFAULT_MAX_BLOCKING_WORKERS: usize = 4;

const MAX_METHOD_BYTES: usize = 256;
pub(crate) const MAX_REQUEST_ID_BYTES: usize = 4_096;
const MAX_IMPLEMENTATION_NAME_BYTES: usize = 256;
const MAX_IMPLEMENTATION_VERSION_BYTES: usize = 256;
const MAX_IMPLEMENTATION_TITLE_BYTES: usize = 512;
const MAX_IMPLEMENTATION_DESCRIPTION_BYTES: usize = 4 * 1024;
const MAX_IMPLEMENTATION_ICONS: usize = 16;
const MAX_ICON_SOURCE_BYTES: usize = 4 * 1024;
const MAX_ICON_MIME_BYTES: usize = 256;
const MAX_ICON_SIZES: usize = 32;
const MAX_ICON_SIZE_BYTES: usize = 64;
const MAX_WEBSITE_BYTES: usize = 4 * 1024;
const MAX_CANCELLATION_REASON_BYTES: usize = 1024;

/// Hard limits applied before lifecycle or method dispatch.
///
/// The type is non-exhaustive so later tool-specific accounting can add limits
/// without making external default-based construction source-incompatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct StdioLimits {
    max_frame_bytes: usize,
    max_response_bytes: usize,
    max_json_depth: usize,
    max_string_bytes: usize,
    max_object_properties: usize,
    max_array_items: usize,
    max_json_nodes: usize,
    max_invalid_messages: usize,
    max_in_flight_requests: usize,
    response_channel_capacity: usize,
    max_blocking_workers: usize,
}

impl Default for StdioLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
            max_string_bytes: DEFAULT_MAX_STRING_BYTES,
            max_object_properties: DEFAULT_MAX_OBJECT_PROPERTIES,
            max_array_items: DEFAULT_MAX_ARRAY_ITEMS,
            max_json_nodes: DEFAULT_MAX_JSON_NODES,
            max_invalid_messages: DEFAULT_MAX_INVALID_MESSAGES,
            max_in_flight_requests: DEFAULT_MAX_IN_FLIGHT_REQUESTS,
            response_channel_capacity: DEFAULT_RESPONSE_CHANNEL_CAPACITY,
            max_blocking_workers: DEFAULT_MAX_BLOCKING_WORKERS,
        }
    }
}

impl StdioLimits {
    /// Overrides the frame ceiling for a constrained embedding or test.
    #[must_use]
    pub fn with_max_frame_bytes(mut self, maximum: usize) -> Self {
        self.max_frame_bytes = maximum;
        self
    }

    /// Overrides the response ceiling for a constrained embedding or test.
    #[must_use]
    pub fn with_max_response_bytes(mut self, maximum: usize) -> Self {
        self.max_response_bytes = maximum;
        self
    }

    /// Overrides the accepted JSON nesting depth.
    #[must_use]
    pub fn with_max_json_depth(mut self, maximum: usize) -> Self {
        self.max_json_depth = maximum;
        self
    }

    /// Overrides the maximum UTF-8 bytes in one string or object key.
    #[must_use]
    pub fn with_max_string_bytes(mut self, maximum: usize) -> Self {
        self.max_string_bytes = maximum;
        self
    }

    /// Overrides the per-object raw property ceiling.
    #[must_use]
    pub fn with_max_object_properties(mut self, maximum: usize) -> Self {
        self.max_object_properties = maximum;
        self
    }

    /// Overrides the per-array item ceiling.
    #[must_use]
    pub fn with_max_array_items(mut self, maximum: usize) -> Self {
        self.max_array_items = maximum;
        self
    }

    /// Overrides the aggregate JSON node ceiling.
    #[must_use]
    pub fn with_max_json_nodes(mut self, maximum: usize) -> Self {
        self.max_json_nodes = maximum;
        self
    }

    /// Overrides the number of invalid messages tolerated by [`serve`].
    #[must_use]
    pub fn with_max_invalid_messages(mut self, maximum: usize) -> Self {
        self.max_invalid_messages = maximum;
        self
    }

    /// Overrides the maximum number of concurrently executing requests.
    #[must_use]
    pub fn with_max_in_flight_requests(mut self, maximum: usize) -> Self {
        self.max_in_flight_requests = maximum;
        self
    }

    /// Overrides the number of encoded responses awaiting standard output.
    #[must_use]
    pub fn with_response_channel_capacity(mut self, maximum: usize) -> Self {
        self.response_channel_capacity = maximum;
        self
    }

    /// Overrides the maximum parallelism of a derived blocking worker pool.
    #[must_use]
    pub fn with_max_blocking_workers(mut self, maximum: usize) -> Self {
        self.max_blocking_workers = maximum;
        self
    }

    /// Creates the bounded blocking boundary selected by these limits.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::InvalidLimits`] when any limit is invalid.
    pub fn blocking_pool(self) -> Result<BoundedBlockingPool, SessionError> {
        let limits = self.validate()?;
        Ok(BoundedBlockingPool::new(limits.max_blocking_workers))
    }

    fn validate(self) -> Result<Self, SessionError> {
        if self.max_frame_bytes == 0
            || self.max_response_bytes == 0
            || self.max_json_depth == 0
            || self.max_json_depth > MAX_SUPPORTED_JSON_DEPTH
            || self.max_string_bytes == 0
            || self.max_object_properties == 0
            || self.max_array_items == 0
            || self.max_json_nodes == 0
            || self.max_invalid_messages == 0
            || self.max_in_flight_requests == 0
            || self.response_channel_capacity == 0
            || self.response_channel_capacity > Semaphore::MAX_PERMITS
            || self.max_blocking_workers == 0
            || self.max_blocking_workers > Semaphore::MAX_PERMITS
        {
            return Err(SessionError::InvalidLimits);
        }
        Ok(self)
    }

    const fn json_limits(self) -> JsonLimits {
        JsonLimits {
            max_depth: self.max_json_depth,
            max_string_bytes: self.max_string_bytes,
            max_object_properties: self.max_object_properties,
            max_array_items: self.max_array_items,
            max_nodes: self.max_json_nodes,
        }
    }
}

/// JSON-RPC request identity accepted by MCP.
#[derive(Clone, Serialize)]
#[serde(untagged)]
pub enum RequestId {
    /// Numeric identity preserved in serde_json's accepted representation.
    Number(Number),
    /// String identity bounded by the configured JSON string limit.
    String(String),
}

impl fmt::Debug for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(_) => formatter.write_str("RequestId::Number(<redacted>)"),
            Self::String(value) => formatter
                .debug_struct("RequestId::String")
                .field("byte_length", &value.len())
                .finish(),
        }
    }
}

impl RequestId {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) if number.to_string().len() <= MAX_REQUEST_ID_BYTES => {
                Some(Self::Number(number.clone()))
            }
            Value::String(value) if value.len() <= MAX_REQUEST_ID_BYTES => {
                Some(Self::String(value.clone()))
            }
            _ => None,
        }
    }
}

impl PartialEq for RequestId {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for RequestId {}

impl Ord for RequestId {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            // Comparing rendered numbers avoids collapsing distinct
            // arbitrary-precision representations through a lossy f64 key.
            (Self::Number(left), Self::Number(right)) => left.to_string().cmp(&right.to_string()),
            (Self::Number(_), Self::String(_)) => Ordering::Less,
            (Self::String(_), Self::Number(_)) => Ordering::Greater,
            (Self::String(left), Self::String(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for RequestId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Stateful MCP lifecycle dispatcher for one client connection.
pub struct Session {
    state: LifecycleState,
    capabilities: HandlerCapabilities,
}

impl Session {
    /// Creates Rootlight's lifecycle session with no unimplemented capability.
    #[must_use]
    pub const fn rootlight() -> Self {
        Self {
            state: LifecycleState::AwaitInitialize,
            capabilities: HandlerCapabilities::none(),
        }
    }

    /// Reports whether the initialization handshake reached operation state.
    #[must_use]
    pub const fn is_operating(&self) -> bool {
        matches!(self.state, LifecycleState::Operating)
    }

    /// Processes one complete lifecycle message without its newline delimiter.
    ///
    /// Operating requests use the default method-not-found behavior because this
    /// synchronous fixture entry point has no domain handler. The stream-level
    /// invalid-message threshold is enforced by [`serve`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] only for local serialization, limit, or memory
    /// failures. Malformed peer input produces a bounded JSON-RPC error.
    pub fn handle_frame(
        &mut self,
        frame: &[u8],
        limits: StdioLimits,
    ) -> Result<Option<Vec<u8>>, SessionError> {
        let limits = limits.validate()?;
        match self.process_frame(frame, limits)? {
            Dispatch::Immediate(processed) => Ok(processed.response),
            Dispatch::Start(request) => static_error(
                Some(&request.id),
                METHOD_NOT_FOUND,
                "method is not available",
                limits,
            ),
            Dispatch::Cancel(_) => Ok(None),
        }
    }

    fn process_frame(
        &mut self,
        frame: &[u8],
        limits: StdioLimits,
    ) -> Result<Dispatch, SessionError> {
        if frame.len() > limits.max_frame_bytes {
            return static_error(
                None,
                INVALID_REQUEST,
                "message exceeds the frame limit",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate);
        }

        let parsed = match parse_bounded(frame, limits.json_limits()) {
            Ok(parsed) => parsed,
            Err(ParseFailure::Malformed) => {
                return static_error(None, PARSE_ERROR, "malformed JSON", limits)
                    .map(ProcessedFrame::invalid)
                    .map(Dispatch::Immediate);
            }
            Err(ParseFailure::Rejected(issue)) => {
                let message = match issue {
                    JsonIssue::Limits => "JSON limits exceeded",
                    JsonIssue::DuplicateName => "duplicate JSON object name",
                };
                return static_error(None, INVALID_REQUEST, message, limits)
                    .map(ProcessedFrame::invalid)
                    .map(Dispatch::Immediate);
            }
            Err(ParseFailure::MemoryUnavailable) => {
                return Err(SessionError::MemoryUnavailable);
            }
        };

        let message = match decode_inbound(parsed) {
            Ok(message) => message,
            Err(issue) => {
                return static_error(
                    issue.id.as_ref(),
                    INVALID_REQUEST,
                    "invalid JSON-RPC message",
                    limits,
                )
                .map(ProcessedFrame::invalid)
                .map(Dispatch::Immediate);
            }
        };
        self.dispatch(message, limits)
    }

    fn dispatch(
        &mut self,
        message: InboundMessage,
        limits: StdioLimits,
    ) -> Result<Dispatch, SessionError> {
        match message.envelope {
            Envelope::Request(id) => {
                self.dispatch_request(id, message.method, message.params, limits)
            }
            Envelope::Notification => self.dispatch_notification(message.method, message.params),
        }
    }

    fn dispatch_request(
        &mut self,
        id: RequestId,
        method: String,
        params: Option<Value>,
        limits: StdioLimits,
    ) -> Result<Dispatch, SessionError> {
        match method.as_str() {
            "initialize" => self.handle_initialize(id, params, limits),
            "ping" => self.handle_ping(id, params, limits),
            "notifications/initialized" | "notifications/cancelled" => static_error(
                Some(&id),
                INVALID_REQUEST,
                "notification method used as a request",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate),
            _ if !matches!(self.state, LifecycleState::Operating) => static_error(
                Some(&id),
                SERVER_NOT_INITIALIZED,
                "server is not initialized",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate),
            _ if params.as_ref().is_some_and(|value| !value.is_object()) => static_error(
                Some(&id),
                INVALID_REQUEST,
                "request parameters must be an object",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate),
            _ => Ok(Dispatch::Start(OperatingRequest { id, method, params })),
        }
    }

    fn dispatch_notification(
        &mut self,
        method: String,
        params: Option<Value>,
    ) -> Result<Dispatch, SessionError> {
        match method.as_str() {
            "notifications/cancelled" => Ok(Dispatch::Cancel(decode_cancellation(params.as_ref()))),
            "notifications/initialized" => {
                if matches!(self.state, LifecycleState::AwaitInitialized)
                    && initialized_params_are_valid(params.as_ref())
                {
                    self.state = LifecycleState::Operating;
                    Ok(Dispatch::Immediate(ProcessedFrame::valid(None)))
                } else {
                    Ok(Dispatch::Immediate(ProcessedFrame::invalid(None)))
                }
            }
            "initialize" | "ping" => Ok(Dispatch::Immediate(ProcessedFrame::invalid(None))),
            _ if matches!(self.state, LifecycleState::Operating) => {
                Ok(Dispatch::Immediate(ProcessedFrame::valid(None)))
            }
            _ => Ok(Dispatch::Immediate(ProcessedFrame::invalid(None))),
        }
    }

    fn handle_ping(
        &self,
        id: RequestId,
        params: Option<Value>,
        limits: StdioLimits,
    ) -> Result<Dispatch, SessionError> {
        if matches!(self.state, LifecycleState::AwaitInitialize) {
            return static_error(
                Some(&id),
                SERVER_NOT_INITIALIZED,
                "server is not initialized",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate);
        }
        if !request_params_are_valid(params.as_ref()) {
            return static_error(Some(&id), INVALID_PARAMS, "invalid ping parameters", limits)
                .map(ProcessedFrame::invalid)
                .map(Dispatch::Immediate);
        }
        result_response(&id, &EmptyObject {}, limits)
            .map(|response| Dispatch::Immediate(ProcessedFrame::valid(Some(response))))
    }

    fn handle_initialize(
        &mut self,
        id: RequestId,
        params: Option<Value>,
        limits: StdioLimits,
    ) -> Result<Dispatch, SessionError> {
        if !matches!(self.state, LifecycleState::AwaitInitialize) {
            return static_error(
                Some(&id),
                INVALID_REQUEST,
                "initialize is already complete",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate);
        }
        if !initialize_params_are_valid(params.as_ref()) {
            return static_error(
                Some(&id),
                INVALID_PARAMS,
                "invalid initialize parameters",
                limits,
            )
            .map(ProcessedFrame::invalid)
            .map(Dispatch::Immediate);
        }

        // The server returns its supported revision when the requested revision
        // differs; the client then decides whether it can continue.
        let result = InitializeResult {
            protocol_version: MCP_SPECIFICATION_DATE,
            capabilities: ServerCapabilities {
                tools: self.capabilities.tools.then_some(ToolsCapability {
                    list_changed: false,
                }),
            },
            server_info: ServerImplementation {
                name: "rootlight",
                title: "Rootlight",
                version: env!("CARGO_PKG_VERSION"),
                description: "Local-first repository intelligence MCP bridge",
            },
        };
        let response = result_response(&id, &result, limits)?;
        self.state = LifecycleState::AwaitInitialized;
        Ok(Dispatch::Immediate(ProcessedFrame::valid(Some(response))))
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::rootlight()
    }
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("state", &self.state)
            .finish()
    }
}

/// One operating-phase request passed to a service integration.
pub struct OperatingRequest {
    id: RequestId,
    method: String,
    params: Option<Value>,
}

impl OperatingRequest {
    /// Returns the requested method.
    #[must_use]
    pub fn method(&self) -> &str {
        &self.method
    }

    /// Returns the optional object-valued request parameters.
    #[must_use]
    pub const fn params(&self) -> Option<&Value> {
        self.params.as_ref()
    }

    fn into_method_params(self) -> (String, Option<Value>) {
        (self.method, self.params)
    }
}

impl fmt::Debug for OperatingRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OperatingRequest")
            .field("id", &self.id)
            .field("method_byte_length", &self.method.len())
            .field("has_params", &self.params.is_some())
            .finish()
    }
}

/// Cancellation signal scoped to one in-flight request.
#[derive(Clone)]
pub struct RequestCancellation {
    receiver: watch::Receiver<bool>,
}

impl RequestCancellation {
    /// Reports whether cancellation has already been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Waits until cancellation is requested or the transport closes.
    pub async fn cancelled(&mut self) {
        if self.is_cancelled() {
            return;
        }
        while self.receiver.changed().await.is_ok() {
            if self.is_cancelled() {
                return;
            }
        }
    }
}

impl fmt::Debug for RequestCancellation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestCancellation")
            .field("is_cancelled", &self.is_cancelled())
            .finish()
    }
}

/// Bounded result returned by an operating request handler.
pub enum HandlerResponse {
    /// Successful object-valued MCP result.
    Success(Map<String, Value>),
    /// Source-free protocol or service error.
    Error {
        /// Stable integer error code.
        code: i32,
        /// Concise static message that cannot contain peer or repository input.
        message: &'static str,
    },
    /// Request processing stopped after a cancellation notification.
    Cancelled,
}

impl HandlerResponse {
    /// Creates a successful empty result.
    #[must_use]
    pub fn empty_success() -> Self {
        Self::Success(Map::new())
    }

    /// Creates a source-free error response.
    #[must_use]
    pub const fn error(code: i32, message: &'static str) -> Self {
        Self::Error { code, message }
    }

    /// Creates the standard cancellation error when a caller elects to reply.
    ///
    /// MCP normally recommends [`HandlerResponse::Cancelled`], which emits no
    /// response. This constructor exists for integrations whose operation
    /// contract explicitly requires a terminal JSON-RPC error.
    #[must_use]
    pub const fn cancellation_error() -> Self {
        Self::Error {
            code: REQUEST_CANCELLED,
            message: "request cancelled",
        }
    }
}

impl fmt::Debug for HandlerResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success(result) => formatter
                .debug_struct("HandlerResponse::Success")
                .field("property_count", &result.len())
                .finish(),
            Self::Error { code, .. } => formatter
                .debug_struct("HandlerResponse::Error")
                .field("code", code)
                .finish_non_exhaustive(),
            Self::Cancelled => formatter.write_str("HandlerResponse::Cancelled"),
        }
    }
}

/// Boxed future returned by [`RequestHandler`].
pub type HandlerFuture = Pin<Box<dyn Future<Output = HandlerResponse> + Send + 'static>>;

/// Operating-phase request handler supplied by the later service layer.
///
/// Implementations must keep async work cancellation-safe. CPU-bound or
/// blocking work must cross [`BoundedBlockingPool`] rather than running on the
/// Tokio worker or spawning an unbounded blocking task.
pub trait RequestHandler: Send + Sync + 'static {
    /// Reports only the capabilities this handler serves for the full session.
    fn capabilities(&self) -> HandlerCapabilities {
        HandlerCapabilities::none()
    }

    /// Begins one bounded operating-phase request.
    fn handle(&self, request: OperatingRequest, cancellation: RequestCancellation)
    -> HandlerFuture;
}

/// MCP capabilities backed by one operating-phase handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandlerCapabilities {
    tools: bool,
}

impl HandlerCapabilities {
    /// No optional server capability.
    #[must_use]
    pub const fn none() -> Self {
        Self { tools: false }
    }

    /// The handler serves both tool discovery and invocation.
    #[must_use]
    pub const fn tools() -> Self {
        Self { tools: true }
    }
}

/// Default handler used while Rootlight advertises no domain capabilities.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRequestHandler;

impl RequestHandler for NoopRequestHandler {
    fn handle(
        &self,
        _request: OperatingRequest,
        _cancellation: RequestCancellation,
    ) -> HandlerFuture {
        Box::pin(async {
            HandlerResponse::Error {
                code: METHOD_NOT_FOUND,
                message: "method is not available",
            }
        })
    }
}

/// Bounded gateway for CPU-bound or blocking service work.
#[derive(Clone)]
pub struct BoundedBlockingPool {
    permits: Arc<Semaphore>,
}

impl BoundedBlockingPool {
    fn new(maximum: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(maximum)),
        }
    }

    /// Runs one blocking closure after acquiring a bounded worker permit.
    ///
    /// The permit is held inside the blocking closure, so cancelling the async
    /// waiter cannot create unbounded detached blocking work.
    ///
    /// # Errors
    ///
    /// Returns [`BlockingTaskError`] when the pool closes or the worker panics.
    pub async fn run<F, T>(&self, work: F) -> Result<T, BlockingTaskError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let permit = Arc::clone(&self.permits)
            .acquire_owned()
            .await
            .map_err(|_| BlockingTaskError::Closed)?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        })
        .await
        .map_err(|_| BlockingTaskError::WorkerFailed)
    }
}

impl fmt::Debug for BoundedBlockingPool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundedBlockingPool")
            .field("available_permits", &self.permits.available_permits())
            .finish()
    }
}

/// Failure from a [`BoundedBlockingPool`] operation.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum BlockingTaskError {
    /// The bounded pool was closed.
    #[error("blocking worker pool is closed")]
    Closed,
    /// The blocking closure panicked or its worker could not complete.
    #[error("blocking worker failed")]
    WorkerFailed,
}

/// Serves one MCP stdio session until clean EOF or a local transport failure.
///
/// Input, executing requests, and encoded responses all have independent hard
/// bounds. A dedicated bounded writer keeps stdout protocol-only while the read
/// loop remains able to route cancellation notifications to in-flight work.
///
/// # Errors
///
/// Returns [`SessionError`] for local I/O, serialization, memory, configuration,
/// task, backpressure, or repeated-invalid-input limits. Peer parse and method
/// errors are returned as JSON-RPC responses where the protocol permits.
pub async fn serve<R, W>(
    input: R,
    output: W,
    session: &mut Session,
    handler: Arc<dyn RequestHandler>,
    limits: StdioLimits,
) -> Result<(), SessionError>
where
    R: AsyncBufRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let limits = limits.validate()?;
    if !matches!(session.state, LifecycleState::AwaitInitialize) {
        return Err(SessionError::SessionAlreadyStarted);
    }
    session.capabilities = handler.capabilities();
    let mut frames = FrameReader::new(input, limits.max_frame_bytes);
    let (response_tx, response_rx) = mpsc::channel(limits.response_channel_capacity);
    let mut writer = AbortOnDropTask::new(tokio::spawn(write_responses(output, response_rx)));
    let mut writer_finished = false;
    let mut requests = JoinSet::<(RequestId, HandlerResponse)>::new();
    let mut in_flight = BTreeMap::<RequestId, watch::Sender<bool>>::new();
    let mut invalid_messages = 0usize;

    // Keep every fallible loop exit inside this scope so task and channel
    // cleanup below also runs after I/O, handler, and serialization failures.
    let result = async {
        loop {
            tokio::select! {
                writer_result = writer.join() => {
                    writer_finished = true;
                    break flatten_writer_result(writer_result);
                }
                completed = requests.join_next(), if !requests.is_empty() => {
                    let (id, response) = completed
                        .ok_or(SessionError::RequestTaskFailed)?
                        .map_err(|_| SessionError::RequestTaskFailed)?;
                    in_flight.remove(&id);
                    if let Some(encoded) = encode_handler_response(&id, response, limits)? {
                        enqueue_response(&response_tx, encoded)?;
                    }
                }
                frame = frames.read_next() => {
                    let frame = frame?;
                    let terminal =
                        matches!(frame, ReadFrame::Oversized | ReadFrame::Unterminated);
                    let dispatch = match frame {
                        ReadFrame::Eof => break Ok(()),
                        ReadFrame::Complete(frame) => session.process_frame(&frame, limits)?,
                        ReadFrame::Oversized => Dispatch::Immediate(ProcessedFrame::invalid(
                            static_error(
                                None,
                                INVALID_REQUEST,
                                "message exceeds the frame limit",
                                limits,
                            )?,
                        )),
                        ReadFrame::Unterminated => Dispatch::Immediate(ProcessedFrame::invalid(
                            static_error(
                                None,
                                PARSE_ERROR,
                                "unterminated standard-stream message",
                                limits,
                            )?,
                        )),
                    };

                    let invalid = match dispatch {
                        Dispatch::Immediate(processed) => {
                            if let Some(response) = processed.response {
                                enqueue_response(&response_tx, response)?;
                            }
                            processed.invalid
                        }
                        Dispatch::Cancel(request_id) => {
                            if let Some(sender) = request_id.and_then(|id| in_flight.get(&id)) {
                                let _ignored = sender.send(true);
                            }
                            false
                        }
                        Dispatch::Start(request) => {
                            if in_flight.contains_key(&request.id) {
                                let response = static_error(
                                    Some(&request.id),
                                    INVALID_REQUEST,
                                    "request identity is already in flight",
                                    limits,
                                )?
                                .ok_or(SessionError::SerializationInvariant)?;
                                enqueue_response(&response_tx, response)?;
                                true
                            } else if in_flight.len() >= limits.max_in_flight_requests {
                                let response = static_error(
                                    Some(&request.id),
                                    SERVER_BUSY,
                                    "in-flight request limit reached",
                                    limits,
                                )?
                                .ok_or(SessionError::SerializationInvariant)?;
                                enqueue_response(&response_tx, response)?;
                                false
                            } else {
                                let (cancel_tx, cancel_rx) = watch::channel(false);
                                in_flight.insert(request.id.clone(), cancel_tx);
                                let handler = Arc::clone(&handler);
                                requests.spawn(async move {
                                    let id = request.id.clone();
                                    let response = handler
                                        .handle(
                                            request,
                                            RequestCancellation {
                                                receiver: cancel_rx,
                                            },
                                        )
                                        .await;
                                    (id, response)
                                });
                                false
                            }
                        }
                    };

                    if invalid {
                        invalid_messages = invalid_messages
                            .checked_add(1)
                            .ok_or(SessionError::TooManyInvalidMessages)?;
                        if invalid_messages >= limits.max_invalid_messages {
                            break Err(SessionError::TooManyInvalidMessages);
                        }
                    }
                    if terminal {
                        break Ok(());
                    }
                }
            }
        }
    }
    .await;

    for cancellation in in_flight.values() {
        let _ignored = cancellation.send(true);
    }
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    drop(response_tx);

    if !writer_finished {
        let writer_result = writer.join().await;
        if result.is_ok() {
            return flatten_writer_result(writer_result);
        }
    }
    result
}

// Tokio detaches a JoinHandle when it is dropped. This owner aborts the
// response writer if an embedding cancels `serve` before its async cleanup.
struct AbortOnDropTask<T> {
    handle: JoinHandle<T>,
}

impl<T> AbortOnDropTask<T> {
    const fn new(handle: JoinHandle<T>) -> Self {
        Self { handle }
    }

    async fn join(&mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.handle).await
    }
}

impl<T> Drop for AbortOnDropTask<T> {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Local failures that terminate an MCP stdio session.
#[derive(Error)]
pub enum SessionError {
    /// Standard-stream I/O failed.
    #[error("standard-stream I/O failed")]
    Io(#[source] io::Error),
    /// A server-owned response could not be serialized.
    #[error("MCP response serialization failed")]
    Serialization(#[source] serde_json::Error),
    /// A response exceeded the configured output ceiling.
    #[error("MCP response exceeded its configured limit")]
    ResponseTooLarge,
    /// A bounded allocation could not be reserved.
    #[error("MCP transport memory is unavailable")]
    MemoryUnavailable,
    /// A transport limit was configured outside the supported range.
    #[error("MCP transport limits are invalid")]
    InvalidLimits,
    /// The peer exceeded the invalid-message allowance.
    #[error("MCP invalid-message limit exceeded")]
    TooManyInvalidMessages,
    /// The bounded response queue could not accept a protocol response.
    #[error("MCP response queue is unavailable")]
    ResponseBackpressure,
    /// The stdout writer task terminated unexpectedly.
    #[error("MCP response writer failed")]
    WriterTaskFailed,
    /// An in-flight request task terminated unexpectedly.
    #[error("MCP request task failed")]
    RequestTaskFailed,
    /// A private serialization helper violated its response invariant.
    #[error("MCP response invariant failed")]
    SerializationInvariant,
    /// A serve loop was started after lifecycle processing began.
    #[error("MCP session has already started")]
    SessionAlreadyStarted,
}

impl fmt::Debug for SessionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionError")
            .field("category", &self.category())
            .finish()
    }
}

impl SessionError {
    /// Returns a stable source-free category suitable for process diagnostics.
    #[must_use]
    pub const fn category(&self) -> &'static str {
        match self {
            Self::Io(_) => "io",
            Self::Serialization(_) => "serialization",
            Self::ResponseTooLarge => "response_limit",
            Self::MemoryUnavailable => "memory",
            Self::InvalidLimits => "configuration",
            Self::TooManyInvalidMessages => "protocol_limit",
            Self::ResponseBackpressure => "response_backpressure",
            Self::WriterTaskFailed => "writer_task",
            Self::RequestTaskFailed => "request_task",
            Self::SerializationInvariant => "serialization_invariant",
            Self::SessionAlreadyStarted => "configuration",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleState {
    AwaitInitialize,
    AwaitInitialized,
    Operating,
}

struct InboundMessage {
    envelope: Envelope,
    method: String,
    params: Option<Value>,
}

enum Envelope {
    Request(RequestId),
    Notification,
}

struct DecodeIssue {
    id: Option<RequestId>,
}

enum Dispatch {
    Immediate(ProcessedFrame),
    Start(OperatingRequest),
    Cancel(Option<RequestId>),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult<'a> {
    protocol_version: &'a str,
    capabilities: ServerCapabilities,
    server_info: ServerImplementation<'a>,
}

#[derive(Serialize)]
struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsCapability>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolsCapability {
    list_changed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ServerImplementation<'a> {
    name: &'a str,
    title: &'a str,
    version: &'a str,
    description: &'a str,
}

#[derive(Serialize)]
struct EmptyObject {}

#[derive(Serialize)]
struct ResultResponse<'a, T> {
    jsonrpc: &'static str,
    id: &'a RequestId,
    result: &'a T,
}

#[derive(Serialize)]
struct ErrorResponse<'a> {
    jsonrpc: &'static str,
    id: Option<&'a RequestId>,
    error: ErrorObject,
}

#[derive(Serialize)]
struct ErrorObject {
    code: i32,
    message: &'static str,
}

struct ProcessedFrame {
    response: Option<Vec<u8>>,
    invalid: bool,
}

impl ProcessedFrame {
    const fn valid(response: Option<Vec<u8>>) -> Self {
        Self {
            response,
            invalid: false,
        }
    }

    const fn invalid(response: Option<Vec<u8>>) -> Self {
        Self {
            response,
            invalid: true,
        }
    }
}

enum ReadFrame {
    Eof,
    Complete(Vec<u8>),
    Oversized,
    Unterminated,
}

// Framing state lives outside the future returned by `read_next`. The serve
// loop may drop that future whenever another `select!` branch wins; retaining
// partial bytes here prevents the next poll from losing progress.
struct FrameReader<R> {
    input: R,
    frame: Vec<u8>,
    maximum: usize,
    #[cfg(test)]
    reservation_growths: usize,
}

impl<R> FrameReader<R>
where
    R: AsyncBufRead + Unpin,
{
    const fn new(input: R, maximum: usize) -> Self {
        Self {
            input,
            frame: Vec::new(),
            maximum,
            #[cfg(test)]
            reservation_growths: 0,
        }
    }

    async fn read_next(&mut self) -> Result<ReadFrame, SessionError> {
        loop {
            let buffer = self.input.fill_buf().await.map_err(SessionError::Io)?;
            if buffer.is_empty() {
                return if self.frame.is_empty() {
                    Ok(ReadFrame::Eof)
                } else {
                    self.frame.clear();
                    Ok(ReadFrame::Unterminated)
                };
            }

            let remaining = self
                .maximum
                .checked_sub(self.frame.len())
                .ok_or(SessionError::MemoryUnavailable)?;
            let inspected_len = buffer.len().min(remaining.saturating_add(1));
            let newline = buffer
                .get(..inspected_len)
                .and_then(|bounded| bounded.iter().position(|byte| *byte == b'\n'));
            let payload_len = newline.unwrap_or(inspected_len);
            if newline.is_none() && buffer.len() > remaining {
                self.input.consume(inspected_len);
                self.frame.clear();
                return Ok(ReadFrame::Oversized);
            }

            let _grew = match try_reserve_bounded(&mut self.frame, payload_len, self.maximum) {
                Ok(grew) => grew,
                Err(BoundedReserveError::Limit) => {
                    self.frame.clear();
                    return Ok(ReadFrame::Oversized);
                }
                Err(BoundedReserveError::Memory) => {
                    return Err(SessionError::MemoryUnavailable);
                }
            };
            #[cfg(test)]
            if _grew {
                self.reservation_growths = self.reservation_growths.saturating_add(1);
            }
            self.frame.extend_from_slice(
                buffer
                    .get(..payload_len)
                    .ok_or(SessionError::MemoryUnavailable)?,
            );
            let consumed = newline.map_or(payload_len, |index| index.saturating_add(1));
            self.input.consume(consumed);
            if newline.is_some() {
                return Ok(ReadFrame::Complete(std::mem::take(&mut self.frame)));
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoundedReserveError {
    Limit,
    Memory,
}

pub(crate) fn try_reserve_bounded<T>(
    values: &mut Vec<T>,
    additional: usize,
    maximum: usize,
) -> Result<bool, BoundedReserveError> {
    let required = values
        .len()
        .checked_add(additional)
        .ok_or(BoundedReserveError::Limit)?;
    if required > maximum {
        return Err(BoundedReserveError::Limit);
    }
    if required <= values.capacity() {
        return Ok(false);
    }

    // Geometric growth bounds allocator calls while the explicit maximum
    // prevents spare capacity requests from following untrusted lengths.
    let doubled = values.capacity().max(1).saturating_mul(2);
    let target = required.max(doubled.min(maximum));
    let reserve = target
        .checked_sub(values.len())
        .ok_or(BoundedReserveError::Limit)?;
    values
        .try_reserve_exact(reserve)
        .map_err(|_| BoundedReserveError::Memory)?;
    Ok(true)
}

fn readable_id(value: &Value) -> Option<RequestId> {
    value
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(RequestId::from_value)
}

fn decode_inbound(value: Value) -> Result<InboundMessage, DecodeIssue> {
    let readable = readable_id(&value);
    let Value::Object(mut object) = value else {
        return Err(DecodeIssue { id: None });
    };

    let envelope = match object.remove("id") {
        Some(value) => RequestId::from_value(&value)
            .map(Envelope::Request)
            .ok_or(DecodeIssue { id: None })?,
        None => Envelope::Notification,
    };
    let jsonrpc = object.remove("jsonrpc");
    let method = object.remove("method");
    let params = object.remove("params");
    if !object.is_empty()
        || !matches!(jsonrpc, Some(Value::String(version)) if version == JSON_RPC_VERSION)
    {
        return Err(DecodeIssue { id: readable });
    }
    let Some(Value::String(method)) = method else {
        return Err(DecodeIssue { id: readable });
    };
    if method.is_empty() || method.len() > MAX_METHOD_BYTES {
        return Err(DecodeIssue { id: readable });
    }
    Ok(InboundMessage {
        envelope,
        method,
        params,
    })
}

fn initialize_params_are_valid(params: Option<&Value>) -> bool {
    let Some(Value::Object(params)) = params else {
        return false;
    };
    if params.keys().any(|key| {
        !matches!(
            key.as_str(),
            "_meta" | "protocolVersion" | "capabilities" | "clientInfo"
        )
    }) || params
        .get("_meta")
        .is_some_and(|meta| !request_meta_is_valid(meta))
    {
        return false;
    }
    let Some(Value::String(protocol_version)) = params.get("protocolVersion") else {
        return false;
    };
    if protocol_version.is_empty()
        || protocol_version.len() > MAX_IMPLEMENTATION_VERSION_BYTES
        || !client_capabilities_are_valid(params.get("capabilities"))
        || !client_implementation_is_valid(params.get("clientInfo"))
    {
        return false;
    }
    true
}

fn client_capabilities_are_valid(capabilities: Option<&Value>) -> bool {
    let Some(Value::Object(capabilities)) = capabilities else {
        return false;
    };
    for (name, value) in capabilities {
        let valid = match name.as_str() {
            "experimental" => {
                matches!(value, Value::Object(values) if values.values().all(Value::is_object))
            }
            "roots" => object_has_typed_fields(value, &[("listChanged", JsonKind::Boolean)]),
            "sampling" => object_has_typed_fields(
                value,
                &[("context", JsonKind::Object), ("tools", JsonKind::Object)],
            ),
            "elicitation" => object_has_typed_fields(
                value,
                &[("form", JsonKind::Object), ("url", JsonKind::Object)],
            ),
            "tasks" => tasks_capability_is_valid(value),
            // The official capability set is explicitly open. Unknown
            // capability payloads retain their vendor-defined JSON shape.
            _ => true,
        };
        if !valid {
            return false;
        }
    }
    true
}

fn tasks_capability_is_valid(value: &Value) -> bool {
    let Some(tasks) = value.as_object() else {
        return false;
    };
    if tasks.get("list").is_some_and(|value| !value.is_object())
        || tasks.get("cancel").is_some_and(|value| !value.is_object())
    {
        return false;
    }
    let Some(requests) = tasks.get("requests") else {
        return true;
    };
    let Some(requests) = requests.as_object() else {
        return false;
    };
    requests
        .get("sampling")
        .is_none_or(|value| object_has_typed_fields(value, &[("createMessage", JsonKind::Object)]))
        && requests
            .get("elicitation")
            .is_none_or(|value| object_has_typed_fields(value, &[("create", JsonKind::Object)]))
}

fn client_implementation_is_valid(value: Option<&Value>) -> bool {
    let Some(Value::Object(implementation)) = value else {
        return false;
    };
    if implementation.keys().any(|key| {
        !matches!(
            key.as_str(),
            "name" | "title" | "version" | "description" | "icons" | "websiteUrl"
        )
    }) {
        return false;
    }
    let Some(Value::String(name)) = implementation.get("name") else {
        return false;
    };
    let Some(Value::String(version)) = implementation.get("version") else {
        return false;
    };
    if name.is_empty()
        || name.len() > MAX_IMPLEMENTATION_NAME_BYTES
        || version.is_empty()
        || version.len() > MAX_IMPLEMENTATION_VERSION_BYTES
        || !optional_bounded_string(implementation.get("title"), MAX_IMPLEMENTATION_TITLE_BYTES)
        || !optional_bounded_string(
            implementation.get("description"),
            MAX_IMPLEMENTATION_DESCRIPTION_BYTES,
        )
        || !optional_bounded_string(implementation.get("websiteUrl"), MAX_WEBSITE_BYTES)
    {
        return false;
    }
    let Some(icons) = implementation.get("icons") else {
        return true;
    };
    matches!(icons, Value::Array(icons) if icons.len() <= MAX_IMPLEMENTATION_ICONS && icons.iter().all(client_icon_is_valid))
}

fn client_icon_is_valid(value: &Value) -> bool {
    let Some(icon) = value.as_object() else {
        return false;
    };
    if icon
        .keys()
        .any(|key| !matches!(key.as_str(), "src" | "mimeType" | "sizes" | "theme"))
    {
        return false;
    }
    let Some(Value::String(source)) = icon.get("src") else {
        return false;
    };
    if source.is_empty()
        || source.len() > MAX_ICON_SOURCE_BYTES
        || !optional_bounded_string(icon.get("mimeType"), MAX_ICON_MIME_BYTES)
        || !valid_icon_theme(icon.get("theme"))
    {
        return false;
    }
    let Some(sizes) = icon.get("sizes") else {
        return true;
    };
    matches!(
        sizes,
        Value::Array(sizes)
            if sizes.len() <= MAX_ICON_SIZES
                && sizes.iter().all(|size| {
                    matches!(size, Value::String(size) if !size.is_empty() && size.len() <= MAX_ICON_SIZE_BYTES)
                })
    )
}

fn valid_icon_theme(theme: Option<&Value>) -> bool {
    match theme {
        None => true,
        Some(Value::String(theme)) => matches!(theme.as_str(), "light" | "dark"),
        Some(_) => false,
    }
}

fn optional_bounded_string(value: Option<&Value>, maximum: usize) -> bool {
    match value {
        None => true,
        Some(Value::String(value)) => value.len() <= maximum,
        Some(_) => false,
    }
}

#[derive(Clone, Copy)]
enum JsonKind {
    Boolean,
    Object,
}

fn object_has_typed_fields(value: &Value, fields: &[(&str, JsonKind)]) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    fields.iter().all(|(name, kind)| {
        object.get(*name).is_none_or(|value| match kind {
            JsonKind::Boolean => value.is_boolean(),
            JsonKind::Object => value.is_object(),
        })
    })
}

fn request_params_are_valid(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return true;
    };
    let Some(params) = params.as_object() else {
        return false;
    };
    params.keys().all(|key| key == "_meta") && params.get("_meta").is_none_or(request_meta_is_valid)
}

fn initialized_params_are_valid(params: Option<&Value>) -> bool {
    let Some(params) = params else {
        return true;
    };
    let Some(params) = params.as_object() else {
        return false;
    };
    params.keys().all(|key| key == "_meta")
        && params.get("_meta").is_none_or(notification_meta_is_valid)
}

pub(crate) fn request_meta_is_valid(value: &Value) -> bool {
    let Some(meta) = value.as_object() else {
        return false;
    };
    meta.get("progressToken")
        .is_none_or(|token| token.is_string() || token.is_number())
}

fn notification_meta_is_valid(value: &Value) -> bool {
    value.is_object()
}

fn decode_cancellation(params: Option<&Value>) -> Option<RequestId> {
    let params = params?.as_object()?;
    if params
        .keys()
        .any(|key| !matches!(key.as_str(), "_meta" | "requestId" | "reason"))
        || params
            .get("_meta")
            .is_some_and(|meta| !notification_meta_is_valid(meta))
        || !optional_bounded_string(params.get("reason"), MAX_CANCELLATION_REASON_BYTES)
    {
        return None;
    }
    params.get("requestId").and_then(RequestId::from_value)
}

fn result_response<T: Serialize>(
    id: &RequestId,
    result: &T,
    limits: StdioLimits,
) -> Result<Vec<u8>, SessionError> {
    encode_response(
        &ResultResponse {
            jsonrpc: JSON_RPC_VERSION,
            id,
            result,
        },
        limits,
    )
}

fn static_error(
    id: Option<&RequestId>,
    code: i32,
    message: &'static str,
    limits: StdioLimits,
) -> Result<Option<Vec<u8>>, SessionError> {
    encode_response(
        &ErrorResponse {
            jsonrpc: JSON_RPC_VERSION,
            id,
            error: ErrorObject { code, message },
        },
        limits,
    )
    .map(Some)
}

fn encode_handler_response(
    id: &RequestId,
    response: HandlerResponse,
    limits: StdioLimits,
) -> Result<Option<Vec<u8>>, SessionError> {
    match response {
        HandlerResponse::Success(result) => result_response(id, &result, limits).map(Some),
        HandlerResponse::Error { code, message } => static_error(Some(id), code, message, limits),
        HandlerResponse::Cancelled => Ok(None),
    }
}

fn encode_response(
    response: &impl Serialize,
    limits: StdioLimits,
) -> Result<Vec<u8>, SessionError> {
    let payload_limit = limits
        .max_response_bytes
        .checked_sub(1)
        .ok_or(SessionError::ResponseTooLarge)?;
    let mut writer = BoundedResponseWriter::new(payload_limit);
    if let Err(error) = serde_json::to_writer(&mut writer, response) {
        return match writer.failure {
            Some(ResponseWriteFailure::Limit) => Err(SessionError::ResponseTooLarge),
            Some(ResponseWriteFailure::Memory) => Err(SessionError::MemoryUnavailable),
            None => Err(SessionError::Serialization(error)),
        };
    }
    try_reserve_bounded(&mut writer.bytes, 1, limits.max_response_bytes).map_err(|failure| {
        match failure {
            BoundedReserveError::Limit => SessionError::ResponseTooLarge,
            BoundedReserveError::Memory => SessionError::MemoryUnavailable,
        }
    })?;
    writer.bytes.push(b'\n');
    Ok(writer.bytes)
}

#[derive(Clone, Copy)]
enum ResponseWriteFailure {
    Limit,
    Memory,
}

struct BoundedResponseWriter {
    bytes: Vec<u8>,
    maximum: usize,
    failure: Option<ResponseWriteFailure>,
    #[cfg(test)]
    reservation_growths: usize,
}

impl BoundedResponseWriter {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
            failure: None,
            #[cfg(test)]
            reservation_growths: 0,
        }
    }
}

impl io::Write for BoundedResponseWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let _grew = try_reserve_bounded(&mut self.bytes, buffer.len(), self.maximum).map_err(
            |failure| match failure {
                BoundedReserveError::Limit => {
                    self.failure = Some(ResponseWriteFailure::Limit);
                    io::Error::other("response limit exceeded")
                }
                BoundedReserveError::Memory => {
                    self.failure = Some(ResponseWriteFailure::Memory);
                    io::Error::other("response memory unavailable")
                }
            },
        )?;
        #[cfg(test)]
        if _grew {
            self.reservation_growths = self.reservation_growths.saturating_add(1);
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

async fn write_responses(
    mut output: impl AsyncWrite + Unpin,
    mut responses: mpsc::Receiver<Vec<u8>>,
) -> Result<(), SessionError> {
    while let Some(response) = responses.recv().await {
        output
            .write_all(&response)
            .await
            .map_err(SessionError::Io)?;
        output.flush().await.map_err(SessionError::Io)?;
    }
    output.flush().await.map_err(SessionError::Io)
}

fn enqueue_response(sender: &mpsc::Sender<Vec<u8>>, response: Vec<u8>) -> Result<(), SessionError> {
    sender
        .try_send(response)
        .map_err(|_| SessionError::ResponseBackpressure)
}

fn flatten_writer_result(
    result: Result<Result<(), SessionError>, tokio::task::JoinError>,
) -> Result<(), SessionError> {
    result.map_err(|_| SessionError::WriterTaskFailed)?
}

#[cfg(test)]
mod tests;
