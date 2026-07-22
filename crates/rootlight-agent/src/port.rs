//! Client-free application ports used by agent orchestration.
//!
//! These interfaces carry only stable agent and MCP contract types. Concrete
//! daemon clients, async runtimes, JSON-RPC request state, and transport errors
//! are adapted by the composing application.

use std::{future::Future, pin::Pin, time::Instant};

use rootlight_mcp_contract::{
    PublicError, RepositorySelector,
    context::BatchTool,
    vertical::{
        CoverageSummary, GenerationSelector, GenerationSummary, ReadEnvelope, ResolvedRepository,
        ResponseBudget, ResponseWarning,
    },
};
use serde_json::{Map, Value};

use crate::policy::CancellationSignal;

/// Future returned by one client-free agent port operation.
pub type AgentPortFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// Read-only repository and generation identity requested before orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentityRequest {
    repository: RepositorySelector,
    generation: Option<GenerationSelector>,
}

impl AgentIdentityRequest {
    /// Creates one bounded identity-resolution request.
    #[must_use]
    pub const fn new(
        repository: RepositorySelector,
        generation: Option<GenerationSelector>,
    ) -> Self {
        Self {
            repository,
            generation,
        }
    }

    /// Consumes the request and returns its public selectors.
    #[must_use]
    pub fn into_selectors(self) -> (RepositorySelector, Option<GenerationSelector>) {
        (self.repository, self.generation)
    }
}

/// Immutable context pinned once before any child request is dispatched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResolvedIdentity {
    /// Stable repository identity and display label.
    pub repository: ResolvedRepository,
    /// Exact immutable generation selected for all child work.
    pub generation: GenerationSummary,
    /// Coverage metadata available from the read-only status lookup.
    pub coverage: CoverageSummary,
    /// Source-free status warnings retained for aggregate responses.
    pub warnings: Vec<ResponseWarning>,
}

/// Cancellation and deadline policy for the identity preflight read.
#[derive(Debug, Clone)]
pub struct AgentResolutionContext<C> {
    cancellation: C,
    deadline: Instant,
}

impl<C> AgentResolutionContext<C>
where
    C: CancellationSignal,
{
    /// Creates one bounded identity-resolution context.
    #[must_use]
    pub const fn new(cancellation: C, deadline: Instant) -> Self {
        Self {
            cancellation,
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

    /// Returns the mandatory monotonic preflight deadline.
    #[must_use]
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }
}

/// Dynamic read-tool request admitted by batch orchestration.
///
/// Batch arguments are necessarily represented as a JSON object because the
/// selected tool is dynamic. The MCP adapter validates the materialized object
/// against that tool's typed schema before executing it.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentToolRequest {
    tool: BatchTool,
    arguments: Map<String, Value>,
    materialized_binding_paths: Vec<String>,
}

impl AgentToolRequest {
    /// Creates one admitted dynamic child-tool request.
    #[must_use]
    pub const fn new(tool: BatchTool, arguments: Map<String, Value>) -> Self {
        Self {
            tool,
            arguments,
            materialized_binding_paths: Vec::new(),
        }
    }

    /// Attaches the exact destination paths populated from dependency bindings.
    #[must_use]
    pub fn with_materialized_binding_paths(mut self, paths: Vec<String>) -> Self {
        self.materialized_binding_paths = paths;
        self
    }

    /// Returns the selected child tool.
    #[must_use]
    pub const fn tool(&self) -> BatchTool {
        self.tool
    }

    /// Returns the exact JSON Pointer destinations populated by bindings.
    #[must_use]
    pub fn materialized_binding_paths(&self) -> &[String] {
        &self.materialized_binding_paths
    }

    /// Consumes the request and returns its dynamic arguments.
    #[must_use]
    pub fn into_arguments(self) -> Map<String, Value> {
        self.arguments
    }

    /// Consumes the request into its dynamic tool, arguments, and provenance.
    #[must_use]
    pub fn into_parts(self) -> (BatchTool, Map<String, Value>, Vec<String>) {
        (self.tool, self.arguments, self.materialized_binding_paths)
    }
}

/// Request-scoped policy supplied to one child-tool invocation.
#[derive(Debug, Clone)]
pub struct AgentCallContext<C> {
    cancellation: C,
    budget: ResponseBudget,
    local_budget: Option<ResponseBudget>,
    pinned_identity: Option<AgentResolvedIdentity>,
    deadline: Option<Instant>,
    local_deadline: bool,
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
            local_budget: None,
            pinned_identity: None,
            deadline,
            local_deadline: false,
        }
    }

    /// Attaches the caller-requested local cap that contributed to the
    /// effective budget.
    #[must_use]
    pub fn with_local_budget(mut self, local_budget: Option<ResponseBudget>) -> Self {
        self.local_budget = local_budget;
        self
    }

    /// Attaches the immutable repository and generation selected before the
    /// child was admitted.
    #[must_use]
    pub fn with_pinned_identity(mut self, identity: AgentResolvedIdentity) -> Self {
        self.pinned_identity = Some(identity);
        self
    }

    /// Marks that the effective deadline originated from a child-local timeout.
    #[must_use]
    pub const fn with_local_deadline(mut self, local_deadline: bool) -> Self {
        self.local_deadline = local_deadline;
        self
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

    /// Returns the original local cap, when the request declared one.
    #[must_use]
    pub const fn local_budget(&self) -> Option<&ResponseBudget> {
        self.local_budget.as_ref()
    }

    /// Returns the immutable repository and generation selected for the batch.
    #[must_use]
    pub const fn pinned_identity(&self) -> Option<&AgentResolvedIdentity> {
        self.pinned_identity.as_ref()
    }

    /// Returns the earliest parent or child monotonic deadline.
    #[must_use]
    pub const fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// Reports whether deadline expiry is a per-operation budget outcome.
    #[must_use]
    pub const fn has_local_deadline(&self) -> bool {
        self.local_deadline
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
    /// The request-scoped deadline elapsed.
    DeadlineExceeded,
    /// A child-local timeout elapsed while the parent request remained live.
    LocalDeadlineExceeded,
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
    /// Resolves repository and generation identity exactly once without source
    /// retrieval or mutation.
    ///
    /// Implementations must race this metadata-only read against the supplied
    /// cancellation signal and mandatory deadline.
    fn resolve_identity(
        &self,
        request: AgentIdentityRequest,
        context: AgentResolutionContext<C>,
    ) -> AgentPortFuture<Result<AgentResolvedIdentity, AgentPortError>>;

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
