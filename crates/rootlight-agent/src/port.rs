//! Client-free application ports used by agent orchestration.
//!
//! These interfaces carry only stable agent and MCP contract types. Concrete
//! daemon clients, async runtimes, JSON-RPC request state, and transport errors
//! are adapted by the composing application.

use std::{future::Future, pin::Pin, time::Instant};

use rootlight_mcp_contract::{
    PublicError,
    context::BatchTool,
    vertical::{ReadEnvelope, ResponseBudget},
};
use serde_json::{Map, Value};

use crate::policy::CancellationSignal;

/// Future returned by one client-free agent port operation.
pub type AgentPortFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Dynamic read-tool request admitted by batch orchestration.
///
/// Batch arguments are necessarily represented as a JSON object because the
/// selected tool is dynamic. The MCP adapter validates the materialized object
/// against that tool's typed schema before executing it.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentToolRequest {
    tool: BatchTool,
    arguments: Map<String, Value>,
}

impl AgentToolRequest {
    /// Creates one admitted dynamic child-tool request.
    #[must_use]
    pub const fn new(tool: BatchTool, arguments: Map<String, Value>) -> Self {
        Self { tool, arguments }
    }

    /// Returns the selected child tool.
    #[must_use]
    pub const fn tool(&self) -> BatchTool {
        self.tool
    }

    /// Consumes the request and returns its validated argument object.
    #[must_use]
    pub fn into_arguments(self) -> Map<String, Value> {
        self.arguments
    }
}

/// Request-scoped policy supplied to one child-tool invocation.
#[derive(Debug, Clone)]
pub struct AgentCallContext<C> {
    cancellation: C,
    budget: ResponseBudget,
    deadline: Option<Instant>,
}

impl<C> AgentCallContext<C>
where
    C: CancellationSignal,
{
    /// Creates one child invocation context.
    #[must_use]
    pub const fn new(cancellation: C, budget: ResponseBudget, deadline: Option<Instant>) -> Self {
        Self {
            cancellation,
            budget,
            deadline,
        }
    }

    /// Returns the cooperative cancellation signal.
    #[must_use]
    pub const fn cancellation(&self) -> &C {
        &self.cancellation
    }

    /// Consumes the context and returns its cancellation signal.
    #[must_use]
    pub fn into_cancellation(self) -> C {
        self.cancellation
    }

    /// Returns the effective parent-and-child budget.
    #[must_use]
    pub const fn budget(&self) -> &ResponseBudget {
        &self.budget
    }

    /// Returns the earliest parent or child monotonic deadline.
    #[must_use]
    pub const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
}

/// Source-free failure returned by a concrete agent tool port.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentPortError {
    /// The child tool returned an expected checked domain failure.
    Public(Box<PublicError>),
    /// Cooperative cancellation won while the child was pending.
    Cancelled,
    /// The effective parent or child deadline elapsed.
    DeadlineExceeded,
    /// The adapter response violated the typed agent-port contract.
    InvalidResponse,
    /// The underlying client or transport failed.
    Unavailable,
}

/// Client-free async boundary through which agent orchestration invokes tools.
pub trait AgentToolPort<C>: Send + Sync + 'static
where
    C: CancellationSignal + Clone + Send + Sync + 'static,
{
    /// Executes one already admitted child-tool request.
    ///
    /// Implementations must race the operation against the supplied
    /// cancellation signal and monotonic deadline. The returned envelope must
    /// retain its immutable repository and generation identity.
    fn execute(
        &self,
        request: AgentToolRequest,
        context: AgentCallContext<C>,
    ) -> AgentPortFuture<Result<ReadEnvelope<Value>, AgentPortError>>;
}
