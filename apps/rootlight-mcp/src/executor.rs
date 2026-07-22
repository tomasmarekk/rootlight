//! Typed production mapping between the MCP tool catalog and a daemon client port.
//!
//! The port supplies facts absent from the current client DTOs so this layer
//! never fabricates index-plan, freshness, coverage, cache, or trace metadata.

use std::{
    collections::BTreeSet,
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_agent::{
    batch::{
        BatchExecutionError, BatchOrchestrationError, BatchPlan, BatchPublicErrors, BatchService,
        BatchValidationError, mcp_tool_for_batch,
        resolve_dependencies as resolve_batch_dependencies, terminal_result,
    },
    change::{
        PlanChangeError, PlanChangePort, PlanChangePortOutput, PlanChangeRequest, PlanChangeResult,
        PlanChangeService, PlanChangeServiceError, PlanImpactResult,
    },
    context_pack::{ContextPackService, ContextPackServiceError},
    policy::is_compact_profile,
    port::{
        AgentCallContext, AgentIdentityRequest, AgentPortError, AgentPortFuture,
        AgentResolutionContext, AgentResolvedIdentity, AgentToolPort, AgentToolRequest,
    },
};
use rootlight_client::{
    self as client, CodeLocate, LocateMode, RepositoryCatalogPage, RepositoryIndex,
    RepositoryOperationAction, RepositoryOperationStatus, RepositoryStatus, SourceRead,
    SymbolExplain,
};
use rootlight_ids::{GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{EntityKind as IrEntityKind, LineRange, SourceRef, SourceSpan};
use rootlight_mcp_contract::change::{
    ArchitectureDelta, BreakingCandidate, ChangeClassification, ChangeImpactData,
    ChangeImpactInput, ChangePlanStep, CompareChangeKind, ContextPackRequest, HistoryCompareData,
    HistoryCompareInput, ImpactEntry, ImpactGroup, ImpactRiskSummary, LineageMatch, MatchedStates,
    PlanChangeInput, PlanDecision, RankedTest, RelationPolicy, ResolvedChange, RevisionSelector,
    RiskLevel, SemanticChange, SemanticChangeKind, TestCandidate, TestCoverageStrategy, TestGap,
    TestKind, TestsSelectData, TestsSelectInput,
};
use rootlight_mcp_contract::intent::{
    ArchitectureComponent, ArchitectureConnection, ArchitectureCyclesData, ArchitectureCyclesInput,
    ArchitectureOverviewData, ArchitectureOverviewInput, ArchitectureView, BlindSpot, CodeDeadData,
    CodeDeadInput, CycleBreakCandidate, DeadCandidate, DeadClassification, DerivedViewInfo,
    Direction, EntryPointPolicy, EntryPointSummary, FlowTraceData, FlowTraceInput, FrontierSummary,
    Hotspot, MinimalCycle, RelationKind, RelationProjection, RelationshipGroup, RelationshipTarget,
    RelationshipTotals, RuleSummary, StronglyConnectedComponent, SymbolRelationshipsData,
    SymbolRelationshipsInput, TraceEdge, TracePath,
};
use rootlight_mcp_contract::{
    DetailKey, ErrorCode, ExposureProfile, GenerationSelector, McpTool, NextAction, PublicError,
    PublicErrorBuildError, PublicValue, RepoIndexInput, RepositorySelector, SafeLabel,
    SchemaVersion, SourceFreeMessage, SourceReadInput, SymbolExplainInput, ToolResponse,
    TrustClassification, VerticalTool,
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance,
        LimitingResource as ContractLimitingResource,
        LimitingResourceKind as ContractLimitingResourceKind, ResultCompleteness,
    },
    context::{
        BatchOperationStatus, BatchStatus, BatchTool, ColumnSchema, ColumnType, ContextPackInput,
        PlanExplanation, QueryAdvancedData, QueryAdvancedInput, QueryBatchData, QueryBatchInput,
        QueryCompleteness,
    },
    error_definition,
    pagination::{AuthenticatedCursor, CursorContext},
    repository::{
        CatalogEnvelope, CatalogSnapshotId, CoverageDetail, CoverageReport, FreshnessRequirement,
        GenerationPublicationState, LanguageCoverageReport, OperationSummary, RepoListData,
        RepoListInput, RepoListSchemaVersion, RepoStatusData, RepoStatusInput, RepositoryEntry,
        RepositoryState,
    },
    vertical::{
        ActiveGeneration, AnalysisTier, CacheStatus, CodeLocateData, CodeLocateInput,
        ContinuationCursor, CoverageSummary, DetailHandle, Diagnostic, EntityKind, Freshness,
        GenerationSummary, IndexMode, IndexPlanScope, IndexPlanSummary, LanguageCoverage,
        LocateReason, LocatedItem, OperationAction, OperationDetail, OperationProgress,
        OperationResources, OperationState, OperationStatusData, OperationStatusInput,
        OperationStatusSuccess, ProvenanceLevel, ProvenanceSummary, QueryInterpretation,
        ReadEnvelope, RepoIndexData, RepoIndexSuccess, RequiredNullable, ResolvedRepository,
        ResponseBudget, ResponseProfile, ResponseWarning, SearchMode, SourceChunk, SourceElision,
        SourceEncoding, SourceEncodingRequest, SourceReadData, SourceReadSelector,
        StaleSourceReference, SymbolExplainData, SymbolExplanation, UsageSummary,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::advanced::{AdvancedQueryError, AdvancedQueryPlan, MAX_ADVANCED_TRAVERSAL};
use crate::{
    RequestCancellation, ToolExecutionError, ToolExecutionFailure, ToolExecutionFuture,
    ToolExecutor,
    error_mapping::{
        MappedDomainFailure, public_error as mapped_public_error, public_error_with_details,
    },
    tools::{
        CapabilityBindingPolicy, MaterializedInputError, MaterializedToolValidator,
        validate_capability_input,
    },
};

const DEFAULT_LOCATE_RESULTS: u16 = 20;
const DEFAULT_ADVANCED_RESULTS: u16 = 100;
#[cfg(test)]
const INVALID_ARGUMENT_MESSAGE: &str = error_definition(ErrorCode::InvalidArgument).message;
#[cfg(test)]
const UNSUPPORTED_MESSAGE: &str = error_definition(ErrorCode::UnsupportedCapability).message;
const BATCH_OPERATION_FAILED_MESSAGE: &str = "batch operation failed";

/// Future returned by one injected first-slice client-port operation.
pub type ClientPortFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, ClientPortError>> + Send + 'static>>;

/// Client-free `plan.change` request admitted by the agent boundary.
pub type PlanChangePortRequest = PlanChangeRequest;

/// Narrow asynchronous daemon-client boundary used by the production MCP executor.
///
/// Implementations own transport and may use the supplied cancellation signal
/// to stop deeper work. The executor also races and drops every pending port
/// future when that signal fires. The response wrappers carry mandatory MCP
/// facts that the current `rootlight-client` DTOs do not yet expose.
pub trait FirstSliceClientPort: Send + Sync + 'static {
    /// Starts one whole-repository first-slice index operation.
    ///
    /// The MCP input has no request-scoped idempotency key. Implementations
    /// must preserve update semantics and may assign a fresh operation ID;
    /// repeated unchanged snapshots converge through content-derived generation
    /// identity. Do not memoize solely by root and options because source may
    /// change between otherwise identical calls.
    fn repository_index(
        &self,
        request: RepositoryIndexPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryIndexPortResponse>;

    /// Reads or cooperatively cancels one repository-index operation.
    fn operation_status(
        &self,
        request: OperationStatusPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryOperationStatus>;

    /// Executes one bounded exact or lexical locate request.
    fn code_locate(
        &self,
        request: CodeLocatePortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse>;

    /// Explains one bounded set of stable symbols.
    fn symbol_explain(
        &self,
        request: SymbolExplainPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse>;

    /// Reads one bounded set of exact generation-pinned source references.
    fn source_read(
        &self,
        request: SourceReadPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse>;

    /// Lists the repositories known to the daemon process.
    fn repository_catalog_page(
        &self,
        request: RepositoryCatalogPagePortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryCatalogPage>;

    /// Reads one repository's active generation status.
    fn repository_status(
        &self,
        request: RepositoryStatusPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryStatus>;

    /// Expands bounded typed relation neighborhoods for stable symbols.
    fn symbol_relationships(
        &self,
        request: SymbolRelationshipsPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolRelationshipsPortResponse>;

    /// Traces bounded directed paths between stable symbols.
    fn flow_trace(
        &self,
        request: FlowTracePortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<FlowTracePortResponse>;

    /// Detects bounded architecture cycles over a relation projection.
    fn architecture_cycles(
        &self,
        request: ArchitectureCyclesPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureCyclesPortResponse>;

    /// Detects bounded dead-code candidates over one generation.
    fn code_dead(
        &self,
        request: CodeDeadPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeDeadPortResponse>;

    /// Aggregates a bounded file-granularity architecture overview over one
    /// generation.
    fn architecture_overview(
        &self,
        request: ArchitectureOverviewPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureOverviewPortResponse>;

    /// Selects bounded relevant tests for a seed set over one generation.
    fn tests_select(
        &self,
        request: TestsSelectPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<TestsSelectPortResponse>;

    /// Maps bounded change impact for an explicit change set over one
    /// generation.
    fn change_impact(
        &self,
        request: ChangeImpactPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<ChangeImpactPortResponse>;

    /// Builds a bounded ordered change plan for an explicit target set over one
    /// generation.
    fn plan_change(
        &self,
        request: PlanChangePortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse>;

    /// Compares two explicit generations for bounded semantic changes.
    fn history_compare(
        &self,
        request: HistoryComparePortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<HistoryComparePortResponse>;

    /// Executes one bounded advanced query over a safe typed AST.
    fn query_advanced(
        &self,
        request: QueryAdvancedPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<QueryAdvancedPortResponse>;
}

/// Source-free failure emitted by an injected daemon client port.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClientPortError {
    /// The daemon returned an expected checked domain failure.
    Public(Box<PublicError>),
    /// The local daemon transport failed.
    Transport,
    /// The daemon response violated the typed client-port contract.
    InvalidResponse,
    /// The port failed before a valid request or response existed.
    Executor,
}

/// Checked daemon catalog-page request used by the MCP boundary.
pub type RepositoryCatalogPagePortRequest = client::RepositoryCatalogPageRequest;

/// Normalized `repo.status` request accepted by the current first-slice daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryStatusPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    coverage_detail: client::RepositoryStatusCoverageDetail,
    include_operations: bool,
    require_freshness: client::RepositoryStatusFreshnessRequirement,
}

impl RepositoryStatusPortRequest {
    /// Creates an aggregate status request without operation details or a freshness gate.
    #[must_use]
    pub const fn new(repository: RepositoryId, generation: client::GenerationSelector) -> Self {
        Self {
            repository,
            generation,
            coverage_detail: client::RepositoryStatusCoverageDetail::Summary,
            include_operations: false,
            require_freshness: client::RepositoryStatusFreshnessRequirement::None,
        }
    }

    /// Applies the supported detail controls after MCP capability preflight.
    #[must_use]
    pub const fn with_controls(
        mut self,
        coverage_detail: client::RepositoryStatusCoverageDetail,
        include_operations: bool,
        require_freshness: client::RepositoryStatusFreshnessRequirement,
    ) -> Self {
        self.coverage_detail = coverage_detail;
        self.include_operations = include_operations;
        self.require_freshness = require_freshness;
        self
    }

    /// Returns the resolved repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the requested supported coverage projection.
    #[must_use]
    pub const fn coverage_detail(&self) -> client::RepositoryStatusCoverageDetail {
        self.coverage_detail
    }

    /// Reports whether bounded operation summaries were requested.
    #[must_use]
    pub const fn include_operations(&self) -> bool {
        self.include_operations
    }

    /// Returns the minimum acceptable freshness.
    #[must_use]
    pub const fn freshness_requirement(&self) -> client::RepositoryStatusFreshnessRequirement {
        self.require_freshness
    }
}

/// Normalized `repo.index` request accepted by the current first-slice daemon.
#[derive(Clone, PartialEq, Eq)]
pub struct RepositoryIndexPortRequest {
    root: String,
    mode: IndexMode,
    detached: bool,
}

impl RepositoryIndexPortRequest {
    /// Returns the local repository root supplied by the MCP caller.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Returns the admitted structural indexing mode.
    #[must_use]
    pub const fn mode(&self) -> IndexMode {
        self.mode
    }

    /// Reports whether work may continue after transport disconnect.
    #[must_use]
    pub const fn detached(&self) -> bool {
        self.detached
    }
}

impl fmt::Debug for RepositoryIndexPortRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryIndexPortRequest")
            .field("root_bytes", &self.root.len())
            .field("mode", &self.mode)
            .field("detached", &self.detached)
            .finish()
    }
}

/// Daemon result plus mandatory admitted-plan facts for `repo.index`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryIndexPortResponse {
    result: RepositoryIndex,
    accepted_plan: IndexPlanSummary,
    diagnostics: Vec<Diagnostic>,
}

impl RepositoryIndexPortResponse {
    /// Creates a complete repository-index response for MCP mapping.
    #[must_use]
    pub const fn new(
        result: RepositoryIndex,
        accepted_plan: IndexPlanSummary,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        Self {
            result,
            accepted_plan,
            diagnostics,
        }
    }
}

/// Normalized `operation.status` request accepted by the daemon client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationStatusPortRequest {
    operation: OperationId,
    action: RepositoryOperationAction,
    wait_ms: Option<u32>,
    after_revision: Option<u64>,
}

impl OperationStatusPortRequest {
    /// Returns the stable operation identity.
    #[must_use]
    pub const fn operation(&self) -> OperationId {
        self.operation
    }

    /// Returns the requested read or cancellation action.
    #[must_use]
    pub const fn action(&self) -> RepositoryOperationAction {
        self.action
    }

    /// Returns the bounded long-poll duration.
    #[must_use]
    pub const fn wait_ms(&self) -> Option<u32> {
        self.wait_ms
    }

    /// Returns the optional journal revision gate.
    #[must_use]
    pub const fn after_revision(&self) -> Option<u64> {
        self.after_revision
    }
}

/// Normalized `code.locate` request supported by the current daemon protocol.
#[derive(Clone, PartialEq, Eq)]
pub struct CodeLocatePortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    query: String,
    mode: LocateMode,
    maximum_results: u32,
    page_offset: u64,
}

impl CodeLocatePortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the user-supplied locate query.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Returns the admitted exact or lexical search mode.
    #[must_use]
    pub const fn mode(&self) -> LocateMode {
        self.mode
    }

    /// Returns the effective result ceiling.
    #[must_use]
    pub const fn maximum_results(&self) -> u32 {
        self.maximum_results
    }

    /// Returns the deterministic page offset.
    #[must_use]
    pub const fn page_offset(&self) -> u64 {
        self.page_offset
    }
}

impl fmt::Debug for CodeLocatePortRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeLocatePortRequest")
            .field("repository", &self.repository)
            .field("generation", &self.generation)
            .field("query_bytes", &self.query.len())
            .field("mode", &self.mode)
            .field("maximum_results", &self.maximum_results)
            .field("page_offset", &self.page_offset)
            .finish()
    }
}

/// Located daemon data plus mandatory MCP read metadata and query tokens.
#[derive(Clone, PartialEq, Eq)]
pub struct CodeLocatePortResponse {
    result: CodeLocate,
    metadata: ReadResponseMetadata,
    query_tokens: Vec<String>,
}

impl CodeLocatePortResponse {
    /// Creates a complete `code.locate` response for MCP mapping.
    #[must_use]
    pub const fn new(
        result: CodeLocate,
        metadata: ReadResponseMetadata,
        query_tokens: Vec<String>,
    ) -> Self {
        Self {
            result,
            metadata,
            query_tokens,
        }
    }
}

impl fmt::Debug for CodeLocatePortResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CodeLocatePortResponse")
            .field("result", &self.result)
            .field("metadata", &self.metadata)
            .field("query_token_count", &self.query_tokens.len())
            .finish()
    }
}

/// Normalized `symbol.explain` request supported by the current daemon protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolExplainPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    symbols: Vec<SymbolId>,
    include_provenance: bool,
}

impl SymbolExplainPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns stable symbols in deterministic request order.
    #[must_use]
    pub fn symbols(&self) -> &[SymbolId] {
        &self.symbols
    }

    /// Reports whether compact provenance was requested.
    #[must_use]
    pub const fn include_provenance(&self) -> bool {
        self.include_provenance
    }
}

/// Explained daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolExplainPortResponse {
    result: SymbolExplain,
    metadata: ReadResponseMetadata,
}

impl SymbolExplainPortResponse {
    /// Creates a complete `symbol.explain` response for MCP mapping.
    #[must_use]
    pub const fn new(result: SymbolExplain, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `symbol.relationships` request supported by the current daemon protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRelationshipsPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    seeds: Vec<SymbolId>,
    relations: Vec<String>,
    direction: Option<String>,
    min_confidence: Option<u16>,
    max_results: Option<u16>,
    page_offset: u64,
}

impl SymbolRelationshipsPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns seed symbols in deterministic request order.
    #[must_use]
    pub fn seeds(&self) -> &[SymbolId] {
        &self.seeds
    }

    /// Returns requested relation family labels.
    #[must_use]
    pub fn relations(&self) -> &[String] {
        &self.relations
    }

    /// Returns the optional direction label.
    #[must_use]
    pub fn direction(&self) -> Option<&str> {
        self.direction.as_deref()
    }

    /// Returns the optional confidence floor.
    #[must_use]
    pub const fn min_confidence(&self) -> Option<u16> {
        self.min_confidence
    }

    /// Returns the optional result bound.
    #[must_use]
    pub const fn max_results(&self) -> Option<u16> {
        self.max_results
    }

    /// Returns the deterministic page offset.
    #[must_use]
    pub const fn page_offset(&self) -> u64 {
        self.page_offset
    }
}

/// Expanded daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRelationshipsPortResponse {
    result: client::SymbolRelationships,
    metadata: ReadResponseMetadata,
}

impl SymbolRelationshipsPortResponse {
    /// Creates a complete `symbol.relationships` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::SymbolRelationships, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `flow.trace` request supported by the current daemon protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowTracePortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    from: SymbolId,
    to: Option<SymbolId>,
    relations: Vec<String>,
    direction: Option<String>,
    max_depth: Option<u8>,
    max_paths: Option<u16>,
    min_confidence: Option<u16>,
}

impl FlowTracePortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the trace source symbol.
    #[must_use]
    pub const fn from(&self) -> SymbolId {
        self.from
    }

    /// Returns the optional trace target symbol.
    #[must_use]
    pub const fn to(&self) -> Option<SymbolId> {
        self.to
    }

    /// Returns requested relation family labels.
    #[must_use]
    pub fn relations(&self) -> &[String] {
        &self.relations
    }

    /// Returns the optional direction label.
    #[must_use]
    pub fn direction(&self) -> Option<&str> {
        self.direction.as_deref()
    }

    /// Returns the optional depth bound.
    #[must_use]
    pub const fn max_depth(&self) -> Option<u8> {
        self.max_depth
    }

    /// Returns the optional path bound.
    #[must_use]
    pub const fn max_paths(&self) -> Option<u16> {
        self.max_paths
    }

    /// Returns the optional confidence floor.
    #[must_use]
    pub const fn min_confidence(&self) -> Option<u16> {
        self.min_confidence
    }
}

/// Traced daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowTracePortResponse {
    result: client::FlowTrace,
    metadata: ReadResponseMetadata,
}

impl FlowTracePortResponse {
    /// Creates a complete `flow.trace` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::FlowTrace, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `architecture.cycles` request supported by the current daemon protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectureCyclesPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    relations: Vec<String>,
    min_size: Option<u8>,
    max_cycles: Option<u16>,
    include_self_cycles: Option<bool>,
}

impl ArchitectureCyclesPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns requested relation family labels.
    #[must_use]
    pub fn relations(&self) -> &[String] {
        &self.relations
    }

    /// Returns the optional minimum component size.
    #[must_use]
    pub const fn min_size(&self) -> Option<u8> {
        self.min_size
    }

    /// Returns the optional cycle bound.
    #[must_use]
    pub const fn max_cycles(&self) -> Option<u16> {
        self.max_cycles
    }

    /// Returns the optional self-cycle opt-in.
    #[must_use]
    pub const fn include_self_cycles(&self) -> Option<bool> {
        self.include_self_cycles
    }
}

/// Detected daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectureCyclesPortResponse {
    result: client::ArchitectureCycles,
    metadata: ReadResponseMetadata,
}

impl ArchitectureCyclesPortResponse {
    /// Creates a complete `architecture.cycles` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::ArchitectureCycles, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `code.dead` request supported by the current daemon protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeDeadPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    entry_point_policy: Option<String>,
    include_exported: Option<bool>,
    include_tests: Option<bool>,
    min_confidence: Option<u16>,
    max_candidates: Option<u16>,
}

impl CodeDeadPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the optional entry-point policy label.
    #[must_use]
    pub fn entry_point_policy(&self) -> Option<&str> {
        self.entry_point_policy.as_deref()
    }

    /// Returns the optional exported-inclusion flag.
    #[must_use]
    pub const fn include_exported(&self) -> Option<bool> {
        self.include_exported
    }

    /// Returns the optional test-inclusion flag.
    #[must_use]
    pub const fn include_tests(&self) -> Option<bool> {
        self.include_tests
    }

    /// Returns the optional confidence floor.
    #[must_use]
    pub const fn min_confidence(&self) -> Option<u16> {
        self.min_confidence
    }

    /// Returns the optional candidate cap.
    #[must_use]
    pub const fn max_candidates(&self) -> Option<u16> {
        self.max_candidates
    }
}

/// Detected daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeDeadPortResponse {
    result: client::CodeDead,
    metadata: ReadResponseMetadata,
}

impl CodeDeadPortResponse {
    /// Creates a complete `code.dead` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::CodeDead, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `architecture.overview` request supported by the current daemon
/// protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectureOverviewPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    views: Vec<String>,
    max_components: Option<u16>,
    include_edges: Option<bool>,
    min_confidence: Option<u16>,
}

impl ArchitectureOverviewPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the accepted derived-view labels.
    #[must_use]
    pub fn views(&self) -> &[String] {
        &self.views
    }

    /// Returns the optional component cap.
    #[must_use]
    pub const fn max_components(&self) -> Option<u16> {
        self.max_components
    }

    /// Returns the optional edge-inclusion flag.
    #[must_use]
    pub const fn include_edges(&self) -> Option<bool> {
        self.include_edges
    }

    /// Returns the optional confidence floor.
    #[must_use]
    pub const fn min_confidence(&self) -> Option<u16> {
        self.min_confidence
    }
}

/// Detected daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchitectureOverviewPortResponse {
    result: client::ArchitectureOverview,
    metadata: ReadResponseMetadata,
}

impl ArchitectureOverviewPortResponse {
    /// Creates a complete `architecture.overview` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::ArchitectureOverview, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `tests.select` request supported by the current daemon protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestsSelectPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    seeds: Vec<SymbolId>,
    test_kinds: Vec<String>,
    max_tests: Option<u16>,
    include_commands: Option<bool>,
}

impl TestsSelectPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the seed symbol identifiers.
    #[must_use]
    pub fn seeds(&self) -> &[SymbolId] {
        &self.seeds
    }

    /// Returns the requested test-kind labels.
    #[must_use]
    pub fn test_kinds(&self) -> &[String] {
        &self.test_kinds
    }

    /// Returns the optional test cap.
    #[must_use]
    pub const fn max_tests(&self) -> Option<u16> {
        self.max_tests
    }

    /// Returns the optional command-inclusion flag.
    #[must_use]
    pub const fn include_commands(&self) -> Option<bool> {
        self.include_commands
    }
}

/// Detected daemon data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestsSelectPortResponse {
    result: client::TestsSelect,
    metadata: ReadResponseMetadata,
}

impl TestsSelectPortResponse {
    /// Creates a complete `tests.select` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::TestsSelect, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `change.impact` request ready for the daemon client port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeImpactPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    changed_symbols: Vec<SymbolId>,
    changed_paths: Vec<String>,
    max_depth: Option<u8>,
    min_confidence: Option<u16>,
    include_tests: Option<bool>,
    max_dependents: Option<u16>,
}

impl ChangeImpactPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the explicit changed symbol identifiers.
    #[must_use]
    pub fn changed_symbols(&self) -> &[SymbolId] {
        &self.changed_symbols
    }

    /// Returns the explicit changed repository-relative paths.
    #[must_use]
    pub fn changed_paths(&self) -> &[String] {
        &self.changed_paths
    }

    /// Returns the optional transitive depth bound.
    #[must_use]
    pub const fn max_depth(&self) -> Option<u8> {
        self.max_depth
    }

    /// Returns the optional minimum propagation confidence.
    #[must_use]
    pub const fn min_confidence(&self) -> Option<u16> {
        self.min_confidence
    }

    /// Returns the optional test-inclusion flag.
    #[must_use]
    pub const fn include_tests(&self) -> Option<bool> {
        self.include_tests
    }

    /// Returns the optional dependent cap.
    #[must_use]
    pub const fn max_dependents(&self) -> Option<u16> {
        self.max_dependents
    }
}

/// Detected daemon change-impact data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeImpactPortResponse {
    result: client::ChangeImpact,
    metadata: ReadResponseMetadata,
}

impl ChangeImpactPortResponse {
    /// Creates a complete `change.impact` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::ChangeImpact, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Detected daemon change-plan data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangePortResponse {
    result: client::PlanChange,
    metadata: ReadResponseMetadata,
}

impl PlanChangePortResponse {
    /// Creates a complete `plan.change` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::PlanChange, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `history.compare` request ready for the daemon client port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryComparePortRequest {
    repository: RepositoryId,
    base: GenerationId,
    head: GenerationId,
    change_kinds: Vec<String>,
    max_results: Option<u16>,
}

impl HistoryComparePortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the resolved base generation.
    #[must_use]
    pub const fn base(&self) -> GenerationId {
        self.base
    }

    /// Returns the resolved head generation.
    #[must_use]
    pub const fn head(&self) -> GenerationId {
        self.head
    }

    /// Returns the change-kind filter labels.
    #[must_use]
    pub fn change_kinds(&self) -> &[String] {
        &self.change_kinds
    }

    /// Returns the optional result cap.
    #[must_use]
    pub const fn max_results(&self) -> Option<u16> {
        self.max_results
    }
}

/// Detected daemon history-compare data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryComparePortResponse {
    result: client::HistoryCompare,
    metadata: ReadResponseMetadata,
}

impl HistoryComparePortResponse {
    /// Creates a complete `history.compare` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::HistoryCompare, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized `query.advanced` request ready for the daemon client port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryAdvancedPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    query_ast: String,
    explain: Option<bool>,
    max_results: Option<u16>,
    max_depth: Option<u8>,
    cost_limit: Option<u64>,
    page_offset: u64,
}

impl QueryAdvancedPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the pinned generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns the JSON-encoded safe typed AST.
    #[must_use]
    pub fn query_ast(&self) -> &str {
        &self.query_ast
    }

    /// Returns whether a plan explanation was requested without execution.
    #[must_use]
    pub const fn explain(&self) -> Option<bool> {
        self.explain
    }

    /// Returns the optional result cap.
    #[must_use]
    pub const fn max_results(&self) -> Option<u16> {
        self.max_results
    }

    /// Returns the optional maximum plan or traversal depth.
    #[must_use]
    pub const fn max_depth(&self) -> Option<u8> {
        self.max_depth
    }

    /// Returns the optional client cost ceiling.
    #[must_use]
    pub const fn cost_limit(&self) -> Option<u64> {
        self.cost_limit
    }

    /// Returns the deterministic page offset.
    #[must_use]
    pub const fn page_offset(&self) -> u64 {
        self.page_offset
    }
}

/// Detected daemon advanced-query data plus mandatory MCP read metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct QueryAdvancedPortResponse {
    result: client::AdvancedQuery,
    metadata: ReadResponseMetadata,
}

impl QueryAdvancedPortResponse {
    /// Creates a complete `query.advanced` response for MCP mapping.
    #[must_use]
    pub const fn new(result: client::AdvancedQuery, metadata: ReadResponseMetadata) -> Self {
        Self { result, metadata }
    }
}

/// Normalized exact-reference `source.read` request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReadPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    references: Vec<client::SourceReference>,
}

impl SourceReadPortRequest {
    /// Returns the selected repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the active or explicit immutable-generation selector.
    #[must_use]
    pub const fn generation(&self) -> client::GenerationSelector {
        self.generation
    }

    /// Returns exact generation-bound source references in request order.
    #[must_use]
    pub fn references(&self) -> &[client::SourceReference] {
        &self.references
    }
}

/// Source daemon data plus mandatory MCP metadata and truncation dispositions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReadPortResponse {
    result: SourceRead,
    metadata: ReadResponseMetadata,
    stale_references: Vec<StaleSourceReference>,
    elisions: Vec<SourceElision>,
}

impl SourceReadPortResponse {
    /// Creates a complete `source.read` response for MCP mapping.
    #[must_use]
    pub const fn new(
        result: SourceRead,
        metadata: ReadResponseMetadata,
        stale_references: Vec<StaleSourceReference>,
        elisions: Vec<SourceElision>,
    ) -> Self {
        Self {
            result,
            metadata,
            stale_references,
            elisions,
        }
    }
}

/// Mandatory read facts not yet represented by `rootlight-client` DTOs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadResponseMetadata {
    display_name: String,
    structural_freshness: Freshness,
    semantic_freshness: Freshness,
    languages: Vec<LanguageCoverage>,
    cache_status: CacheStatus,
    trace_id: String,
    warnings: Vec<ResponseWarning>,
}

impl ReadResponseMetadata {
    /// Creates complete server-owned metadata for one MCP read response.
    #[must_use]
    pub const fn new(
        display_name: String,
        structural_freshness: Freshness,
        semantic_freshness: Freshness,
        languages: Vec<LanguageCoverage>,
        cache_status: CacheStatus,
        trace_id: String,
        warnings: Vec<ResponseWarning>,
    ) -> Self {
        Self {
            display_name,
            structural_freshness,
            semantic_freshness,
            languages,
            cache_status,
            trace_id,
            warnings,
        }
    }
}

/// Construction failure for the production first-slice executor.
#[derive(Debug, Error)]
pub enum ToolExecutorBuildError {
    /// The built-in unsupported-capability error violated the public contract.
    #[error("built-in unsupported capability error is invalid")]
    UnsupportedError(#[source] PublicErrorBuildError),
    /// The built-in invalid-argument error violated the public contract.
    #[error("built-in invalid argument error is invalid")]
    InvalidArgumentError(#[source] PublicErrorBuildError),
    /// Secure entropy for the cursor signing key was unavailable.
    #[error("secure cursor signing key initialization failed")]
    CursorKeyInitialization,
    /// Checked MCP contracts could not be compiled for batch child validation.
    #[error("batch child validator initialization failed")]
    BatchValidator(#[source] crate::ToolRegistryError),
}

/// Process-local material used to authenticate pagination cursors.
///
/// The secret never leaves the executor. The public identifier allows a
/// restarted process to reject cursors from a retired key before attempting
/// authentication.
#[derive(Clone, Copy)]
struct CursorSigningKey {
    secret: [u8; 32],
    key_id: u64,
}

impl CursorSigningKey {
    fn new(secret: [u8; 32], key_id: u64) -> Result<Self, ToolExecutorBuildError> {
        if secret.iter().all(|byte| *byte == 0) || key_id == 0 {
            return Err(ToolExecutorBuildError::CursorKeyInitialization);
        }
        Ok(Self { secret, key_id })
    }

    #[cfg(test)]
    fn deterministic(secret: [u8; 32]) -> Result<Self, ToolExecutorBuildError> {
        let digest = blake3::hash(&secret);
        let key_id = u64::from_le_bytes(
            digest.as_bytes()[..8]
                .try_into()
                .expect("BLAKE3 digest contains eight key-id bytes"),
        );
        Self::new(secret, key_id)
    }
}

impl fmt::Debug for CursorSigningKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CursorSigningKey")
            .field("key_id", &self.key_id)
            .field("secret", &"<redacted>")
            .finish()
    }
}

trait CursorKeyProvider {
    fn load(&self) -> Result<CursorSigningKey, ToolExecutorBuildError>;
}

struct SystemCursorKeyProvider;

impl CursorKeyProvider for SystemCursorKeyProvider {
    fn load(&self) -> Result<CursorSigningKey, ToolExecutorBuildError> {
        let mut material = [0_u8; 40];
        getrandom::fill(&mut material)
            .map_err(|_| ToolExecutorBuildError::CursorKeyInitialization)?;
        let secret = material[..32]
            .try_into()
            .expect("cursor key material contains a 32-byte secret");
        let key_id = u64::from_le_bytes(
            material[32..]
                .try_into()
                .expect("cursor key material contains an eight-byte identifier"),
        );
        CursorSigningKey::new(secret, key_id)
    }
}

/// Production MCP executor over an injected asynchronous daemon-client port.
pub struct FirstSliceToolExecutor<P> {
    port: Arc<P>,
    invalid_arguments: PublicError,
    unsupported: PublicError,
    invalid_cursor: PublicError,
    batch_validator: Arc<MaterializedToolValidator>,
    /// Process-local secret used to authenticate pagination cursors.
    ///
    /// It rotates on process restart, gracefully invalidating outstanding
    /// cursors (they fail validation and clients restart the listing).
    cursor_key: CursorSigningKey,
}

impl<P> FirstSliceToolExecutor<P>
where
    P: FirstSliceClientPort,
{
    /// Creates an executor after checking its server-owned public error.
    ///
    /// # Errors
    ///
    /// Returns [`ToolExecutorBuildError`] if a built-in source-free error
    /// cannot be represented by the shared public error contract, or if secure
    /// entropy for the cursor signing key is unavailable.
    pub fn new(port: P) -> Result<Self, ToolExecutorBuildError> {
        Self::with_cursor_key_provider(port, &SystemCursorKeyProvider)
    }

    /// Creates an executor with a caller-provided cursor key.
    ///
    /// Test-only: a deterministic key makes cursor round-trips reproducible.
    /// Production must use [`Self::new`], which fails closed on missing entropy.
    #[cfg(test)]
    pub(crate) fn with_cursor_key(
        port: P,
        cursor_key: [u8; 32],
    ) -> Result<Self, ToolExecutorBuildError> {
        Self::build(port, CursorSigningKey::deterministic(cursor_key)?)
    }

    fn with_cursor_key_provider(
        port: P,
        provider: &impl CursorKeyProvider,
    ) -> Result<Self, ToolExecutorBuildError> {
        Self::build(port, provider.load()?)
    }

    fn build(port: P, cursor_key: CursorSigningKey) -> Result<Self, ToolExecutorBuildError> {
        let unsupported =
            mapped_public_error(MappedDomainFailure::unsupported_capability("arguments"))
                .map_err(ToolExecutorBuildError::UnsupportedError)?;
        let invalid_arguments =
            mapped_public_error(MappedDomainFailure::invalid_argument("arguments"))
                .map_err(ToolExecutorBuildError::InvalidArgumentError)?;
        let invalid_cursor = mapped_public_error(MappedDomainFailure::invalid_cursor())
            .map_err(ToolExecutorBuildError::InvalidArgumentError)?;
        let batch_validator = Arc::new(
            MaterializedToolValidator::compile().map_err(ToolExecutorBuildError::BatchValidator)?,
        );
        Ok(Self {
            port: Arc::new(port),
            invalid_arguments,
            unsupported,
            invalid_cursor,
            batch_validator,
            cursor_key,
        })
    }
}

impl<P> ToolExecutor for FirstSliceToolExecutor<P>
where
    P: FirstSliceClientPort,
{
    fn execute(
        &self,
        tool: VerticalTool,
        arguments: Map<String, Value>,
        exposure_profile: ExposureProfile,
        cancellation: RequestCancellation,
    ) -> ToolExecutionFuture {
        let port = Arc::clone(&self.port);
        let invalid_arguments = self.invalid_arguments.clone();
        let unsupported = self.unsupported.clone();
        let invalid_cursor = self.invalid_cursor.clone();
        let cursor_key = self.cursor_key;
        let batch_validator = Arc::clone(&self.batch_validator);
        Box::pin(async move {
            match tool {
                VerticalTool::RepoIndex => {
                    execute_repository_index(
                        port,
                        arguments,
                        cancellation,
                        &unsupported,
                        &invalid_arguments,
                    )
                    .await
                }
                VerticalTool::RepoStatus => {
                    execute_repo_status(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::RepoList => {
                    execute_repo_list(
                        port,
                        arguments,
                        exposure_profile,
                        cancellation,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::ChangeImpact => {
                    execute_change_impact(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::PlanChange => {
                    execute_plan_change(
                        port,
                        batch_validator,
                        arguments,
                        exposure_profile,
                        cancellation,
                        &unsupported,
                        &invalid_arguments,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::HistoryCompare => {
                    execute_history_compare(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::QueryAdvanced => {
                    execute_query_advanced(
                        port,
                        arguments,
                        exposure_profile,
                        cancellation,
                        &unsupported,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::SymbolRelationships => {
                    execute_symbol_relationships(
                        port,
                        arguments,
                        CursorPresentation {
                            shaping: ResponseShaping::Public,
                            exposure_profile,
                        },
                        cancellation,
                        &unsupported,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::FlowTrace => {
                    execute_flow_trace(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::ArchitectureCycles => {
                    execute_architecture_cycles(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::CodeDead => {
                    execute_code_dead(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::ArchitectureOverview => {
                    execute_architecture_overview(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::TestsSelect => {
                    execute_tests_select(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::ContextPack => {
                    execute_context_pack(
                        port,
                        batch_validator,
                        arguments,
                        exposure_profile,
                        cancellation,
                        &unsupported,
                        &invalid_arguments,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::QueryBatch => {
                    execute_query_batch(
                        port,
                        batch_validator,
                        arguments,
                        exposure_profile,
                        cancellation,
                        &unsupported,
                        &invalid_arguments,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::OperationStatus => {
                    execute_operation_status(port, arguments, cancellation).await
                }
                VerticalTool::CodeLocate => {
                    execute_code_locate(
                        port,
                        arguments,
                        CursorPresentation {
                            shaping: ResponseShaping::Public,
                            exposure_profile,
                        },
                        cancellation,
                        &unsupported,
                        &invalid_cursor,
                        cursor_key,
                    )
                    .await
                }
                VerticalTool::SymbolExplain => {
                    execute_symbol_explain(
                        port,
                        arguments,
                        ResponseShaping::Public,
                        cancellation,
                        &unsupported,
                    )
                    .await
                }
                VerticalTool::SourceRead => {
                    execute_source_read(
                        port,
                        arguments,
                        cancellation,
                        &unsupported,
                        &invalid_arguments,
                    )
                    .await
                }
            }
        })
    }
}

impl<P> fmt::Debug for FirstSliceToolExecutor<P> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FirstSliceToolExecutor")
            .finish_non_exhaustive()
    }
}

/// Builds the source-free `query.batch` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the shared generation); the batch
/// plan is validated but no operation runs. Each operation is reported as
/// not-run so the bounded result schema stays satisfied.
async fn explain_query_batch<P>(
    port: Arc<P>,
    repository: RepositoryId,
    input: &QueryBatchInput,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<QueryBatchData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request =
        RepositoryStatusPortRequest::new(repository, client_generation(input.generation.clone()));
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::query_batch_plan(input.operations.len()),
        &status.active_generation.to_string(),
    );
    let operation_results = input
        .operations
        .iter()
        .map(|operation| terminal_result(operation, BatchOperationStatus::NotRun))
        .collect();
    let data = QueryBatchData {
        batch_status: BatchStatus::Planned,
        generation_id: status.active_generation,
        operation_results,
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the batch boundary carries checked validators, public errors, and cursor state explicitly"
)]
async fn execute_query_batch<P>(
    port: Arc<P>,
    batch_validator: Arc<MaterializedToolValidator>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: QueryBatchInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    if !is_compact_profile(input.response_profile) {
        return Err(unsupported_field("response_profile"));
    }

    let tools: Vec<McpTool> = input
        .operations
        .iter()
        .map(|operation| mcp_tool_for_batch(operation.tool))
        .collect();
    let dependencies =
        resolve_batch_dependencies(&input.operations).map_err(batch_dependency_error)?;
    BatchPlan::validate(&tools, &dependencies).map_err(batch_plan_error)?;
    preflight_batch_capabilities(&input)?;

    let repository = repository_id(input.repository.clone(), unsupported)?;
    if explain_only {
        let output = explain_query_batch(port, repository, &input, cancellation).await?;
        return serialize_success(output);
    }
    let operation_failed =
        PublicError::builder(ErrorCode::Internal, BATCH_OPERATION_FAILED_MESSAGE)
            .build()
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    let budget_exceeded = authoritative_error(MappedDomainFailure::budget_exceeded());
    let adapter = Arc::new(McpAgentToolPort {
        port: Arc::clone(&port),
        validator: batch_validator,
        unsupported: unsupported.clone(),
        invalid_arguments: invalid_arguments.clone(),
        invalid_cursor: invalid_cursor.clone(),
        exposure_profile,
        cursor_key,
    });
    let errors = BatchPublicErrors::new(binding_invalid_error(), operation_failed, budget_exceeded);
    let output = BatchService
        .execute(adapter, input, repository, cancellation, errors)
        .await
        .map_err(map_batch_orchestration_error)?;
    serialize_success(output)
}

fn preflight_batch_capabilities(input: &QueryBatchInput) -> Result<(), ToolExecutionError> {
    for (index, operation) in input.operations.iter().enumerate() {
        let Some(tool) = vertical_tool_for_batch(operation.tool) else {
            return Err(unsupported_field("operations"));
        };
        let arguments = Value::Object(operation.arguments.clone());
        if let Err(error) = validate_capability_input(
            tool,
            &arguments,
            CapabilityBindingPolicy::RejectUnprovenRestrictedBindings,
        ) {
            let prefix = format!("operations.{index}.arguments");
            let public = error
                .to_public_error(Some(&prefix))
                .map_err(|_| internal(ToolExecutionFailure::Executor))?;
            return Err(ToolExecutionError::new(public));
        }
    }
    Ok(())
}

/// MCP adapter for the client-free agent tool port.
struct McpAgentToolPort<P> {
    port: Arc<P>,
    validator: Arc<MaterializedToolValidator>,
    unsupported: PublicError,
    invalid_arguments: PublicError,
    invalid_cursor: PublicError,
    exposure_profile: ExposureProfile,
    cursor_key: CursorSigningKey,
}

impl<P> AgentToolPort<RequestCancellation> for McpAgentToolPort<P>
where
    P: FirstSliceClientPort,
{
    fn resolve_identity(
        &self,
        request: AgentIdentityRequest,
        context: AgentResolutionContext<RequestCancellation>,
    ) -> AgentPortFuture<Result<AgentResolvedIdentity, AgentPortError>> {
        let port = Arc::clone(&self.port);
        let unsupported = self.unsupported.clone();
        let deadline = context.deadline();
        let cancellation = context.into_cancellation();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(AgentPortError::Cancelled);
            }
            let (repository, generation) = request.into_selectors();
            let repository =
                repository_id(repository, &unsupported).map_err(map_agent_child_error)?;
            let requested_generation = generation.clone();
            let request =
                RepositoryStatusPortRequest::new(repository, client_generation(generation));
            let operation = port.repository_status(request, cancellation.clone());
            let mut cancellation_wait = cancellation.clone();
            let response = tokio::select! {
                biased;
                _ = cancellation_wait.cancelled() => {
                    return Err(AgentPortError::Cancelled);
                }
                _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    return Err(AgentPortError::DeadlineExceeded);
                }
                response = operation => response,
            }
            .map_err(|_| AgentPortError::Unavailable)?;
            if response.repository_id != repository {
                return Err(AgentPortError::InvalidResponse);
            }
            if matches!(
                requested_generation,
                Some(GenerationSelector::Explicit(expected))
                    if response.resolved_generation != expected
            ) {
                return Err(AgentPortError::Public(Box::new(stale_generation_error())));
            }
            Ok(agent_identity_from_status(response))
        })
    }

    fn execute(
        &self,
        request: AgentToolRequest,
        context: AgentCallContext<RequestCancellation>,
    ) -> AgentPortFuture<Result<ReadEnvelope<Value>, AgentPortError>> {
        let port = Arc::clone(&self.port);
        let unsupported = self.unsupported.clone();
        let invalid_arguments = self.invalid_arguments.clone();
        let invalid_cursor = self.invalid_cursor.clone();
        let exposure_profile = self.exposure_profile;
        let cursor_key = self.cursor_key;
        let validator = Arc::clone(&self.validator);
        let budget = context.budget().clone();
        let local_budget = context.local_budget().cloned();
        let deadline = context.deadline();
        let local_deadline = context.has_local_deadline();
        let cancellation = context.into_cancellation();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(AgentPortError::Cancelled);
            }
            let (tool, mut arguments, binding_paths) = request.into_parts();
            validate_local_child_budget(tool, local_budget.as_ref())
                .map_err(map_agent_child_error)?;
            let vertical_tool = vertical_tool_for_batch(tool)
                .ok_or_else(|| AgentPortError::Public(Box::new(unsupported.clone())))?;
            validator
                .validate(vertical_tool, &arguments, exposure_profile)
                .map_err(|error| {
                    map_materialized_input_error(error, &binding_paths, &invalid_arguments)
                })?;
            apply_child_budget(tool, &budget, &mut arguments).map_err(map_agent_child_error)?;
            validator
                .validate(vertical_tool, &arguments, exposure_profile)
                .map_err(|error| {
                    map_materialized_input_error(error, &binding_paths, &invalid_arguments)
                })?;
            let operation = execute_agent_child(
                tool,
                port,
                validator,
                arguments,
                exposure_profile,
                cancellation.clone(),
                &unsupported,
                &invalid_arguments,
                &invalid_cursor,
                cursor_key,
            );
            let response = if let Some(deadline) = deadline {
                let mut cancellation_wait = cancellation.clone();
                tokio::select! {
                    biased;
                    _ = cancellation_wait.cancelled() => {
                        return Err(AgentPortError::Cancelled);
                    }
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        return Err(if local_deadline {
                            AgentPortError::LocalDeadlineExceeded
                        } else {
                            AgentPortError::DeadlineExceeded
                        });
                    }
                    response = operation => response,
                }
            } else {
                operation.await
            };
            let response = response.map_err(map_agent_child_error)?;
            let envelope: ReadEnvelope<Value> = serde_json::from_value(Value::Object(response))
                .map_err(|_| AgentPortError::InvalidResponse)?;
            Ok(envelope)
        })
    }
}

impl<P> PlanChangePort<RequestCancellation> for McpAgentToolPort<P>
where
    P: FirstSliceClientPort,
{
    fn resolve_identity(
        &self,
        request: AgentIdentityRequest,
        context: AgentResolutionContext<RequestCancellation>,
    ) -> rootlight_agent::change::PlanChangePortFuture<Result<AgentResolvedIdentity, AgentPortError>>
    {
        <Self as AgentToolPort<RequestCancellation>>::resolve_identity(self, request, context)
    }

    fn plan_change(
        &self,
        request: PlanChangeRequest,
        context: AgentCallContext<RequestCancellation>,
    ) -> rootlight_agent::change::PlanChangePortFuture<Result<PlanChangePortOutput, AgentPortError>>
    {
        let port = Arc::clone(&self.port);
        let deadline = context.deadline();
        let cancellation = context.into_cancellation();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(AgentPortError::Cancelled);
            }
            let expected = request.clone();
            let operation = port.plan_change(request, cancellation.clone());
            let response = if let Some(deadline) = deadline {
                let mut cancellation_wait = cancellation.clone();
                tokio::select! {
                    biased;
                    _ = cancellation_wait.cancelled() => {
                        return Err(AgentPortError::Cancelled);
                    }
                    _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                        return Err(AgentPortError::DeadlineExceeded);
                    }
                    response = operation => response,
                }
            } else {
                operation.await
            }
            .map_err(|_| AgentPortError::Unavailable)?;
            adapt_plan_change_response(response, &expected).map_err(map_agent_child_error)
        })
    }
}

fn vertical_tool_for_batch(tool: BatchTool) -> Option<VerticalTool> {
    match tool {
        BatchTool::CodeLocate => Some(VerticalTool::CodeLocate),
        BatchTool::SymbolExplain => Some(VerticalTool::SymbolExplain),
        BatchTool::SymbolRelationships => Some(VerticalTool::SymbolRelationships),
        BatchTool::FlowTrace => Some(VerticalTool::FlowTrace),
        BatchTool::ChangeImpact => Some(VerticalTool::ChangeImpact),
        BatchTool::TestsSelect => Some(VerticalTool::TestsSelect),
        BatchTool::ArchitectureOverview => Some(VerticalTool::ArchitectureOverview),
        BatchTool::ArchitectureCycles => Some(VerticalTool::ArchitectureCycles),
        BatchTool::CodeDead => Some(VerticalTool::CodeDead),
        BatchTool::ContextPack => Some(VerticalTool::ContextPack),
        BatchTool::SourceRead => Some(VerticalTool::SourceRead),
        BatchTool::PlanChange => None,
    }
}

fn apply_child_budget(
    tool: BatchTool,
    budget: &ResponseBudget,
    arguments: &mut Map<String, Value>,
) -> Result<(), ToolExecutionError> {
    if budget.evidence_level.is_some() {
        return Err(unsupported_field("budget"));
    }
    match tool {
        BatchTool::CodeLocate => {
            lower_numeric_argument(arguments, "max_results", budget.max_results.map(u64::from));
        }
        BatchTool::SymbolExplain => {
            if let Some(limit) = budget.max_results
                && let Some(Value::Array(symbols)) = arguments.get_mut("symbol_ids")
            {
                symbols.truncate(usize::from(limit));
            }
        }
        BatchTool::SymbolRelationships => {
            lower_numeric_argument(arguments, "max_results", budget.max_results.map(u64::from));
        }
        BatchTool::FlowTrace => {
            lower_numeric_argument(arguments, "max_depth", budget.max_depth.map(u64::from));
            lower_numeric_argument(arguments, "max_paths", budget.max_paths.map(u64::from));
            lower_numeric_argument(arguments, "max_paths", budget.max_results.map(u64::from));
        }
        BatchTool::ChangeImpact => {
            lower_numeric_argument(arguments, "max_depth", budget.max_depth.map(u64::from));
        }
        BatchTool::TestsSelect => {
            lower_numeric_argument(arguments, "max_tests", budget.max_results.map(u64::from));
        }
        BatchTool::ArchitectureOverview => {
            lower_numeric_argument(
                arguments,
                "max_components",
                budget.max_results.map(u64::from),
            );
        }
        BatchTool::ArchitectureCycles => {
            lower_numeric_argument(arguments, "max_cycles", budget.max_results.map(u64::from));
        }
        BatchTool::CodeDead => {
            lower_numeric_argument(
                arguments,
                "max_candidates",
                budget.max_results.map(u64::from),
            );
        }
        BatchTool::ContextPack => {
            if let Some(tokens) = budget.max_tokens {
                if tokens < 500 {
                    return Err(ToolExecutionError::new(authoritative_error(
                        MappedDomainFailure::budget_exceeded(),
                    )));
                }
                lower_numeric_argument(arguments, "token_budget", Some(u64::from(tokens)));
            }
        }
        BatchTool::SourceRead => {
            if let Some(limit) = budget.max_results
                && let Some(Value::Array(references)) = arguments.get_mut("references")
            {
                references.truncate(usize::from(limit));
            }
        }
        BatchTool::PlanChange => return Err(unsupported_field("operations")),
    }
    Ok(())
}

fn validate_local_child_budget(
    tool: BatchTool,
    local: Option<&ResponseBudget>,
) -> Result<(), ToolExecutionError> {
    let Some(local) = local else {
        return Ok(());
    };
    if local.evidence_level.is_some() {
        return Err(unsupported_field("local_budget_evidence_level"));
    }
    if local.max_tokens.is_some() && tool != BatchTool::ContextPack {
        return Err(unsupported_field("local_budget_max_tokens"));
    }
    if local.max_source_bytes.is_some() {
        return Err(unsupported_field("local_budget_max_source_bytes"));
    }
    if local.max_traversal_facts.is_some() {
        return Err(unsupported_field("local_budget_max_traversal_facts"));
    }
    if local.max_depth.is_some() && !matches!(tool, BatchTool::FlowTrace | BatchTool::ChangeImpact)
    {
        return Err(unsupported_field("local_budget_max_depth"));
    }
    if local.max_paths.is_some() && tool != BatchTool::FlowTrace {
        return Err(unsupported_field("local_budget_max_paths"));
    }
    if local.max_results.is_some()
        && matches!(tool, BatchTool::ChangeImpact | BatchTool::ContextPack)
    {
        return Err(unsupported_field("local_budget_max_results"));
    }
    Ok(())
}

fn lower_numeric_argument(arguments: &mut Map<String, Value>, field: &str, limit: Option<u64>) {
    let Some(limit) = limit else {
        return;
    };
    match arguments.get_mut(field) {
        Some(Value::Number(current)) => {
            if let Some(value) = current.as_u64() {
                *current = serde_json::Number::from(value.min(limit));
            }
        }
        Some(_) => {}
        None => {
            arguments.insert(field.to_owned(), Value::from(limit));
        }
    }
}

fn map_materialized_input_error(
    error: MaterializedInputError,
    binding_paths: &[String],
    invalid_arguments: &PublicError,
) -> AgentPortError {
    match error {
        MaterializedInputError::Invalid { instance_path } => {
            let public = if instance_path
                .as_deref()
                .is_some_and(|path| binding_path_overlaps(path, binding_paths))
            {
                binding_type_mismatch_error()
            } else {
                invalid_arguments.clone()
            };
            AgentPortError::Public(Box::new(public))
        }
        MaterializedInputError::Public(error) => {
            if error.code() == ErrorCode::TypeMismatch
                && public_error_overlaps_bindings(&error, binding_paths)
            {
                AgentPortError::Public(Box::new(binding_type_mismatch_error()))
            } else {
                AgentPortError::Public(error)
            }
        }
    }
}

fn public_error_overlaps_bindings(error: &PublicError, binding_paths: &[String]) -> bool {
    error.next_actions().iter().any(|action| {
        let NextAction::CorrectField { field } = action else {
            return false;
        };
        let path = format!("/{}", field.as_str());
        binding_path_overlaps(&path, binding_paths)
    })
}

fn binding_path_overlaps(error_path: &str, binding_paths: &[String]) -> bool {
    if error_path.is_empty() {
        return false;
    }
    binding_paths.iter().any(|binding_path| {
        json_pointer_is_ancestor(error_path, binding_path)
            || json_pointer_is_ancestor(binding_path, error_path)
    })
}

fn json_pointer_is_ancestor(ancestor: &str, descendant: &str) -> bool {
    ancestor == descendant
        || descendant
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

/// Maps one admitted child request to its concrete MCP/daemon adapter.
#[expect(
    clippy::too_many_arguments,
    reason = "the child boundary carries checked validators, public errors, and cursor state explicitly"
)]
async fn execute_agent_child<P>(
    tool: BatchTool,
    port: Arc<P>,
    validator: Arc<MaterializedToolValidator>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    match tool {
        BatchTool::CodeLocate => {
            execute_code_locate(
                port,
                arguments,
                CursorPresentation {
                    shaping: ResponseShaping::CanonicalInternal,
                    exposure_profile,
                },
                cancellation,
                unsupported,
                invalid_cursor,
                cursor_key,
            )
            .await
        }
        BatchTool::SymbolExplain => {
            execute_symbol_explain(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::SourceRead => {
            execute_source_read(
                port,
                arguments,
                cancellation,
                unsupported,
                invalid_arguments,
            )
            .await
        }
        BatchTool::SymbolRelationships => {
            execute_symbol_relationships(
                port,
                arguments,
                CursorPresentation {
                    shaping: ResponseShaping::CanonicalInternal,
                    exposure_profile,
                },
                cancellation,
                unsupported,
                invalid_cursor,
                cursor_key,
            )
            .await
        }
        BatchTool::FlowTrace => {
            execute_flow_trace(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::ChangeImpact => {
            execute_change_impact(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::TestsSelect => {
            execute_tests_select(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::ArchitectureOverview => {
            execute_architecture_overview(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::ArchitectureCycles => {
            execute_architecture_cycles(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::CodeDead => {
            execute_code_dead(
                port,
                arguments,
                ResponseShaping::CanonicalInternal,
                cancellation,
                unsupported,
            )
            .await
        }
        BatchTool::ContextPack => {
            execute_context_pack(
                port,
                validator,
                arguments,
                exposure_profile,
                cancellation,
                unsupported,
                invalid_arguments,
                invalid_cursor,
                cursor_key,
            )
            .await
        }
        BatchTool::PlanChange => Err(ToolExecutionError::new(unsupported.clone())),
    }
}

fn map_agent_child_error(error: ToolExecutionError) -> AgentPortError {
    if let Some(public) = error.public_error() {
        return AgentPortError::Public(Box::new(public.clone()));
    }
    match error.failure() {
        Some(ToolExecutionFailure::InvalidResponse) => AgentPortError::InvalidResponse,
        Some(ToolExecutionFailure::Transport | ToolExecutionFailure::Executor) | None => {
            AgentPortError::Unavailable
        }
    }
}

fn map_batch_orchestration_error(error: BatchOrchestrationError) -> ToolExecutionError {
    match error {
        BatchOrchestrationError::InvalidArguments => invalid_input(),
        BatchOrchestrationError::UnsupportedProfile => unsupported_field("response_profile"),
        BatchOrchestrationError::BudgetExceeded | BatchOrchestrationError::DeadlineExceeded => {
            ToolExecutionError::new(authoritative_error(MappedDomainFailure::budget_exceeded()))
        }
        BatchOrchestrationError::IdentityResolution(error) => ToolExecutionError::new(*error),
        BatchOrchestrationError::Cancelled | BatchOrchestrationError::Internal => {
            internal(ToolExecutionFailure::Executor)
        }
        BatchOrchestrationError::InvalidResponse => internal(ToolExecutionFailure::InvalidResponse),
        _ => internal(ToolExecutionFailure::Executor),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "context assembly carries checked validators, public errors, and cursor state explicitly"
)]
async fn execute_context_pack<P>(
    port: Arc<P>,
    validator: Arc<MaterializedToolValidator>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: ContextPackInput = decode_input(arguments)?;
    let repository = repository_id(input.repository.clone(), unsupported)?;
    let adapter = Arc::new(McpAgentToolPort {
        port,
        validator,
        unsupported: unsupported.clone(),
        invalid_arguments: invalid_arguments.clone(),
        invalid_cursor: invalid_cursor.clone(),
        exposure_profile,
        cursor_key,
    });
    let output = ContextPackService
        .execute(adapter, input, repository, cancellation)
        .await
        .map_err(|error| map_context_pack_service_error(error, unsupported))?;
    serialize_success(output)
}

fn map_context_pack_service_error(
    error: ContextPackServiceError,
    unsupported: &PublicError,
) -> ToolExecutionError {
    match error {
        ContextPackServiceError::UnsupportedField(field) => unsupported_field(field),
        ContextPackServiceError::EmptySeeds => ToolExecutionError::new(unsupported.clone()),
        ContextPackServiceError::Public(error) => ToolExecutionError::new(*error),
        ContextPackServiceError::DeadlineExceeded => {
            ToolExecutionError::new(authoritative_error(MappedDomainFailure::budget_exceeded()))
        }
        ContextPackServiceError::InvalidResponse => internal(ToolExecutionFailure::InvalidResponse),
        ContextPackServiceError::Cancelled | ContextPackServiceError::Unavailable => {
            internal(ToolExecutionFailure::Executor)
        }
        _ => internal(ToolExecutionFailure::Executor),
    }
}

/// Lists one immutable page of repositories known to the daemon process.
async fn execute_repo_list<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: RepoListInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    if !is_compact_profile(input.response_profile) {
        return Err(unsupported_field("response_profile"));
    }
    let page_size = input.max_results.unwrap_or(DEFAULT_REPO_LIST_RESULTS);
    let states_were_present = input.states.is_some();
    let mut states = input.states.unwrap_or_default();
    states.sort_unstable();
    states.dedup();
    let client_states: Vec<_> = states.iter().copied().map(client_catalog_state).collect();
    let client_states = states_were_present.then_some(client_states);
    let normalized = client::RepositoryCatalogPageRequest::new(
        page_size,
        input.query.as_deref(),
        client_states.as_deref(),
        None,
        None,
    )
    .map_err(|_| invalid_input())?;
    let normalized_query = normalized.normalized_query().map(str::to_owned);
    let canonical_states = normalized.states().map(<[_]>::to_vec);
    let plan_context = rootlight_agent::explain::RepoListPlanContext::new(
        page_size,
        normalized_query.is_some(),
        states.iter().copied(),
        ResponseProfile::Compact,
    )
    .map_err(|_| invalid_input())?;
    let plan = rootlight_agent::explain::repo_list_plan(&plan_context);

    // Snapshot bytes are read from the opaque cursor only to construct the
    // expected context. No daemon lookup occurs until the MAC and every bound
    // request dimension have been validated.
    let parsed_cursor = input
        .cursor
        .as_ref()
        .map(|cursor| {
            AuthenticatedCursor::from_wire(cursor.as_str())
                .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))
        })
        .transpose()?;
    let cursor_snapshot = parsed_cursor.as_ref().map(AuthenticatedCursor::snapshot_id);
    let continuation = if let Some(parsed) = parsed_cursor.as_ref() {
        let snapshot = cursor_snapshot.expect("parsed cursor exposes its snapshot");
        let context = repo_list_cursor_context(
            normalized_query.as_deref(),
            canonical_states.as_deref(),
            page_size,
            snapshot,
            &plan,
            exposure_profile,
            cursor_key.key_id,
        );
        parsed
            .validate(&context, now_unix_ms(), &cursor_key.secret)
            .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?;
        Some(
            client::RepositoryCatalogSortKey::from_bytes(parsed.last_sort_key())
                .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?,
        )
    } else {
        None
    };

    let snapshot = cursor_snapshot.map(client::RepositoryCatalogSnapshotId::from_bytes);
    let request = client::RepositoryCatalogPageRequest::new(
        page_size,
        normalized_query.as_deref(),
        canonical_states.as_deref(),
        snapshot,
        continuation,
    )
    .map_err(|_| invalid_input())?;
    let future = port.repository_catalog_page(request, cancellation.clone());
    let page = await_port(future, cancellation).await?;
    let total_count = page
        .total_count
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    let snapshot_bytes = *page.snapshot_id.as_bytes();
    let snapshot_id = printable_catalog_snapshot(snapshot_bytes)?;
    let repositories = if explain_only {
        Vec::new()
    } else {
        page.repositories
            .into_iter()
            .map(map_catalog_entry)
            .collect::<Result<Vec<_>, _>>()?
    };
    let rows = u64::try_from(repositories.len())
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let truncated = !explain_only && page.truncated;
    let context = repo_list_cursor_context(
        normalized_query.as_deref(),
        canonical_states.as_deref(),
        page_size,
        snapshot_bytes,
        &plan,
        exposure_profile,
        cursor_key.key_id,
    );
    let next_cursor = if truncated {
        let next_after = page
            .next_after
            .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
        let next = AuthenticatedCursor::create(
            context,
            next_after.as_bytes().to_vec(),
            now_unix_ms(),
            &cursor_key.secret,
        )
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        RequiredNullable(Some(
            ContinuationCursor::parse(&next.to_wire())
                .map_err(|_| internal(ToolExecutionFailure::Executor))?,
        ))
    } else {
        RequiredNullable(None)
    };
    let data = RepoListData {
        repositories,
        total_count,
        explanation: explain_only
            .then(|| rootlight_agent::explain::finalize_plan(plan, snapshot_id.as_str())),
    };
    let envelope = CatalogEnvelope {
        schema_version: RepoListSchemaVersion::V2_0,
        snapshot_id,
        data,
        truncated,
        next_cursor,
        usage: UsageSummary {
            rows,
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 0,
            cache_status: CacheStatus::NotApplicable,
            trace_id: "catalog-page".to_owned(),
        },
        warnings: Vec::new(),
        trust: TrustClassification::UntrustedRepositoryData,
    };
    serialize_catalog_success(envelope, started_at)
}

fn map_catalog_entry(
    entry: client::RepositoryCatalogEntry,
) -> Result<RepositoryEntry, ToolExecutionError> {
    let has_active_generation = entry.active_generation.is_some();
    let coverage = if entry.coverage.is_empty() {
        None
    } else {
        Some(status_coverage_report_checked(&entry.coverage)?)
    };
    Ok(RepositoryEntry {
        repository_id: entry.repository_id,
        display_name: entry.display_name,
        state: catalog_repository_state(entry.state),
        active_generation: RequiredNullable(entry.active_generation),
        generation_count: entry.generation_count,
        alias: RequiredNullable(entry.alias),
        languages: entry.languages,
        structural_freshness: RequiredNullable(
            has_active_generation.then(|| catalog_freshness(entry.structural_freshness)),
        ),
        semantic_freshness: RequiredNullable(
            has_active_generation.then(|| catalog_freshness(entry.semantic_freshness)),
        ),
        coverage: RequiredNullable(coverage),
    })
}

fn status_coverage_report_checked(
    entries: &[client::RepositoryCoverageEntry],
) -> Result<CoverageReport, ToolExecutionError> {
    let total_files = checked_coverage_sum(entries, |entry| entry.discovered_files)?;
    let indexed_files = checked_coverage_sum(entries, |entry| entry.indexed_files)?;
    let languages = entries
        .iter()
        .map(|entry| LanguageCoverageReport {
            language: entry.language.clone(),
            tier: tier_label(&entry.tier),
            files_indexed: entry.indexed_files,
            files_skipped: entry.discovered_files.saturating_sub(entry.indexed_files),
            missing_build_context: 0,
        })
        .collect();
    Ok(CoverageReport {
        status: aggregate_coverage_status(entries),
        languages,
        total_files,
        indexed_files,
        skipped_files: total_files.saturating_sub(indexed_files),
    })
}

fn checked_coverage_sum(
    entries: &[client::RepositoryCoverageEntry],
    value: impl Fn(&client::RepositoryCoverageEntry) -> u64,
) -> Result<u64, ToolExecutionError> {
    entries.iter().try_fold(0_u64, |sum, entry| {
        sum.checked_add(value(entry))
            .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))
    })
}

/// Default page size for `repo.list`.
const DEFAULT_REPO_LIST_RESULTS: u16 = 20;

/// Builds the list-level cursor context for `repo.list`.
///
/// `repo.list` has no single repository generation, so the cursor derives
/// catalog-level identities from the immutable snapshot rather than using
/// ambiguous all-zero sentinels.
fn repo_list_cursor_context(
    query: Option<&str>,
    states: Option<&[client::RepositoryCatalogState]>,
    page_size: u16,
    snapshot_id: [u8; 32],
    plan: &PlanExplanation,
    exposure_profile: ExposureProfile,
    key_id: u64,
) -> CursorContext {
    let repository_digest = domain_hash(b"rootlight.repo-list.repository.v1", &snapshot_id);
    let generation_digest = domain_hash(b"rootlight.repo-list.generation.v1", &snapshot_id);
    CursorContext {
        repository: RepositoryId::from_bytes(
            repository_digest[..16]
                .try_into()
                .expect("digest contains a repository identity"),
        ),
        generation: GenerationId::from_bytes(
            generation_digest[..20]
                .try_into()
                .expect("digest contains a generation identity"),
        ),
        tool: McpTool::RepoList,
        tool_major_version: 2,
        query_fingerprint: repo_list_fingerprint(query, states, page_size),
        plan_fingerprint: repo_list_plan_fingerprint(plan, &snapshot_id),
        response_profile: ResponseProfile::Compact,
        exposure_profile,
        snapshot_id,
        page_size,
        key_id,
    }
}

fn repo_list_fingerprint(
    query: Option<&str>,
    states: Option<&[client::RepositoryCatalogState]>,
    page_size: u16,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.repo-list.request.v2");
    match query {
        Some(query) => {
            hasher.update(&[1]);
            hash_length_prefixed(&mut hasher, query.as_bytes());
        }
        None => {
            hasher.update(&[0]);
        }
    }
    match states {
        Some(states) => {
            hasher.update(&[1]);
            hasher.update(
                &u64::try_from(states.len())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            for state in states {
                hash_length_prefixed(&mut hasher, catalog_state_label(*state).as_bytes());
            }
        }
        None => {
            hasher.update(&[0]);
        }
    }
    hasher.update(&page_size.to_le_bytes());
    hasher.update(&2_u16.to_le_bytes());
    hasher.update(&u32::from(rootlight_agent::explain::REPO_LIST_SORT_VERSION).to_le_bytes());
    hasher.update(&[0]);
    *hasher.finalize().as_bytes()
}

fn repo_list_plan_fingerprint(plan: &PlanExplanation, snapshot_id: &[u8; 32]) -> [u8; 32] {
    let catalog_identity = printable_catalog_snapshot_text(*snapshot_id);
    rootlight_agent::explain::physical_plan_fingerprint(plan, &catalog_identity)
}

fn printable_catalog_snapshot(
    snapshot_id: [u8; 32],
) -> Result<CatalogSnapshotId, ToolExecutionError> {
    CatalogSnapshotId::parse(&printable_catalog_snapshot_text(snapshot_id))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn printable_catalog_snapshot_text(snapshot_id: [u8; 32]) -> String {
    format!(
        "catalog1_{}",
        blake3::Hash::from_bytes(snapshot_id).to_hex()
    )
}

fn hash_length_prefixed(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(value);
}

fn domain_hash(domain: &[u8], value: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    hash_length_prefixed(&mut hasher, value);
    *hasher.finalize().as_bytes()
}

fn parse_repository_cursor(
    cursor: Option<&ContinuationCursor>,
    invalid_cursor: &PublicError,
) -> Result<Option<AuthenticatedCursor>, ToolExecutionError> {
    cursor
        .map(|cursor| {
            AuthenticatedCursor::from_wire(cursor.as_str())
                .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))
        })
        .transpose()
}

fn pin_request_generation(
    generation: &mut client::GenerationSelector,
    cursor: &AuthenticatedCursor,
    invalid_cursor: &PublicError,
) -> Result<(), ToolExecutionError> {
    match *generation {
        client::GenerationSelector::Active => {
            *generation = client::GenerationSelector::Generation(cursor.generation());
            Ok(())
        }
        client::GenerationSelector::Generation(requested) if requested == cursor.generation() => {
            Ok(())
        }
        client::GenerationSelector::Generation(_) => {
            Err(ToolExecutionError::new(invalid_cursor.clone()))
        }
    }
}

fn decode_page_offset(
    bytes: &[u8],
    invalid_cursor: &PublicError,
) -> Result<u64, ToolExecutionError> {
    let offset = bytes
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?;
    if offset == 0 {
        return Err(ToolExecutionError::new(invalid_cursor.clone()));
    }
    Ok(offset)
}

fn validate_repository_cursor(
    cursor: &AuthenticatedCursor,
    context: &CursorContext,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<(), ToolExecutionError> {
    cursor
        .validate(context, now_unix_ms(), &cursor_key.secret)
        .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))
}

fn create_page_cursor(
    next_offset: Option<u64>,
    context: CursorContext,
    cursor_key: CursorSigningKey,
) -> Result<Option<ContinuationCursor>, ToolExecutionError> {
    next_offset
        .map(|offset| {
            let cursor = AuthenticatedCursor::create(
                context,
                offset.to_be_bytes().to_vec(),
                now_unix_ms(),
                &cursor_key.secret,
            )
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
            ContinuationCursor::parse(&cursor.to_wire())
                .map_err(|_| internal(ToolExecutionFailure::Executor))
        })
        .transpose()
}

fn repository_snapshot_id(repository: RepositoryId, generation: GenerationId) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.repository-generation.snapshot.v1");
    hasher.update(repository.as_bytes());
    hasher.update(generation.as_bytes());
    *hasher.finalize().as_bytes()
}

#[expect(
    clippy::too_many_arguments,
    reason = "the cursor context must bind every independent public request dimension"
)]
fn repository_cursor_context(
    tool: McpTool,
    repository: RepositoryId,
    generation: GenerationId,
    exposure_profile: ExposureProfile,
    response_profile: ResponseProfile,
    page_size: u16,
    query_fingerprint: [u8; 32],
    plan_fingerprint: [u8; 32],
    key_id: u64,
) -> CursorContext {
    CursorContext {
        repository,
        generation,
        tool,
        tool_major_version: 1,
        query_fingerprint,
        plan_fingerprint,
        response_profile,
        exposure_profile,
        snapshot_id: repository_snapshot_id(repository, generation),
        page_size,
        key_id,
    }
}

fn code_locate_cursor_context(
    request: &CodeLocatePortRequest,
    generation: GenerationId,
    exposure_profile: ExposureProfile,
    response_profile: ResponseProfile,
    key_id: u64,
) -> CursorContext {
    let mut request_hasher = blake3::Hasher::new();
    request_hasher.update(b"rootlight.code-locate.request.v1");
    request_hasher.update(request.repository.as_bytes());
    request_hasher.update(request.query.as_bytes());
    request_hasher.update(&[locate_mode_tag(request.mode)]);
    request_hasher.update(&request.maximum_results.to_le_bytes());
    let query_fingerprint = *request_hasher.finalize().as_bytes();
    let mut plan_material = Vec::from(query_fingerprint);
    plan_material.extend_from_slice(b"lexical-rank-desc-symbol-id-asc.v1");
    repository_cursor_context(
        McpTool::CodeLocate,
        request.repository,
        generation,
        exposure_profile,
        response_profile,
        u16::try_from(request.maximum_results).expect("public locate page size is at most 200"),
        query_fingerprint,
        domain_hash(b"rootlight.code-locate.plan.v1", &plan_material),
        key_id,
    )
}

fn locate_mode_tag(mode: LocateMode) -> u8 {
    match mode {
        LocateMode::Exact => 0,
        LocateMode::Prefix => 1,
        LocateMode::Text => 2,
        LocateMode::SafeRegex => 3,
        LocateMode::Glob => 4,
    }
}

fn symbol_relationships_cursor_context(
    request: &SymbolRelationshipsPortRequest,
    generation: GenerationId,
    exposure_profile: ExposureProfile,
    response_profile: ResponseProfile,
    key_id: u64,
) -> CursorContext {
    let mut request_hasher = blake3::Hasher::new();
    request_hasher.update(b"rootlight.symbol-relationships.request.v1");
    request_hasher.update(request.repository.as_bytes());
    for seed in &request.seeds {
        request_hasher.update(seed.as_bytes());
    }
    for relation in &request.relations {
        request_hasher.update(relation.as_bytes());
        request_hasher.update(&[0]);
    }
    request_hasher.update(request.direction.as_deref().unwrap_or("natural").as_bytes());
    request_hasher.update(&request.min_confidence.unwrap_or(700).to_le_bytes());
    request_hasher.update(&request.max_results.unwrap_or(50).to_le_bytes());
    let query_fingerprint = *request_hasher.finalize().as_bytes();
    let mut plan_material = Vec::from(query_fingerprint);
    plan_material.extend_from_slice(b"seed-family-direction-target-confidence.v1");
    repository_cursor_context(
        McpTool::SymbolRelationships,
        request.repository,
        generation,
        exposure_profile,
        response_profile,
        request.max_results.unwrap_or(50),
        query_fingerprint,
        domain_hash(b"rootlight.symbol-relationships.plan.v1", &plan_material),
        key_id,
    )
}

fn query_advanced_cursor_context(
    request: &QueryAdvancedPortRequest,
    generation: GenerationId,
    exposure_profile: ExposureProfile,
    key_id: u64,
) -> CursorContext {
    let mut request_hasher = blake3::Hasher::new();
    request_hasher.update(b"rootlight.query-advanced.request.v1");
    request_hasher.update(request.repository.as_bytes());
    request_hasher.update(request.query_ast.as_bytes());
    request_hasher.update(&[u8::from(request.explain.unwrap_or(false))]);
    request_hasher.update(
        &request
            .max_results
            .unwrap_or(DEFAULT_ADVANCED_RESULTS)
            .to_le_bytes(),
    );
    request_hasher.update(&request.max_depth.unwrap_or(3).to_le_bytes());
    request_hasher.update(&request.cost_limit.unwrap_or(u64::MAX).to_le_bytes());
    let query_fingerprint = *request_hasher.finalize().as_bytes();
    let mut plan_material = Vec::from(query_fingerprint);
    plan_material.extend_from_slice(b"typed-ast-explicit-sort-stable-input-order.v1");
    repository_cursor_context(
        McpTool::QueryAdvanced,
        request.repository,
        generation,
        exposure_profile,
        ResponseProfile::Compact,
        request.max_results.unwrap_or(DEFAULT_ADVANCED_RESULTS),
        query_fingerprint,
        domain_hash(b"rootlight.query-advanced.plan.v1", &plan_material),
        key_id,
    )
}

const fn client_catalog_state(state: RepositoryState) -> client::RepositoryCatalogState {
    match state {
        RepositoryState::Ready => client::RepositoryCatalogState::Ready,
        RepositoryState::Indexing => client::RepositoryCatalogState::Indexing,
        RepositoryState::Degraded => client::RepositoryCatalogState::Degraded,
        RepositoryState::Corrupt => client::RepositoryCatalogState::Corrupt,
        RepositoryState::MigrationRequired => client::RepositoryCatalogState::MigrationRequired,
        RepositoryState::RebuildRequired => client::RepositoryCatalogState::RebuildRequired,
    }
}

const fn catalog_repository_state(state: client::RepositoryCatalogState) -> RepositoryState {
    match state {
        client::RepositoryCatalogState::Ready => RepositoryState::Ready,
        client::RepositoryCatalogState::Indexing => RepositoryState::Indexing,
        client::RepositoryCatalogState::Degraded => RepositoryState::Degraded,
        client::RepositoryCatalogState::Corrupt => RepositoryState::Corrupt,
        client::RepositoryCatalogState::MigrationRequired => RepositoryState::MigrationRequired,
        client::RepositoryCatalogState::RebuildRequired => RepositoryState::RebuildRequired,
    }
}

const fn catalog_state_label(state: client::RepositoryCatalogState) -> &'static str {
    match state {
        client::RepositoryCatalogState::Ready => "ready",
        client::RepositoryCatalogState::Indexing => "indexing",
        client::RepositoryCatalogState::Degraded => "degraded",
        client::RepositoryCatalogState::Corrupt => "corrupt",
        client::RepositoryCatalogState::MigrationRequired => "migration_required",
        client::RepositoryCatalogState::RebuildRequired => "rebuild_required",
    }
}

const fn catalog_freshness(freshness: client::RepositoryCatalogFreshness) -> Freshness {
    match freshness {
        client::RepositoryCatalogFreshness::Current => Freshness::Current,
        client::RepositoryCatalogFreshness::Superseded => Freshness::Superseded,
        client::RepositoryCatalogFreshness::Stale => Freshness::Stale,
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Reads one repository's active or exact generation status.
async fn execute_repo_status<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: RepoStatusInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let repository = repository_id(input.repository.clone(), unsupported)?;
    let coverage_detail = match input.coverage_detail.unwrap_or(CoverageDetail::Summary) {
        CoverageDetail::Summary => client::RepositoryStatusCoverageDetail::Summary,
        CoverageDetail::Language => client::RepositoryStatusCoverageDetail::Language,
        CoverageDetail::Project | CoverageDetail::File => {
            return Err(unsupported_field("coverage_detail"));
        }
    };
    let require_freshness = match input
        .require_freshness
        .unwrap_or(FreshnessRequirement::None)
    {
        FreshnessRequirement::None => client::RepositoryStatusFreshnessRequirement::None,
        FreshnessRequirement::Structural => {
            client::RepositoryStatusFreshnessRequirement::Structural
        }
        FreshnessRequirement::Semantic => client::RepositoryStatusFreshnessRequirement::Semantic,
    };
    if input.budget.is_some() {
        return Err(unsupported_field("budget"));
    }
    if !is_compact_profile(input.response_profile) {
        return Err(unsupported_field("response_profile"));
    }
    let requested_generation = match input.generation.as_ref() {
        Some(GenerationSelector::Explicit(generation)) => Some(*generation),
        Some(GenerationSelector::Active(_)) | None => None,
    };
    let request = RepositoryStatusPortRequest::new(repository, client_generation(input.generation))
        .with_controls(
            coverage_detail,
            input.include_operations.unwrap_or(false),
            require_freshness,
        );
    let future = port.repository_status(request, cancellation.clone());
    let status = await_port(future, cancellation).await?;

    let generation_summary = GenerationSummary {
        generation_id: status.resolved_generation,
        parent_generation: RequiredNullable(status.parent_generation),
        structural_freshness: freshness_from_label(&status.structural_freshness),
        semantic_freshness: freshness_from_label(&status.semantic_freshness),
    };
    let active_generation_summary = if status.active_generation == status.resolved_generation {
        generation_summary.clone()
    } else {
        GenerationSummary {
            generation_id: status.active_generation,
            parent_generation: RequiredNullable(status.active_parent_generation),
            structural_freshness: freshness_from_label(&status.active_structural_freshness),
            semantic_freshness: freshness_from_label(&status.active_semantic_freshness),
        }
    };
    let summary_languages: Vec<LanguageCoverage> = status
        .coverage
        .iter()
        .map(|entry| LanguageCoverage {
            language: entry.language.clone(),
            tier: analysis_tier(&entry.tier),
            status: coverage_status_from_label(&entry.status),
        })
        .collect();
    let detailed_coverage = status_coverage_report(&status.coverage);
    let operations = status
        .operations
        .iter()
        .map(|operation| OperationSummary {
            operation_id: operation.operation,
            kind: "repository_index".to_owned(),
            state: operation_state(operation.state),
            progress_permille: operation_progress_permille(
                operation.state,
                operation.completed_units,
                operation.total_units,
            ),
            owned_by_session: operation.owned_by_client,
        })
        .collect::<Vec<_>>();
    let publication_state = match status.publication_state.as_str() {
        "published" => GenerationPublicationState::Published,
        "retained" => GenerationPublicationState::Retained,
        _ => return Err(internal(ToolExecutionFailure::InvalidResponse)),
    };
    let mut recommended_actions = Vec::new();
    let mut warnings = Vec::new();
    if !matches!(
        freshness_from_label(&status.structural_freshness),
        Freshness::Current
    ) || !matches!(
        freshness_from_label(&status.semantic_freshness),
        Freshness::Current
    ) {
        recommended_actions.push(source_free_message("index repository")?);
        warnings.push(ResponseWarning {
            code: SafeLabel::parse("stale_generation")
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
            message: source_free_message("selected generation is not current")?,
        });
    }
    if operations.iter().any(|operation| {
        matches!(
            operation.state,
            OperationState::Queued | OperationState::Running
        )
    }) {
        recommended_actions.push(source_free_message("inspect operation")?);
    }
    let repository_state = repository_state(&status.state)
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    if matches!(
        repository_state,
        RepositoryState::Corrupt | RepositoryState::RebuildRequired
    ) {
        recommended_actions.push(source_free_message("rebuild repository")?);
    }
    let data = RepoStatusData {
        repository_state,
        requested_generation: RequiredNullable(requested_generation),
        resolved_generation: status.resolved_generation,
        active_generation: RequiredNullable(Some(active_generation_summary)),
        publication_state,
        alias: RequiredNullable(status.alias.clone()),
        coverage: CoverageReport {
            languages: if matches!(
                coverage_detail,
                client::RepositoryStatusCoverageDetail::Language
            ) {
                detailed_coverage.languages
            } else {
                Vec::new()
            },
            ..detailed_coverage
        },
        operations,
        recommended_actions,
        explanation: explain_only.then(|| {
            rootlight_agent::explain::finalize_plan(
                rootlight_agent::explain::repo_status_plan(),
                &status.resolved_generation.to_string(),
            )
        }),
    };
    let envelope = ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: ResolvedRepository {
            repository_id: status.repository_id,
            display_name: status.display_name,
        },
        generation: generation_summary,
        coverage: CoverageSummary {
            status: aggregate_coverage_status(&status.coverage),
            languages: summary_languages,
            skipped_inputs: 0,
        },
        data,
        truncated: false,
        completeness: ResultCompleteness::complete(),
        next_cursor: RequiredNullable(None),
        usage: UsageSummary {
            rows: 1_u64
                .saturating_add(u64::try_from(status.coverage.len()).unwrap_or(u64::MAX))
                .saturating_add(u64::try_from(status.operations.len()).unwrap_or(u64::MAX)),
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 0,
            cache_status: CacheStatus::Miss,
            trace_id: "repo-status".to_owned(),
        },
        warnings,
        trust: TrustClassification::UntrustedRepositoryData,
    };
    serialize_measured_read_success(envelope, started_at)
}

fn status_coverage_report(entries: &[client::RepositoryCoverageEntry]) -> CoverageReport {
    let languages: Vec<LanguageCoverageReport> = entries
        .iter()
        .map(|entry| LanguageCoverageReport {
            language: entry.language.clone(),
            tier: tier_label(&entry.tier),
            files_indexed: entry.indexed_files,
            files_skipped: entry.discovered_files.saturating_sub(entry.indexed_files),
            missing_build_context: 0,
        })
        .collect();
    let total_files: u64 = entries.iter().map(|entry| entry.discovered_files).sum();
    let indexed_files: u64 = entries.iter().map(|entry| entry.indexed_files).sum();
    CoverageReport {
        status: aggregate_coverage_status(entries),
        languages,
        total_files,
        indexed_files,
        skipped_files: total_files.saturating_sub(indexed_files),
    }
}

fn aggregate_coverage_status(
    entries: &[client::RepositoryCoverageEntry],
) -> rootlight_ir::CoverageStatus {
    if entries.iter().any(|entry| entry.status == "unknown") {
        rootlight_ir::CoverageStatus::Unknown
    } else if entries.iter().any(|entry| entry.status == "sampled") {
        rootlight_ir::CoverageStatus::Sampled
    } else if entries.iter().any(|entry| entry.status == "bounded") {
        rootlight_ir::CoverageStatus::Bounded
    } else if entries.is_empty() {
        rootlight_ir::CoverageStatus::Unknown
    } else {
        rootlight_ir::CoverageStatus::Complete
    }
}

fn repository_state(label: &str) -> Option<RepositoryState> {
    match label {
        "ready" => Some(RepositoryState::Ready),
        "indexing" => Some(RepositoryState::Indexing),
        "degraded" => Some(RepositoryState::Degraded),
        "corrupt" => Some(RepositoryState::Corrupt),
        "migration_required" => Some(RepositoryState::MigrationRequired),
        "rebuild_required" => Some(RepositoryState::RebuildRequired),
        _ => None,
    }
}

fn operation_progress_permille(state: client::OperationState, completed: u32, total: u32) -> u16 {
    if total == 0 {
        return if state == client::OperationState::Succeeded {
            1_000
        } else {
            0
        };
    }
    let scaled = u64::from(completed)
        .saturating_mul(1_000)
        .checked_div(u64::from(total))
        .unwrap_or(0)
        .min(1_000);
    u16::try_from(scaled).unwrap_or(1_000)
}

fn source_free_message(value: &str) -> Result<SourceFreeMessage, ToolExecutionError> {
    SourceFreeMessage::parse(value).map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn freshness_from_label(label: &str) -> Freshness {
    match label {
        "current" => Freshness::Current,
        "superseded" => Freshness::Superseded,
        _ => Freshness::Stale,
    }
}

fn coverage_status_from_label(label: &str) -> rootlight_ir::CoverageStatus {
    match label {
        "complete" => rootlight_ir::CoverageStatus::Complete,
        "bounded" => rootlight_ir::CoverageStatus::Bounded,
        "sampled" => rootlight_ir::CoverageStatus::Sampled,
        _ => rootlight_ir::CoverageStatus::Unknown,
    }
}

fn tier_label(label: &str) -> String {
    match label {
        "tier_a" => "A",
        "tier_b" => "B",
        "tier_d" => "D",
        _ => "C",
    }
    .to_owned()
}

fn analysis_tier(label: &str) -> AnalysisTier {
    match label {
        "tier_a" => AnalysisTier::A,
        "tier_b" => AnalysisTier::B,
        "tier_d" => AnalysisTier::D,
        _ => AnalysisTier::C,
    }
}

async fn execute_repository_index<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: RepoIndexInput = decode_input(arguments)?;
    let request = normalize_repository_index(input, unsupported, invalid_arguments)?;
    let expected_mode = request.mode;
    let future = port.repository_index(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_repository_index(response, expected_mode)?;
    serialize_success(output)
}

async fn execute_operation_status<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: OperationStatusInput = decode_input(arguments)?;
    let request = OperationStatusPortRequest {
        operation: input.operation_id,
        action: match input.action.unwrap_or(OperationAction::Get) {
            OperationAction::Get => RepositoryOperationAction::Get,
            OperationAction::Cancel => RepositoryOperationAction::Cancel,
        },
        wait_ms: input.wait_ms,
        after_revision: input.after_revision,
    };
    let expected_operation = request.operation;
    let future = port.operation_status(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_operation_status(response, expected_operation)?;
    serialize_success(output)
}

async fn execute_code_locate<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    presentation: CursorPresentation,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: CodeLocateInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    if explain_only && input.cursor.is_some() {
        return Err(ToolExecutionError::new(invalid_cursor.clone()));
    }
    let cursor = input.cursor.clone();
    let mut request = normalize_code_locate(input, unsupported)?;
    if let Some(parsed) = parse_repository_cursor(cursor.as_ref(), invalid_cursor)? {
        pin_request_generation(&mut request.generation, &parsed, invalid_cursor)?;
        request.page_offset = decode_page_offset(parsed.last_sort_key(), invalid_cursor)?;
        let context = code_locate_cursor_context(
            &request,
            parsed.generation(),
            presentation.exposure_profile,
            response_profile,
            cursor_key.key_id,
        );
        validate_repository_cursor(&parsed, &context, invalid_cursor, cursor_key)?;
    }
    if explain_only {
        let output = explain_code_locate(port, request, cancellation).await?;
        return serialize_profiled_read_success(
            output,
            response_profile,
            started_at,
            presentation.shaping,
        );
    }
    let expected = request.clone();
    let future = port.code_locate(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let generation = response.result.context.generation;
    let next_cursor = create_page_cursor(
        response.result.next_page_offset,
        code_locate_cursor_context(
            &expected,
            generation,
            presentation.exposure_profile,
            response_profile,
            cursor_key.key_id,
        ),
        cursor_key,
    )?;
    let output = map_code_locate(response, &expected, next_cursor)?;
    serialize_profiled_read_success(output, response_profile, started_at, presentation.shaping)
}

/// Builds the source-free `code.locate` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no source bodies
/// are fetched and no locate traversal runs, so explain is safe before work is
/// spent. The plan is deterministic for the normalized request.
async fn explain_code_locate<P>(
    port: Arc<P>,
    request: CodeLocatePortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<CodeLocateData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let mode = match request.mode {
        LocateMode::Exact => SearchMode::Exact,
        LocateMode::Text => SearchMode::Lexical,
        LocateMode::Prefix | LocateMode::SafeRegex | LocateMode::Glob => {
            return Err(internal(ToolExecutionFailure::InvalidResponse));
        }
    };
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::code_locate_plan(
            matches!(request.mode, LocateMode::Exact),
            request.maximum_results,
        ),
        &status.active_generation.to_string(),
    );
    let data = CodeLocateData {
        matches: Vec::new(),
        query_interpretation: QueryInterpretation {
            tokens: Vec::new(),
            modes: BTreeSet::from([mode]),
            semantic_available: false,
        },
        suggested_next: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

/// Builds a source-free explain envelope from repository metadata.
///
/// Shared by the read tools' explain mode: the envelope identity is pinned to
/// the resolved generation and no source is read.
fn explain_envelope_from_status<T>(
    status: client::RepositoryStatus,
    data: T,
) -> Result<ReadEnvelope<T>, ToolExecutionError> {
    let languages: Vec<LanguageCoverage> = status
        .coverage
        .iter()
        .map(|entry| LanguageCoverage {
            language: entry.language.clone(),
            tier: analysis_tier(&entry.tier),
            status: coverage_status_from_label(&entry.status),
        })
        .collect();
    let context = client::QueryContext {
        repository: status.repository_id,
        generation: status.active_generation,
        parent_generation: status.parent_generation,
        active_generation: true,
        tier: client::AnalysisTier::TierC,
        coverage_status: client::CoverageStatus::Bounded,
        skipped_inputs: 0,
        usage: client::QueryUsage {
            rows: 0,
            edges: 0,
            results: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            elapsed_micros: 0,
        },
    };
    let metadata = ReadResponseMetadata::new(
        status.repository_id.to_string(),
        freshness_from_label(&status.structural_freshness),
        freshness_from_label(&status.semantic_freshness),
        languages,
        CacheStatus::NotApplicable,
        "explain".to_owned(),
        Vec::new(),
    );
    map_read_envelope(context, metadata, data, complete_client_result(), None)
}

fn agent_identity_from_status(status: client::RepositoryStatus) -> AgentResolvedIdentity {
    let coverage = status
        .coverage
        .iter()
        .map(|entry| LanguageCoverage {
            language: entry.language.clone(),
            tier: analysis_tier(&entry.tier),
            status: coverage_status_from_label(&entry.status),
        })
        .collect();
    AgentResolvedIdentity {
        repository: ResolvedRepository {
            repository_id: status.repository_id,
            display_name: status.display_name,
        },
        generation: GenerationSummary {
            generation_id: status.resolved_generation,
            parent_generation: RequiredNullable(status.parent_generation),
            structural_freshness: freshness_from_label(&status.structural_freshness),
            semantic_freshness: freshness_from_label(&status.semantic_freshness),
        },
        coverage: CoverageSummary {
            status: rootlight_ir::CoverageStatus::Bounded,
            languages: coverage,
            skipped_inputs: 0,
        },
        warnings: Vec::new(),
    }
}

/// Builds the source-free `symbol.explain` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no symbol evidence
/// is fetched. The plan is deterministic for the normalized request.
async fn explain_symbol_explain<P>(
    port: Arc<P>,
    request: SymbolExplainPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<SymbolExplainData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::symbol_explain_plan(request.symbols.len()),
        &status.active_generation.to_string(),
    );
    let data = SymbolExplainData {
        symbols: Vec::new(),
        unresolved_ids: Vec::new(),
        detail_handles: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

/// Builds the source-free `source.read` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no source bytes
/// are read. The plan is deterministic for the normalized request.
async fn explain_source_read<P>(
    port: Arc<P>,
    request: SourceReadPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<SourceReadData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::source_read_plan(request.references.len()),
        &status.active_generation.to_string(),
    );
    let data = SourceReadData {
        chunks: Vec::new(),
        stale_references: Vec::new(),
        elisions: Vec::new(),
        total_source_bytes: 0,
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

/// Builds the source-free `symbol.relationships` plan without executing
/// retrieval.
///
/// Only repository metadata is read (to pin the generation); no neighborhood
/// expansion runs. The plan is deterministic for the normalized request.
async fn explain_symbol_relationships<P>(
    port: Arc<P>,
    request: SymbolRelationshipsPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<SymbolRelationshipsData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::symbol_relationships_plan(
            request.seeds.len(),
            request.max_results.map(u32::from),
        ),
        &status.active_generation.to_string(),
    );
    let data = SymbolRelationshipsData {
        groups: Vec::new(),
        unresolved: Vec::new(),
        totals: RelationshipTotals {
            returned_edges: 0,
            total_edges: 0,
            exact: true,
        },
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_symbol_explain<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: SymbolExplainInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    let request = normalize_symbol_explain(input, unsupported)?;
    if explain_only {
        let output = explain_symbol_explain(port, request, cancellation).await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.symbol_explain(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_symbol_explain(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

async fn execute_symbol_relationships<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    presentation: CursorPresentation,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: SymbolRelationshipsInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    if explain_only && input.cursor.is_some() {
        return Err(ToolExecutionError::new(invalid_cursor.clone()));
    }
    let cursor = input.cursor.clone();
    let mut request = normalize_symbol_relationships(input, unsupported)?;
    if let Some(parsed) = parse_repository_cursor(cursor.as_ref(), invalid_cursor)? {
        pin_request_generation(&mut request.generation, &parsed, invalid_cursor)?;
        request.page_offset = decode_page_offset(parsed.last_sort_key(), invalid_cursor)?;
        let context = symbol_relationships_cursor_context(
            &request,
            parsed.generation(),
            presentation.exposure_profile,
            response_profile,
            cursor_key.key_id,
        );
        validate_repository_cursor(&parsed, &context, invalid_cursor, cursor_key)?;
    }
    if explain_only {
        let output = explain_symbol_relationships(port, request, cancellation).await?;
        return serialize_profiled_read_success(
            output,
            response_profile,
            started_at,
            presentation.shaping,
        );
    }
    let expected = request.clone();
    let future = port.symbol_relationships(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let generation = response.result.context.generation;
    let next_cursor = create_page_cursor(
        response.result.next_page_offset,
        symbol_relationships_cursor_context(
            &expected,
            generation,
            presentation.exposure_profile,
            response_profile,
            cursor_key.key_id,
        ),
        cursor_key,
    )?;
    let output = map_symbol_relationships(response, &expected, next_cursor)?;
    serialize_profiled_read_success(output, response_profile, started_at, presentation.shaping)
}

fn normalize_symbol_relationships(
    input: SymbolRelationshipsInput,
    unsupported: &PublicError,
) -> Result<SymbolRelationshipsPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, ambiguous candidates, and custom budgets are not
    // served by this slice.
    if input.scope.is_some() || input.include_candidates == Some(true) || input.budget.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let mut relations = Vec::new();
    relations
        .try_reserve_exact(input.relations.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for kind in &input.relations {
        relations.push(relation_kind_label(*kind)?);
    }
    let direction = match input.direction {
        Some(direction) => Some(direction_label(direction)?),
        None => None,
    };
    Ok(SymbolRelationshipsPortRequest {
        repository,
        generation: client_generation(input.generation),
        seeds: input.symbol_ids.into_iter().collect(),
        relations,
        direction,
        min_confidence: input.min_confidence,
        max_results: input.max_results,
        page_offset: 0,
    })
}

fn map_symbol_relationships(
    response: SymbolRelationshipsPortResponse,
    request: &SymbolRelationshipsPortRequest,
    next_cursor: Option<ContinuationCursor>,
) -> Result<ReadEnvelope<SymbolRelationshipsData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let next_page_offset = response.result.next_page_offset;
    let mut groups = Vec::new();
    groups
        .try_reserve_exact(response.result.groups.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for group in response.result.groups {
        let relation = relation_kind_from_label(&group.relation)?;
        let direction = direction_from_label(&group.direction)?;
        let mut items = Vec::new();
        items
            .try_reserve_exact(group.items.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for item in group.items {
            let mut source_refs = Vec::new();
            source_refs
                .try_reserve_exact(item.source_refs.len())
                .map_err(|_| internal(ToolExecutionFailure::Executor))?;
            for source in &item.source_refs {
                source_refs.push(client_source_ref(source)?);
            }
            items.push(RelationshipTarget {
                symbol_id: item.symbol,
                confidence: item.confidence,
                source_refs,
                provenance: Vec::new(),
                trust: TrustClassification::UntrustedRepositoryData,
            });
        }
        let total_count = u32::try_from(group.total_count)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
        groups.push(RelationshipGroup {
            seed: group.seed,
            relation,
            direction,
            items,
            total_count,
        });
    }
    let mapped_returned_edges = groups.iter().try_fold(0_u64, |total, group| {
        total
            .checked_add(
                u64::try_from(group.items.len())
                    .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
            )
            .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))
    })?;
    let returned_edges = u32::try_from(response.result.returned_edges)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let total_edges = u32::try_from(response.result.total_edges)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let returned_end = request
        .page_offset
        .checked_add(u64::from(returned_edges))
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    if response.result.returned_edges != mapped_returned_edges
        || response.result.returned_edges > response.result.total_edges
        || (!response.result.truncated
            && (!response.result.exact
                || next_page_offset.is_some()
                || returned_end != response.result.total_edges))
        || next_page_offset.is_some_and(|next| {
            !response.result.exact
                || !response.result.truncated
                || next != returned_end
                || next >= response.result.total_edges
        })
        || next_cursor.is_some() != next_page_offset.is_some()
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let data = SymbolRelationshipsData {
        groups,
        unresolved: Vec::new(),
        totals: RelationshipTotals {
            returned_edges,
            total_edges,
            exact: response.result.exact,
        },
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        next_cursor,
    )
}

fn relation_kind_label(kind: RelationKind) -> Result<String, ToolExecutionError> {
    match serde_json::to_value(kind).map_err(|_| internal(ToolExecutionFailure::InvalidResponse))? {
        Value::String(label) => Ok(label),
        _ => Err(internal(ToolExecutionFailure::InvalidResponse)),
    }
}

fn relation_kind_from_label(label: &str) -> Result<RelationKind, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn direction_label(direction: Direction) -> Result<String, ToolExecutionError> {
    match serde_json::to_value(direction)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?
    {
        Value::String(label) => Ok(label),
        _ => Err(internal(ToolExecutionFailure::InvalidResponse)),
    }
}

fn direction_from_label(label: &str) -> Result<Direction, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

/// Builds the source-free `flow.trace` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no traversal runs.
/// The plan is deterministic for the normalized request.
async fn explain_flow_trace<P>(
    port: Arc<P>,
    request: FlowTracePortRequest,
    relations: BTreeSet<RelationKind>,
    min_confidence: Option<u16>,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<FlowTraceData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::flow_trace_plan(request.max_depth, request.max_paths),
        &status.active_generation.to_string(),
    );
    let data = FlowTraceData {
        paths: Vec::new(),
        frontier: FrontierSummary {
            reached_nodes: 0,
            examined_edges: 0,
            truncated: false,
            unresolved_boundaries: 0,
        },
        projection: RelationProjection {
            relations,
            min_confidence: min_confidence.unwrap_or(0),
        },
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_flow_trace<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: FlowTraceInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    let trace_relations = input.relations.clone();
    let trace_min_confidence = input.min_confidence;
    let request = normalize_flow_trace(input, unsupported)?;
    if explain_only {
        let output = explain_flow_trace(
            port,
            request,
            trace_relations,
            trace_min_confidence,
            cancellation,
        )
        .await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.flow_trace(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_flow_trace(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

fn normalize_flow_trace(
    input: FlowTraceInput,
    unsupported: &PublicError,
) -> Result<FlowTracePortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Cross-repository traversal, explicit path policies, and custom budgets
    // are not served by this slice.
    if input.cross_repository == Some(true) || input.path_policy.is_some() || input.budget.is_some()
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // The first slice resolves only stable symbol endpoints; route, service,
    // and database selectors have no oracle data yet.
    let from = input
        .from
        .symbol_id
        .ok_or_else(|| ToolExecutionError::new(unsupported.clone()))?;
    let to = match input.to {
        Some(selector) => Some(
            selector
                .symbol_id
                .ok_or_else(|| ToolExecutionError::new(unsupported.clone()))?,
        ),
        None => None,
    };
    let mut relations = Vec::new();
    relations
        .try_reserve_exact(input.relations.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for kind in &input.relations {
        relations.push(relation_kind_label(*kind)?);
    }
    let direction = match input.direction {
        Some(direction) => Some(direction_label(direction)?),
        None => None,
    };
    Ok(FlowTracePortRequest {
        repository,
        generation: client_generation(input.generation),
        from,
        to,
        relations,
        direction,
        max_depth: input.max_depth,
        max_paths: input.max_paths,
        min_confidence: input.min_confidence,
    })
}

fn map_flow_trace(
    response: FlowTracePortResponse,
    request: &FlowTracePortRequest,
) -> Result<ReadEnvelope<FlowTraceData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let frontier = response.result.frontier;
    let projection = response.result.projection;
    let mut paths = Vec::new();
    paths
        .try_reserve_exact(response.result.paths.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for path in response.result.paths {
        let mut edges = Vec::new();
        edges
            .try_reserve_exact(path.edges.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for edge in path.edges {
            let kind = relation_kind_from_label(&edge.kind)?;
            let mut source_refs = Vec::new();
            source_refs
                .try_reserve_exact(edge.source_refs.len())
                .map_err(|_| internal(ToolExecutionFailure::Executor))?;
            for source in &edge.source_refs {
                source_refs.push(client_source_ref(source)?);
            }
            edges.push(TraceEdge {
                kind,
                confidence: edge.confidence,
                source_refs,
                trust: TrustClassification::UntrustedRepositoryData,
            });
        }
        paths.push(TracePath {
            confidence: path.confidence,
            nodes: path.nodes,
            edges,
            cyclic: path.cyclic,
        });
    }
    let mut relations = BTreeSet::new();
    for relation in &projection.relations {
        relations.insert(relation_kind_from_label(relation)?);
    }
    let data = FlowTraceData {
        paths,
        frontier: FrontierSummary {
            reached_nodes: frontier.reached_nodes,
            examined_edges: frontier.examined_edges,
            truncated: frontier.truncated,
            unresolved_boundaries: frontier.unresolved_boundaries,
        },
        projection: RelationProjection {
            relations,
            min_confidence: projection.min_confidence,
        },
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

/// Builds the source-free `architecture.cycles` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no cycle detection
/// runs. The plan is deterministic for the normalized request.
async fn explain_architecture_cycles<P>(
    port: Arc<P>,
    request: ArchitectureCyclesPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<ArchitectureCyclesData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::architecture_cycles_plan(request.max_cycles),
        &status.active_generation.to_string(),
    );
    let data = ArchitectureCyclesData {
        components: Vec::new(),
        cycles: Vec::new(),
        break_candidates: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_architecture_cycles<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: ArchitectureCyclesInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    let request = normalize_architecture_cycles(input, unsupported)?;
    if explain_only {
        let output = explain_architecture_cycles(port, request, cancellation).await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.architecture_cycles(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_architecture_cycles(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

fn normalize_architecture_cycles(
    input: ArchitectureCyclesInput,
    unsupported: &PublicError,
) -> Result<ArchitectureCyclesPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, ranking strategies, and custom budgets are not served
    // by this slice. The projection level is accepted as a descriptive label;
    // detection runs at symbol granularity.
    if input.scope.is_some() || input.rank_by.is_some() || input.budget.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let mut relations = Vec::new();
    relations
        .try_reserve_exact(input.projection.relations.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for kind in &input.projection.relations {
        relations.push(relation_kind_label(*kind)?);
    }
    Ok(ArchitectureCyclesPortRequest {
        repository,
        generation: client_generation(input.generation),
        relations,
        min_size: input.min_size,
        max_cycles: input.max_cycles,
        include_self_cycles: input.include_self_cycles,
    })
}

fn map_architecture_cycles(
    response: ArchitectureCyclesPortResponse,
    request: &ArchitectureCyclesPortRequest,
) -> Result<ReadEnvelope<ArchitectureCyclesData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let mut components = Vec::new();
    components
        .try_reserve_exact(response.result.components.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for component in response.result.components {
        let mut members = Vec::new();
        members
            .try_reserve_exact(component.members.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for member in component.members {
            members.push(member.to_string());
        }
        components.push(StronglyConnectedComponent {
            size: component.size,
            members,
            internal_edges: component.internal_edges,
        });
    }
    let mut cycles = Vec::new();
    cycles
        .try_reserve_exact(response.result.cycles.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for cycle in response.result.cycles {
        let mut nodes = Vec::new();
        nodes
            .try_reserve_exact(cycle.nodes.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for node in cycle.nodes {
            nodes.push(node.to_string());
        }
        let mut edge_evidence = Vec::new();
        edge_evidence
            .try_reserve_exact(cycle.edge_evidence.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for source in &cycle.edge_evidence {
            edge_evidence.push(client_source_ref(source)?);
        }
        cycles.push(MinimalCycle {
            nodes,
            edge_evidence,
            confidence: cycle.confidence,
        });
    }
    let mut break_candidates = Vec::new();
    break_candidates
        .try_reserve_exact(response.result.break_candidates.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for candidate in response.result.break_candidates {
        let kind = relation_kind_from_label(&candidate.kind)?;
        let mut source_refs = Vec::new();
        source_refs
            .try_reserve_exact(candidate.source_refs.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for source in &candidate.source_refs {
            source_refs.push(client_source_ref(source)?);
        }
        break_candidates.push(CycleBreakCandidate {
            from: candidate.from.to_string(),
            to: candidate.to.to_string(),
            kind,
            break_cost: candidate.break_cost,
            source_refs,
        });
    }
    let data = ArchitectureCyclesData {
        components,
        cycles,
        break_candidates,
        explanation: None,
    };
    // The requested cycle cap is an explicit bound honored by the daemon; this
    // slice does not surface separate budget-truncation through the wire.
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

/// Builds the source-free `code.dead` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no reachability
/// analysis runs. The plan is deterministic for the normalized request.
async fn explain_code_dead<P>(
    port: Arc<P>,
    request: CodeDeadPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<CodeDeadData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::code_dead_plan(request.max_candidates),
        &status.active_generation.to_string(),
    );
    let data = CodeDeadData {
        candidates: Vec::new(),
        entry_points: EntryPointSummary {
            policy: EntryPointPolicy::Standard,
            entry_point_count: 0,
            complete: false,
        },
        blind_spots: Vec::new(),
        false_positive_controls: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_code_dead<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: CodeDeadInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    let request = normalize_code_dead(input, unsupported)?;
    if explain_only {
        let output = explain_code_dead(port, request, cancellation).await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.code_dead(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_code_dead(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

fn normalize_code_dead(
    input: CodeDeadInput,
    unsupported: &PublicError,
) -> Result<CodeDeadPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope and custom budgets are not served by this slice.
    if input.scope.is_some() || input.budget.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let entry_point_policy = match input.entry_point_policy {
        Some(policy) => Some(entry_point_policy_label(policy)?),
        None => None,
    };
    Ok(CodeDeadPortRequest {
        repository,
        generation: client_generation(input.generation),
        entry_point_policy,
        include_exported: input.include_exported,
        include_tests: input.include_tests,
        min_confidence: input.min_confidence,
        max_candidates: input.max_candidates,
    })
}

fn map_code_dead(
    response: CodeDeadPortResponse,
    request: &CodeDeadPortRequest,
) -> Result<ReadEnvelope<CodeDeadData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(response.result.candidates.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for candidate in response.result.candidates {
        let classification = dead_classification_from_label(&candidate.classification)?;
        let mut source_refs = Vec::new();
        source_refs
            .try_reserve_exact(candidate.source_refs.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for source in &candidate.source_refs {
            source_refs.push(client_source_ref(source)?);
        }
        candidates.push(DeadCandidate {
            symbol_id: candidate.symbol_id,
            classification,
            confidence: candidate.confidence,
            why: candidate.why,
            suppressions_checked: candidate.suppressions_checked,
            source_refs,
            trust: TrustClassification::UntrustedRepositoryData,
        });
    }
    let entry_points = response.result.entry_points;
    let policy = entry_point_policy_from_label(&entry_points.policy)?;
    let mut blind_spots = Vec::new();
    blind_spots
        .try_reserve_exact(response.result.blind_spots.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for spot in response.result.blind_spots {
        blind_spots.push(BlindSpot {
            category: spot.category,
            affected_count: spot.affected_count,
        });
    }
    let mut false_positive_controls = Vec::new();
    false_positive_controls
        .try_reserve_exact(response.result.false_positive_controls.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for rule in response.result.false_positive_controls {
        false_positive_controls.push(RuleSummary {
            rule: rule.rule,
            suppressed_count: rule.suppressed_count,
        });
    }
    let data = CodeDeadData {
        candidates,
        entry_points: EntryPointSummary {
            policy,
            entry_point_count: entry_points.entry_point_count,
            complete: entry_points.complete,
        },
        blind_spots,
        false_positive_controls,
        explanation: None,
    };
    // The requested candidate cap is an explicit bound honored by the daemon;
    // this slice does not surface separate budget-truncation through the wire.
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn entry_point_policy_label(policy: EntryPointPolicy) -> Result<String, ToolExecutionError> {
    match serde_json::to_value(policy)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?
    {
        Value::String(label) => Ok(label),
        _ => Err(internal(ToolExecutionFailure::InvalidResponse)),
    }
}

fn entry_point_policy_from_label(label: &str) -> Result<EntryPointPolicy, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn dead_classification_from_label(label: &str) -> Result<DeadClassification, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

/// Builds the source-free `architecture.overview` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no component
/// aggregation runs. The plan is deterministic for the normalized request.
async fn explain_architecture_overview<P>(
    port: Arc<P>,
    request: ArchitectureOverviewPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<ArchitectureOverviewData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::architecture_overview_plan(request.max_components),
        &status.active_generation.to_string(),
    );
    let data = ArchitectureOverviewData {
        components: Vec::new(),
        connections: Vec::new(),
        hotspots: Vec::new(),
        views: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_architecture_overview<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: ArchitectureOverviewInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.response_profile.unwrap_or(ResponseProfile::Compact);
    let request = normalize_architecture_overview(input, unsupported)?;
    if explain_only {
        let output = explain_architecture_overview(port, request, cancellation).await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.architecture_overview(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_architecture_overview(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

fn normalize_architecture_overview(
    input: ArchitectureOverviewInput,
    unsupported: &PublicError,
) -> Result<ArchitectureOverviewPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, explicit detail levels, and custom budgets are not
    // served by this slice. The base file-granularity model is always returned;
    // only the hotspot derived view is honored.
    if input.scope.is_some() || input.detail.is_some() || input.budget.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let mut views = Vec::new();
    if let Some(requested) = input.views {
        for view in requested {
            if view != ArchitectureView::Hotspots {
                return Err(ToolExecutionError::new(unsupported.clone()));
            }
            views.push(architecture_view_label(view)?);
        }
    }
    Ok(ArchitectureOverviewPortRequest {
        repository,
        generation: client_generation(input.generation),
        views,
        max_components: input.max_components,
        include_edges: input.include_edges,
        min_confidence: input.min_confidence,
    })
}

fn map_architecture_overview(
    response: ArchitectureOverviewPortResponse,
    request: &ArchitectureOverviewPortRequest,
) -> Result<ReadEnvelope<ArchitectureOverviewData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let mut components = Vec::new();
    components
        .try_reserve_exact(response.result.components.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for component in response.result.components {
        components.push(ArchitectureComponent {
            id: component.id,
            kind: component.kind,
            name: component.name,
            symbol_count: component.symbol_count,
            responsibility_evidence: component.responsibility_evidence,
            confidence: component.confidence,
            trust: TrustClassification::UntrustedRepositoryData,
        });
    }
    let mut connections = Vec::new();
    connections
        .try_reserve_exact(response.result.connections.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for connection in response.result.connections {
        let kind = relation_kind_from_label(&connection.kind)?;
        connections.push(ArchitectureConnection {
            from: connection.from,
            to: connection.to,
            kind,
            weight: connection.weight,
            confidence: connection.confidence,
        });
    }
    let mut hotspots = Vec::new();
    hotspots
        .try_reserve_exact(response.result.hotspots.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for hotspot in response.result.hotspots {
        hotspots.push(Hotspot {
            component_id: hotspot.component_id,
            fan_in: hotspot.fan_in,
            fan_out: hotspot.fan_out,
            change_frequency: hotspot.change_frequency,
            complexity: hotspot.complexity,
            score: hotspot.score,
        });
    }
    let mut views = Vec::new();
    views
        .try_reserve_exact(response.result.views.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for view in response.result.views {
        let category = architecture_view_from_label(&view.view)?;
        views.push(DerivedViewInfo {
            view: category,
            algorithm_version: view.algorithm_version,
        });
    }
    let data = ArchitectureOverviewData {
        components,
        connections,
        hotspots,
        views,
        explanation: None,
    };
    // The requested component cap is an explicit bound honored by the daemon;
    // this slice does not surface separate budget-truncation through the wire.
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn architecture_view_label(view: ArchitectureView) -> Result<String, ToolExecutionError> {
    match serde_json::to_value(view).map_err(|_| internal(ToolExecutionFailure::InvalidResponse))? {
        Value::String(label) => Ok(label),
        _ => Err(internal(ToolExecutionFailure::InvalidResponse)),
    }
}

fn architecture_view_from_label(label: &str) -> Result<ArchitectureView, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

/// Builds the source-free `tests.select` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no test selection
/// runs. The plan is deterministic for the normalized request.
async fn explain_tests_select<P>(
    port: Arc<P>,
    request: TestsSelectPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<TestsSelectData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::tests_select_plan(request.max_tests),
        &status.active_generation.to_string(),
    );
    let data = TestsSelectData {
        tests: Vec::new(),
        coverage_strategy: TestCoverageStrategy {
            direct_edges: false,
            transitive_signals: false,
            history_signals: false,
            build_target_signals: false,
        },
        gaps: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_tests_select<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: TestsSelectInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.profile.unwrap_or(ResponseProfile::Compact);
    let request = normalize_tests_select(input, unsupported)?;
    if explain_only {
        let output = explain_tests_select(port, request, cancellation).await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.tests_select(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_tests_select(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

fn normalize_tests_select(
    input: TestsSelectInput,
    unsupported: &PublicError,
) -> Result<TestsSelectPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Custom budgets, execution budgets, and framework filters are not served
    // by this slice.
    if input.budget.is_some() || input.execution_budget.is_some() || input.frameworks.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // Only explicit symbol seeds are served; path, change, and build-target
    // seeds require capabilities this slice does not provide.
    if input.seeds.paths.is_some()
        || input.seeds.change.is_some()
        || input.seeds.build_targets.is_some()
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let seeds = match input.seeds.symbols {
        Some(symbols) if !symbols.is_empty() => symbols,
        _ => return Err(ToolExecutionError::new(unsupported.clone())),
    };
    let mut test_kinds = Vec::new();
    if let Some(requested) = input.test_kinds {
        for kind in requested {
            test_kinds.push(test_kind_label(kind)?);
        }
    }
    Ok(TestsSelectPortRequest {
        repository,
        generation: client_generation(input.generation),
        seeds,
        test_kinds,
        max_tests: input.max_tests,
        include_commands: input.include_commands,
    })
}

fn map_tests_select(
    response: TestsSelectPortResponse,
    request: &TestsSelectPortRequest,
) -> Result<ReadEnvelope<TestsSelectData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let mut tests = Vec::new();
    tests
        .try_reserve_exact(response.result.tests.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for test in response.result.tests {
        let kind = test_kind_from_label(&test.kind)?;
        tests.push(RankedTest {
            test_id: test.test_id,
            kind,
            path: test.path,
            score: test.score,
            why: test.why,
            estimated_cost_ms: test.estimated_cost_ms,
            command_hint: test.command_hint,
        });
    }
    let strategy = response.result.coverage_strategy;
    let mut gaps = Vec::new();
    gaps.try_reserve_exact(response.result.gaps.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for gap in response.result.gaps {
        let reason = SafeLabel::parse(&gap.reason)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
        gaps.push(TestGap {
            scope: gap.scope,
            reason,
        });
    }
    let data = TestsSelectData {
        tests,
        coverage_strategy: TestCoverageStrategy {
            direct_edges: strategy.direct_edges,
            transitive_signals: strategy.transitive_signals,
            history_signals: strategy.history_signals,
            build_target_signals: strategy.build_target_signals,
        },
        gaps,
        explanation: None,
    };
    // The requested test cap is an explicit bound honored by the daemon; this
    // slice does not surface separate budget-truncation through the wire.
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn test_kind_label(kind: TestKind) -> Result<String, ToolExecutionError> {
    match serde_json::to_value(kind).map_err(|_| internal(ToolExecutionFailure::InvalidResponse))? {
        Value::String(label) => Ok(label),
        _ => Err(internal(ToolExecutionFailure::InvalidResponse)),
    }
}

fn test_kind_from_label(label: &str) -> Result<TestKind, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

/// Builds the source-free `change.impact` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no impact analysis
/// runs. The plan is deterministic for the normalized request.
async fn explain_change_impact<P>(
    port: Arc<P>,
    request: ChangeImpactPortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<ChangeImpactData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(request.repository, request.generation);
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let changed_count = request
        .changed_symbols
        .len()
        .saturating_add(request.changed_paths.len());
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::change_impact_plan(changed_count),
        &status.active_generation.to_string(),
    );
    let data = ChangeImpactData {
        resolved_changes: Vec::new(),
        impacted: Vec::new(),
        service_impacts: Vec::new(),
        tests: Vec::new(),
        risk_summary: ImpactRiskSummary {
            level: RiskLevel::None,
            reasons: Vec::new(),
            coverage: rootlight_ir::CoverageStatus::Unknown,
            breaking_surface: false,
            fanout: 0,
            dynamic_blind_spots: false,
        },
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_change_impact<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    shaping: ResponseShaping,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: ChangeImpactInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let response_profile = input.profile.unwrap_or(ResponseProfile::Compact);
    let request = normalize_change_impact(input, unsupported)?;
    if explain_only {
        let output = explain_change_impact(port, request, cancellation).await?;
        return serialize_profiled_read_success(output, response_profile, started_at, shaping);
    }
    let expected = request.clone();
    let future = port.change_impact(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_change_impact(response, &expected)?;
    serialize_profiled_read_success(output, response_profile, started_at, shaping)
}

fn normalize_change_impact(
    input: ChangeImpactInput,
    unsupported: &PublicError,
) -> Result<ChangeImpactPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Scope bounding and custom budgets are not served by this slice.
    if input.scope.is_some() || input.budget.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // Working-tree and revision-range changes require a git diff this slice does
    // not compute.
    if input.change.working_tree.is_some() || input.change.revision_range.is_some() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // History signals are not served by this slice.
    if input.include_history == Some(true) {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // The standard and direct-only policies are served; the conservative
    // over-approximation needs relation families this slice cannot provide.
    let mut max_depth = input.max_depth;
    match input.relation_policy {
        None | Some(RelationPolicy::Standard) => {}
        Some(RelationPolicy::DirectOnly) => max_depth = Some(1),
        Some(RelationPolicy::Conservative) => {
            return Err(ToolExecutionError::new(unsupported.clone()));
        }
    }
    let changed_symbols = input.change.symbol_ids.unwrap_or_default();
    let changed_paths = input.change.paths.unwrap_or_default();
    // An empty change set carries no resolvable change.
    if changed_symbols.is_empty() && changed_paths.is_empty() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    Ok(ChangeImpactPortRequest {
        repository,
        generation: client_generation(input.generation),
        changed_symbols,
        changed_paths,
        max_depth,
        min_confidence: input.min_confidence,
        include_tests: input.include_tests,
        // The MCP contract exposes no dependent cap; the daemon applies its
        // bounded default.
        max_dependents: None,
    })
}

fn map_change_impact(
    response: ChangeImpactPortResponse,
    request: &ChangeImpactPortRequest,
) -> Result<ReadEnvelope<ChangeImpactData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let mut resolved_changes = Vec::new();
    resolved_changes
        .try_reserve_exact(response.result.resolved_changes.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for change in response.result.resolved_changes {
        let classification = change_classification_from_label(&change.classification)?;
        let kind = match change.kind {
            Some(label) => Some(ir_entity_kind_from_label(&label)?),
            None => None,
        };
        resolved_changes.push(ResolvedChange {
            symbol_id: RequiredNullable(change.symbol_id),
            file_id: RequiredNullable(change.file_id),
            classification,
            kind,
        });
    }
    let mut impacted = Vec::new();
    impacted
        .try_reserve_exact(response.result.impacted.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for group in response.result.impacted {
        let mut dependents = Vec::new();
        dependents
            .try_reserve_exact(group.dependents.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        for entry in group.dependents {
            dependents.push(ImpactEntry {
                symbol_id: entry.symbol_id,
                kind: ir_entity_kind_from_label(&entry.kind)?,
                distance: entry.distance,
                confidence: entry.confidence,
                via: entry.via,
                is_public: entry.is_public,
            });
        }
        impacted.push(ImpactGroup {
            source_index: group.source_index,
            dependents,
        });
    }
    let mut tests = Vec::new();
    tests
        .try_reserve_exact(response.result.tests.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for test in response.result.tests {
        tests.push(TestCandidate {
            test_id: test.test_id,
            relevance: test.relevance,
            why: test.why,
            estimated_cost_ms: test.estimated_cost_ms,
        });
    }
    let risk = response.result.risk_summary;
    let data = ChangeImpactData {
        resolved_changes,
        impacted,
        // This slice models no service or cross-repository boundary.
        service_impacts: Vec::new(),
        tests,
        risk_summary: ImpactRiskSummary {
            level: risk_level_from_label(&risk.level)?,
            reasons: risk.reasons,
            coverage: coverage_status_from_label(&risk.coverage),
            breaking_surface: risk.breaking_surface,
            fanout: risk.fanout,
            dynamic_blind_spots: risk.dynamic_blind_spots,
        },
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn change_classification_from_label(
    label: &str,
) -> Result<ChangeClassification, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn risk_level_from_label(label: &str) -> Result<RiskLevel, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn ir_entity_kind_from_label(label: &str) -> Result<IrEntityKind, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

#[expect(
    clippy::too_many_arguments,
    reason = "change planning carries checked validators, public errors, and cursor state explicitly"
)]
async fn execute_plan_change<P>(
    port: Arc<P>,
    validator: Arc<MaterializedToolValidator>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let started_at = Instant::now();
    let input: PlanChangeInput = decode_input(arguments)?;
    let response_profile = input.profile.unwrap_or(ResponseProfile::Compact);
    let deadline = started_at
        .checked_add(Duration::from_millis(30_000))
        .ok_or_else(|| internal(ToolExecutionFailure::Executor))?;
    let adapter = Arc::new(McpAgentToolPort {
        port,
        validator,
        unsupported: unsupported.clone(),
        invalid_arguments: invalid_arguments.clone(),
        invalid_cursor: invalid_cursor.clone(),
        exposure_profile,
        cursor_key,
    });
    let output = PlanChangeService
        .execute(adapter, input, cancellation, deadline)
        .await
        .map_err(|error| map_plan_change_service_error(error, unsupported))?;
    serialize_profiled_read_success(
        output,
        response_profile,
        started_at,
        ResponseShaping::Public,
    )
}

fn map_plan_change_service_error(
    error: PlanChangeServiceError,
    unsupported: &PublicError,
) -> ToolExecutionError {
    match error {
        PlanChangeServiceError::Admission(
            PlanChangeError::UnsupportedRepository
            | PlanChangeError::UnsupportedOption
            | PlanChangeError::EmptyTargets,
        ) => ToolExecutionError::new(unsupported.clone()),
        PlanChangeServiceError::Admission(PlanChangeError::InvalidRisk)
        | PlanChangeServiceError::InvalidResponse => {
            internal(ToolExecutionFailure::InvalidResponse)
        }
        PlanChangeServiceError::Public(error) => ToolExecutionError::new(*error),
        PlanChangeServiceError::DeadlineExceeded => {
            ToolExecutionError::new(authoritative_error(MappedDomainFailure::budget_exceeded()))
        }
        PlanChangeServiceError::Cancelled | PlanChangeServiceError::Unavailable => {
            internal(ToolExecutionFailure::Executor)
        }
        _ => internal(ToolExecutionFailure::Executor),
    }
}

fn adapt_plan_change_response(
    response: PlanChangePortResponse,
    request: &PlanChangePortRequest,
) -> Result<PlanChangePortOutput, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository(),
        client_generation_ref(request.generation()),
    )?;
    let mut plan = Vec::new();
    plan.try_reserve_exact(response.result.plan.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for step in response.result.plan {
        plan.push(ChangePlanStep {
            step: step.step,
            action: step.action,
            targets: step.targets,
            depends_on: step.depends_on,
            risks: step.risks,
            verification: step.verification,
        });
    }
    let scope = response.result.affected_scope;
    let mut test_plan = Vec::new();
    test_plan
        .try_reserve_exact(response.result.test_plan.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for test in response.result.test_plan {
        test_plan.push(TestCandidate {
            test_id: test.test_id,
            relevance: test.relevance,
            why: test.why,
            estimated_cost_ms: test.estimated_cost_ms,
        });
    }
    let mut open_decisions = Vec::new();
    open_decisions
        .try_reserve_exact(response.result.open_decisions.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for decision in response.result.open_decisions {
        open_decisions.push(PlanDecision {
            question: decision.question,
            recommended_default: decision.recommended_default,
        });
    }
    let pack = response.result.context_pack_request;
    let result = PlanChangeResult {
        plan,
        affected_scope: PlanImpactResult {
            affected_symbols: scope.affected_symbols,
            affected_files: scope.affected_files,
            risk_level: scope.risk_level,
            touches_public_surface: scope.touches_public_surface,
        },
        test_plan,
        open_decisions,
        context_pack_request: ContextPackRequest {
            symbols: pack.symbols,
            files: pack.files,
        },
    };
    let metadata = map_read_envelope(
        response.result.context,
        response.metadata,
        (),
        response.result.execution_completeness,
        None,
    )?;
    Ok(PlanChangePortOutput {
        identity: AgentResolvedIdentity {
            repository: metadata.repository,
            generation: metadata.generation,
            coverage: metadata.coverage,
            warnings: Vec::new(),
        },
        result,
        usage: metadata.usage,
        truncated: metadata.truncated,
        completeness: metadata.completeness,
        warnings: metadata.warnings,
    })
}

/// Builds the source-free `history.compare` plan without executing retrieval.
///
/// Only repository metadata is read (to pin the generation); no revision
/// comparison runs. The plan is deterministic for the normalized request.
async fn explain_history_compare<P>(
    port: Arc<P>,
    request: HistoryComparePortRequest,
    cancellation: RequestCancellation,
) -> Result<ReadEnvelope<HistoryCompareData>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let status_request = RepositoryStatusPortRequest::new(
        request.repository,
        client::GenerationSelector::Generation(request.head),
    );
    let status = await_port(
        port.repository_status(status_request, cancellation.clone()),
        cancellation,
    )
    .await?;
    let explanation = rootlight_agent::explain::finalize_plan(
        rootlight_agent::explain::history_compare_plan(request.max_results),
        &status.active_generation.to_string(),
    );
    let data = HistoryCompareData {
        matched_states: MatchedStates {
            base_generation: request.base,
            head_generation: request.head,
            coverage: rootlight_ir::CoverageStatus::Unknown,
        },
        changes: Vec::new(),
        architecture_delta: ArchitectureDelta {
            new_cross_service_edges: 0,
            removed_cross_service_edges: 0,
            new_boundaries: 0,
            removed_boundaries: 0,
        },
        breaking_candidates: Vec::new(),
        lineage: Vec::new(),
        explanation: Some(explanation),
    };
    explain_envelope_from_status(status, data)
}

async fn execute_history_compare<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: HistoryCompareInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let request = normalize_history_compare(input, unsupported)?;
    if explain_only {
        let output = explain_history_compare(port, request, cancellation).await?;
        return serialize_success(output);
    }
    let expected = request.clone();
    let future = port.history_compare(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_history_compare(response, &expected)?;
    serialize_success(output)
}

fn normalize_history_compare(
    input: HistoryCompareInput,
    unsupported: &PublicError,
) -> Result<HistoryComparePortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Scope bounding, unchanged-context inclusion, custom budgets, and expanded
    // profiles are not served by this slice.
    if input.scope.is_some()
        || input.include_unchanged_context == Some(true)
        || input.budget.is_some()
        || !is_compact_profile(input.profile)
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // Git revision selectors require a git-ref to generation mapping this slice
    // does not maintain.
    let RevisionSelector::Generation(base) = input.base else {
        return Err(ToolExecutionError::new(unsupported.clone()));
    };
    let RevisionSelector::Generation(head) = input.head else {
        return Err(ToolExecutionError::new(unsupported.clone()));
    };
    let change_kinds = input
        .change_kinds
        .unwrap_or_default()
        .iter()
        .map(|kind| compare_change_kind_label(*kind).to_owned())
        .collect();
    Ok(HistoryComparePortRequest {
        repository,
        base,
        head,
        change_kinds,
        max_results: input.max_results,
    })
}

/// Returns the stable wire label for one typed compare change kind.
const fn compare_change_kind_label(kind: CompareChangeKind) -> &'static str {
    match kind {
        CompareChangeKind::Entities => "entities",
        CompareChangeKind::Signatures => "signatures",
        CompareChangeKind::Relations => "relations",
        CompareChangeKind::Architecture => "architecture",
        CompareChangeKind::Ownership => "ownership",
        CompareChangeKind::Tests => "tests",
        CompareChangeKind::Routes => "routes",
        CompareChangeKind::Data => "data",
    }
}

fn map_history_compare(
    response: HistoryComparePortResponse,
    request: &HistoryComparePortRequest,
) -> Result<ReadEnvelope<HistoryCompareData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        client::GenerationSelector::Generation(request.head),
    )?;
    let states = response.result.matched_states;
    let delta = response.result.architecture_delta;
    let mut changes = Vec::new();
    changes
        .try_reserve_exact(response.result.changes.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for change in response.result.changes {
        changes.push(SemanticChange {
            kind: semantic_change_kind_from_label(&change.kind)?,
            symbol_id: change.symbol_id,
            entity_kind: ir_entity_kind_from_label(&change.entity_kind)?,
            breaking_candidate: change.breaking_candidate,
            significance: change.significance,
        });
    }
    let mut breaking_candidates = Vec::new();
    breaking_candidates
        .try_reserve_exact(response.result.breaking_candidates.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for candidate in response.result.breaking_candidates {
        breaking_candidates.push(BreakingCandidate {
            symbol_id: candidate.symbol_id,
            consumer_count: candidate.consumer_count,
            is_public_surface: candidate.is_public_surface,
            reason: SafeLabel::parse(&candidate.reason)
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
        });
    }
    let mut lineage = Vec::new();
    lineage
        .try_reserve_exact(response.result.lineage.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for lineage_match in response.result.lineage {
        lineage.push(LineageMatch {
            base_symbol_id: lineage_match.base_symbol_id,
            head_symbol_id: lineage_match.head_symbol_id,
            confidence: lineage_match.confidence,
            is_rename: lineage_match.is_rename,
        });
    }
    let data = HistoryCompareData {
        matched_states: MatchedStates {
            base_generation: states.base_generation,
            head_generation: states.head_generation,
            coverage: coverage_status_from_label(&states.coverage),
        },
        changes,
        architecture_delta: ArchitectureDelta {
            new_cross_service_edges: delta.new_cross_service_edges,
            removed_cross_service_edges: delta.removed_cross_service_edges,
            new_boundaries: delta.new_boundaries,
            removed_boundaries: delta.removed_boundaries,
        },
        breaking_candidates,
        lineage,
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn semantic_change_kind_from_label(label: &str) -> Result<SemanticChangeKind, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

async fn execute_query_advanced<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    exposure_profile: ExposureProfile,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_cursor: &PublicError,
    cursor_key: CursorSigningKey,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: QueryAdvancedInput = decode_input(arguments)?;
    if input.explain == Some(true) && input.cursor.is_some() {
        return Err(ToolExecutionError::new(invalid_cursor.clone()));
    }
    let cursor = input.cursor.clone();
    let mut request = normalize_query_advanced(input, unsupported)?;
    if let Some(parsed) = parse_repository_cursor(cursor.as_ref(), invalid_cursor)? {
        pin_request_generation(&mut request.generation, &parsed, invalid_cursor)?;
        request.page_offset = decode_page_offset(parsed.last_sort_key(), invalid_cursor)?;
        let context = query_advanced_cursor_context(
            &request,
            parsed.generation(),
            exposure_profile,
            cursor_key.key_id,
        );
        validate_repository_cursor(&parsed, &context, invalid_cursor, cursor_key)?;
    }
    let expected = request.clone();
    let future = port.query_advanced(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let generation = response.result.context.generation;
    let next_cursor = create_page_cursor(
        response.result.next_page_offset,
        query_advanced_cursor_context(&expected, generation, exposure_profile, cursor_key.key_id),
        cursor_key,
    )?;
    let output = map_query_advanced(response, &expected, next_cursor)?;
    serialize_success(output)
}

fn normalize_query_advanced(
    input: QueryAdvancedInput,
    unsupported: &PublicError,
) -> Result<QueryAdvancedPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Bound parameters are not served by this slice.
    if input
        .parameters
        .as_ref()
        .is_some_and(|parameters| !parameters.is_empty())
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let max_rows = usize::from(input.max_results.unwrap_or(DEFAULT_ADVANCED_RESULTS));
    let plan = AdvancedQueryPlan::from_ast(&input.query, max_rows, MAX_ADVANCED_TRAVERSAL, None)
        .map_err(advanced_query_error)?;
    if input
        .cost_limit
        .is_some_and(|limit| plan.estimated_cost > limit)
    {
        return Err(cost_limit_error(plan.estimated_cost, input.cost_limit));
    }
    let query_ast = serde_json::to_string(&input.query)
        .map_err(|_| ToolExecutionError::new(unsupported.clone()))?;
    Ok(QueryAdvancedPortRequest {
        repository,
        generation: client_generation(input.generation),
        query_ast,
        explain: input.explain,
        max_results: input.max_results,
        max_depth: input.max_depth,
        cost_limit: input.cost_limit,
        page_offset: 0,
    })
}

fn map_query_advanced(
    response: QueryAdvancedPortResponse,
    request: &QueryAdvancedPortRequest,
    next_cursor: Option<ContinuationCursor>,
) -> Result<ReadEnvelope<QueryAdvancedData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    if response.result.columns.is_empty() || response.result.columns.len() > 64 {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(response.result.columns.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for column in response.result.columns {
        columns.push(ColumnSchema {
            name: column.name,
            column_type: column_type_from_label(&column.column_type)?,
        });
    }
    let next_page_offset = response.result.next_page_offset;
    if response.result.rows.len()
        > usize::from(request.max_results.unwrap_or(DEFAULT_ADVANCED_RESULTS))
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let returned_end = request
        .page_offset
        .checked_add(
            u64::try_from(response.result.rows.len())
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
        )
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    let rows = response.result.rows;
    let resolved_generation = response.result.context.generation.to_string();
    let plan = match response.result.plan {
        Some(plan) => {
            if plan.estimated_cost > 10_000_000
                || plan.operators.len() > 64
                || plan.applied_limits.len() > 16
            {
                return Err(internal(ToolExecutionFailure::InvalidResponse));
            }
            let explanation = rootlight_agent::explain::finalize_plan(
                PlanExplanation::new(plan.estimated_cost, plan.operators, plan.applied_limits),
                &resolved_generation,
            );
            RequiredNullable(Some(explanation))
        }
        None => RequiredNullable(None),
    };
    let completeness = query_completeness_from_label(&response.result.completeness)?;
    if matches!(completeness, QueryCompleteness::Paged) != next_page_offset.is_some()
        || next_page_offset.is_some_and(|next| next != returned_end || next <= request.page_offset)
        || next_cursor.is_some() != next_page_offset.is_some()
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let data = QueryAdvancedData {
        columns,
        rows,
        plan,
        completeness,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        next_cursor,
    )
}

fn column_type_from_label(label: &str) -> Result<ColumnType, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn query_completeness_from_label(label: &str) -> Result<QueryCompleteness, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

async fn execute_source_read<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: SourceReadInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let request = normalize_source_read(input, unsupported, invalid_arguments)?;
    if explain_only {
        let output = explain_source_read(port, request, cancellation).await?;
        return serialize_success(output);
    }
    let expected = request.clone();
    let future = port.source_read(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_source_read(response, &expected)?;
    serialize_success(output)
}

async fn await_port<T>(
    future: ClientPortFuture<T>,
    mut cancellation: RequestCancellation,
) -> Result<T, ToolExecutionError> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(internal(ToolExecutionFailure::Executor)),
        response = future => response.map_err(map_port_error),
    }
}

fn decode_input<T>(arguments: Map<String, Value>) -> Result<T, ToolExecutionError>
where
    T: DeserializeOwned,
{
    serde_json::from_value(Value::Object(arguments)).map_err(|_| type_mismatch_error("arguments"))
}

fn authoritative_error(failure: MappedDomainFailure) -> PublicError {
    mapped_public_error(failure)
        .expect("authoritative MCP error mappings satisfy public error bounds")
}

fn stale_generation_error() -> PublicError {
    let definition = error_definition(ErrorCode::StaleGeneration);
    PublicError::builder(ErrorCode::StaleGeneration, definition.message)
        .build()
        .expect("stale-generation registry entry satisfies public error bounds")
}

fn type_mismatch_error(field: &'static str) -> ToolExecutionError {
    ToolExecutionError::new(authoritative_error(MappedDomainFailure::type_mismatch(
        field,
    )))
}

fn binding_invalid_error() -> PublicError {
    authoritative_error(MappedDomainFailure::binding_invalid())
}

fn binding_type_mismatch_error() -> PublicError {
    authoritative_error(MappedDomainFailure::binding_type_mismatch())
}

fn batch_dependency_error(error: BatchExecutionError) -> ToolExecutionError {
    let failure = match error {
        BatchExecutionError::InvalidOperationId | BatchExecutionError::DuplicateOperationId => {
            MappedDomainFailure::invalid_argument("operations")
        }
        BatchExecutionError::UnknownDependency => {
            MappedDomainFailure::invalid_argument("depends_on")
        }
        BatchExecutionError::InvalidBinding => MappedDomainFailure::binding_invalid(),
        BatchExecutionError::Serialization | BatchExecutionError::MemoryUnavailable => {
            return internal(ToolExecutionFailure::Executor);
        }
    };
    ToolExecutionError::new(authoritative_error(failure))
}

fn batch_plan_error(error: BatchValidationError) -> ToolExecutionError {
    ToolExecutionError::new(authoritative_error(error.into()))
}

fn advanced_query_error(error: AdvancedQueryError) -> ToolExecutionError {
    ToolExecutionError::new(authoritative_error(error.into()))
}

fn cost_limit_error(estimated_cost: u64, requested_limit: Option<u64>) -> ToolExecutionError {
    let mut details = vec![(
        DetailKey::parse("estimated_cost").expect("static detail key is valid"),
        PublicValue::Unsigned(estimated_cost),
    )];
    if let Some(limit) = requested_limit {
        details.push((
            DetailKey::parse("cost_limit").expect("static detail key is valid"),
            PublicValue::Unsigned(limit),
        ));
    }
    ToolExecutionError::new(
        public_error_with_details(MappedDomainFailure::cost_limit("cost_limit"), details)
            .expect("authoritative cost-limit details satisfy public bounds"),
    )
}

/// Builds the client-correctable error for malformed tool arguments.
///
/// Argument decoding failures are caller errors, not internal failures, so they
/// are reported as invalid arguments with a stable correct-field action rather
/// than collapsed into an opaque internal error.
fn invalid_input() -> ToolExecutionError {
    ToolExecutionError::new(authoritative_error(MappedDomainFailure::invalid_argument(
        "arguments",
    )))
}

/// Builds the pre-execution error for a schema-valid field this slice does not
/// serve, naming the offending field so a client can correct the request
/// instead of seeing a generic arguments-level rejection.
fn unsupported_field(field: &'static str) -> ToolExecutionError {
    ToolExecutionError::new(authoritative_error(
        MappedDomainFailure::unsupported_capability(field),
    ))
}
fn normalize_repository_index(
    input: RepoIndexInput,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<RepositoryIndexPortRequest, ToolExecutionError> {
    if input.repository_id.is_some()
        || input.scope.is_some()
        || matches!(input.mode, Some(IndexMode::Deep | IndexMode::Rebuild))
        || input
            .requested_tiers
            .as_ref()
            .is_some_and(|tiers| !tiers.is_empty())
        || input
            .configuration_patch
            .as_ref()
            .is_some_and(|patch| !patch.is_empty())
        || input.wait_ms.is_some()
        || input.detached == Some(true)
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let root = input
        .root
        .ok_or_else(|| internal(ToolExecutionFailure::Executor))?;
    if root.contains('\0') {
        return Err(ToolExecutionError::new(invalid_arguments.clone()));
    }
    Ok(RepositoryIndexPortRequest {
        root,
        mode: input.mode.unwrap_or(IndexMode::Auto),
        detached: false,
    })
}

fn normalize_code_locate(
    input: CodeLocateInput,
    unsupported: &PublicError,
) -> Result<CodeLocatePortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    if input.kinds.is_some()
        || input.scope.is_some()
        || input.languages.is_some()
        || input.related_to.is_some()
        || input.min_confidence.is_some()
        || budget_has_unsupported_locate_limits(input.budget.as_ref())
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let mode = locate_mode(input.search_modes.as_ref(), unsupported)?;
    let maximum_results = input
        .max_results
        .into_iter()
        .chain(input.budget.as_ref().and_then(|budget| budget.max_results))
        .min()
        .unwrap_or(DEFAULT_LOCATE_RESULTS);
    Ok(CodeLocatePortRequest {
        repository,
        generation: client_generation(input.generation),
        query: input.query,
        mode,
        maximum_results: u32::from(maximum_results),
        page_offset: 0,
    })
}

fn normalize_symbol_explain(
    input: SymbolExplainInput,
    unsupported: &PublicError,
) -> Result<SymbolExplainPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    if input.sections.is_some()
        || input.relation_sample_limit.is_some()
        || input.source_preview_lines.is_some()
        || input.budget.is_some()
        || matches!(input.include_provenance, Some(ProvenanceLevel::Full))
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let include_provenance = !matches!(input.include_provenance, Some(ProvenanceLevel::None));
    Ok(SymbolExplainPortRequest {
        repository,
        generation: client_generation(input.generation),
        symbols: input.symbol_ids.into_iter().collect(),
        include_provenance,
    })
}

fn normalize_source_read(
    input: SourceReadInput,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<SourceReadPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    if input.context_lines_before.is_some()
        || input.context_lines_after.is_some()
        || input.merge_overlaps == Some(true)
        || input.max_source_bytes.is_some()
        || input.include_line_numbers == Some(false)
        || matches!(input.encoding, Some(SourceEncodingRequest::BytesBase64))
        || input.budget.is_some()
        || !is_compact_profile(input.response_profile)
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }

    let generation = client_generation(input.generation);
    let explicit_generation = match generation {
        client::GenerationSelector::Active => None,
        client::GenerationSelector::Generation(generation) => Some(generation),
    };
    let mut reference_generation = None;
    let mut references = Vec::new();
    references
        .try_reserve_exact(input.references.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for selector in input.references {
        let SourceReadSelector::Reference(reference) = selector else {
            return Err(ToolExecutionError::new(unsupported.clone()));
        };
        let source = reference.source_ref;
        if source.repository() != repository
            || explicit_generation.is_some_and(|generation| source.generation() != generation)
            || reference_generation.is_some_and(|generation| source.generation() != generation)
        {
            return Err(ToolExecutionError::new(invalid_arguments.clone()));
        }
        reference_generation = Some(source.generation());
        let span = source.span();
        let lines = source
            .line_hint()
            .map(|lines| lines.start_line()..=lines.end_line());
        let reference = client::SourceReference::new(
            source.repository(),
            source.generation(),
            span.file(),
            span.start_byte()..span.end_byte(),
            source.content_hash(),
            lines,
        )
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        if references.contains(&reference) {
            return Err(ToolExecutionError::new(invalid_arguments.clone()));
        }
        references.push(reference);
    }
    Ok(SourceReadPortRequest {
        repository,
        generation,
        references,
    })
}

fn repository_id(
    selector: RepositorySelector,
    unsupported: &PublicError,
) -> Result<RepositoryId, ToolExecutionError> {
    match selector {
        RepositorySelector::ById(selector) => Ok(selector.repository_id),
        RepositorySelector::ByAlias(_) => Err(ToolExecutionError::new(unsupported.clone())),
    }
}

fn client_generation(selector: Option<GenerationSelector>) -> client::GenerationSelector {
    match selector {
        None | Some(GenerationSelector::Active(ActiveGeneration::Active)) => {
            client::GenerationSelector::Active
        }
        Some(GenerationSelector::Explicit(generation)) => {
            client::GenerationSelector::Generation(generation)
        }
    }
}

fn client_generation_ref(selector: &GenerationSelector) -> client::GenerationSelector {
    match selector {
        GenerationSelector::Active(ActiveGeneration::Active) => client::GenerationSelector::Active,
        GenerationSelector::Explicit(generation) => {
            client::GenerationSelector::Generation(*generation)
        }
    }
}

fn locate_mode(
    modes: Option<&BTreeSet<SearchMode>>,
    unsupported: &PublicError,
) -> Result<LocateMode, ToolExecutionError> {
    match modes {
        None => Ok(LocateMode::Text),
        Some(modes) if modes.is_empty() => Ok(LocateMode::Text),
        Some(modes) if modes.len() == 1 && modes.contains(&SearchMode::Exact) => {
            Ok(LocateMode::Exact)
        }
        Some(modes) if modes.len() == 1 && modes.contains(&SearchMode::Lexical) => {
            Ok(LocateMode::Text)
        }
        Some(_) => Err(ToolExecutionError::new(unsupported.clone())),
    }
}

fn budget_has_unsupported_locate_limits(budget: Option<&ResponseBudget>) -> bool {
    budget.is_some_and(|budget| {
        budget.max_tokens.is_some()
            || budget.max_source_bytes.is_some()
            || budget.max_traversal_facts.is_some()
            || budget.max_depth.is_some()
            || budget.max_paths.is_some()
            || budget.timeout_ms.is_some()
            || budget.evidence_level.is_some()
    })
}

fn map_repository_index(
    mut response: RepositoryIndexPortResponse,
    expected_mode: IndexMode,
) -> Result<RepoIndexSuccess, ToolExecutionError> {
    if response.accepted_plan.scope != IndexPlanScope::Repository
        || !matches!(
            (expected_mode, response.accepted_plan.mode),
            (
                IndexMode::Auto | IndexMode::Structural,
                IndexMode::Structural
            )
        )
        || response.accepted_plan.parent_generation.0 != response.result.parent_generation
        || response.result.published_generation.is_some()
            != (response.result.state == client::OperationState::Succeeded)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    response.accepted_plan.providers.sort();
    if response
        .accepted_plan
        .providers
        .iter()
        .any(|provider| !safe_label(provider, 128))
        || has_adjacent_duplicates(&response.accepted_plan.providers)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    response.diagnostics.sort_by(|left, right| {
        left.code
            .as_str()
            .cmp(right.code.as_str())
            .then_with(|| left.message.as_str().cmp(right.message.as_str()))
    });
    Ok(RepoIndexSuccess {
        schema_version: SchemaVersion::V1_0,
        data: RepoIndexData {
            repository_id: response.result.repository,
            operation_id: response.result.operation,
            accepted_plan: response.accepted_plan,
            state: operation_state(response.result.state),
            published_generation: RequiredNullable(response.result.published_generation),
            diagnostics: response.diagnostics,
        },
    })
}

fn map_operation_status(
    response: RepositoryOperationStatus,
    expected_operation: OperationId,
) -> Result<OperationStatusSuccess, ToolExecutionError> {
    let operation = response.operation;
    if operation.operation != expected_operation
        || operation.kind != client::OperationKind::RepositoryIndex
        || response.published_generation.is_some()
            != (operation.state == client::OperationState::Succeeded)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let total_units = (operation.total_units != 0).then_some(u64::from(operation.total_units));
    Ok(OperationStatusSuccess {
        schema_version: SchemaVersion::V1_0,
        data: OperationStatusData {
            operation: OperationDetail {
                kind: "repository_index".to_owned(),
                state: operation_state(operation.state),
                stage: operation_stage(operation.stage).to_owned(),
                progress: OperationProgress {
                    completed_units: u64::from(operation.completed_units),
                    total_units: RequiredNullable(total_units),
                },
                revision: operation.revision,
                started_at: format_unix_millis(response.started_unix_ms)?,
                resources: OperationResources {
                    peak_rss_bytes: response.peak_rss_bytes,
                    written_bytes: response.written_bytes,
                    files_examined: response.files_examined,
                },
            },
            published_generation: RequiredNullable(response.published_generation),
            error: RequiredNullable(operation.error),
            retry_after_ms: RequiredNullable(response.retry_after_ms),
        },
    })
}

fn map_code_locate(
    response: CodeLocatePortResponse,
    request: &CodeLocatePortRequest,
    next_cursor: Option<ContinuationCursor>,
) -> Result<ReadEnvelope<CodeLocateData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let returned_end = request
        .page_offset
        .checked_add(
            u64::try_from(response.result.hits.len())
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
        )
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    let next_page_offset = response.result.next_page_offset;
    if response.result.hits.len()
        > usize::try_from(request.maximum_results)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?
        || response.result.matched_candidates < returned_end
        || (!response.result.truncated && response.result.matched_candidates != returned_end)
        || next_page_offset.is_some_and(|next| {
            !response.result.truncated
                || next != returned_end
                || next >= response.result.matched_candidates
        })
        || next_cursor.is_some() != next_page_offset.is_some()
        || response.query_tokens.len() > 128
        || response
            .query_tokens
            .iter()
            .any(|token| token.is_empty() || token.len() > 256)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }

    let reason = match request.mode {
        LocateMode::Exact => LocateReason::Identifier,
        LocateMode::Text => LocateReason::Lexical,
        LocateMode::Prefix | LocateMode::SafeRegex | LocateMode::Glob => {
            return Err(internal(ToolExecutionFailure::InvalidResponse));
        }
    };
    let mut matches = Vec::new();
    matches
        .try_reserve_exact(response.result.hits.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for hit in response.result.hits {
        if hit.identifier.is_empty()
            || hit.identifier.len() > 1_024
            || hit.path.is_empty()
            || hit.path.len() > 8_192
            || !safe_label(&hit.language, 64)
        {
            return Err(internal(ToolExecutionFailure::InvalidResponse));
        }
        let source_ref = hit.source.as_ref().map(client_source_ref).transpose()?;
        matches.push(LocatedItem {
            symbol_id: Some(hit.symbol),
            file_id: Some(hit.file),
            kind: entity_kind(&hit.kind)?,
            display_name: hit.identifier,
            signature: None,
            path: hit.path,
            score: u16::try_from(hit.score)
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
            why: vec![reason],
            source_ref,
            trust: TrustClassification::UntrustedRepositoryData,
        });
    }
    let mode = match request.mode {
        LocateMode::Exact => SearchMode::Exact,
        LocateMode::Text => SearchMode::Lexical,
        LocateMode::Prefix | LocateMode::SafeRegex | LocateMode::Glob => {
            return Err(internal(ToolExecutionFailure::InvalidResponse));
        }
    };
    let data = CodeLocateData {
        matches,
        query_interpretation: QueryInterpretation {
            tokens: response.query_tokens,
            modes: BTreeSet::from([mode]),
            semantic_available: false,
        },
        suggested_next: Vec::new(),
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        next_cursor,
    )
}

fn map_symbol_explain(
    response: SymbolExplainPortResponse,
    request: &SymbolExplainPortRequest,
) -> Result<ReadEnvelope<SymbolExplainData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    let mut symbols = Vec::new();
    symbols
        .try_reserve_exact(response.result.symbols.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for explanation in response.result.symbols {
        if explanation.display_name.is_empty()
            || explanation.display_name.len() > 1_024
            || explanation
                .signature
                .as_ref()
                .is_some_and(|signature| signature.len() > 4_096)
            || !safe_label(&explanation.provider, 128)
            || !safe_label(&explanation.evidence, 128)
        {
            return Err(internal(ToolExecutionFailure::InvalidResponse));
        }
        let confidence = u16::try_from(explanation.confidence)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
        let provenance = if request.include_provenance {
            vec![ProvenanceSummary {
                provider: explanation.provider,
                evidence: explanation.evidence,
                confidence,
            }]
        } else {
            Vec::new()
        };
        symbols.push(SymbolExplanation {
            symbol_id: explanation.symbol,
            kind: entity_kind(&explanation.kind)?,
            display_name: explanation.display_name,
            signature: explanation.signature,
            definition: client_source_ref(&explanation.definition)?,
            relations: rootlight_mcp_contract::vertical::RelationSummary {
                outbound_exact: explanation.outbound_exact,
                outbound_candidates: explanation.outbound_candidates,
                inbound_exact: explanation.inbound_exact,
                inbound_candidates: explanation.inbound_candidates,
                references_exact: explanation.references_exact,
            },
            provenance,
            confidence,
            uncertainty: Vec::new(),
            trust: TrustClassification::UntrustedRepositoryData,
        });
    }
    let data = SymbolExplainData {
        symbols,
        unresolved_ids: response.result.unresolved_symbols,
        detail_handles: Vec::<DetailHandle>::new(),
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn map_source_read(
    response: SourceReadPortResponse,
    request: &SourceReadPortRequest,
) -> Result<ReadEnvelope<SourceReadData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    if response.result.chunks.len() > request.references.len()
        || (!response.result.truncated
            && (response.result.chunks.len() != request.references.len()
                || !response.stale_references.is_empty()
                || !response.elisions.is_empty()))
        || (response.result.truncated
            && response.stale_references.is_empty()
            && response.elisions.is_empty())
        || response
            .stale_references
            .iter()
            .any(|item| usize::from(item.selector_index) >= request.references.len())
        || response
            .elisions
            .iter()
            .any(|item| usize::from(item.selector_index) >= request.references.len())
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }

    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(response.result.chunks.len())
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    for (chunk, requested) in response.result.chunks.into_iter().zip(&request.references) {
        let requested_bytes = requested.byte_range();
        let returned_bytes = chunk
            .end_byte
            .checked_sub(chunk.start_byte)
            .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
        if chunk.source != *requested
            || chunk.start_byte > requested_bytes.start
            || chunk.end_byte < requested_bytes.end
            || chunk.start_line == 0
            || chunk.start_line > chunk.end_line
            || chunk.content_hash != requested.content_hash()
            || u64::try_from(chunk.content.len())
                .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?
                != returned_bytes
            || !safe_label(&chunk.language, 256)
        {
            return Err(internal(ToolExecutionFailure::InvalidResponse));
        }
        let span = SourceSpan::new(requested.file(), chunk.start_byte, chunk.end_byte)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
        let lines = LineRange::new(chunk.start_line, chunk.end_line)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
        let source_ref = SourceRef::new(
            requested.repository(),
            requested.generation(),
            span,
            requested.content_hash(),
            Some(lines),
        );
        chunks.push(SourceChunk {
            source_ref,
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            content: chunk.content,
            encoding: SourceEncoding::Utf8,
            content_hash: chunk.content_hash,
            language: chunk.language,
            generated: chunk.generated,
            trust: TrustClassification::UntrustedRepositoryData,
        });
    }
    let total_source_bytes = u32::try_from(response.result.total_source_bytes)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let data = SourceReadData {
        chunks,
        stale_references: response.stale_references,
        elisions: response.elisions,
        total_source_bytes,
        explanation: None,
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.execution_completeness,
        None,
    )
}

fn map_read_envelope<T>(
    context: client::QueryContext,
    mut metadata: ReadResponseMetadata,
    data: T,
    completeness: client::ResultCompleteness,
    next_cursor: Option<ContinuationCursor>,
) -> Result<ReadEnvelope<T>, ToolExecutionError> {
    if !safe_display_name(&metadata.display_name) || !safe_label(&metadata.trace_id, 128) {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    metadata
        .languages
        .sort_by(|left, right| left.language.cmp(&right.language));
    if metadata
        .languages
        .iter()
        .any(|language| !safe_label(&language.language, 64))
        || metadata
            .languages
            .windows(2)
            .any(|pair| pair[0].language == pair[1].language)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let completeness = contract_completeness(completeness)?;
    if (completeness.continuation == ContinuationAvailability::Available) != next_cursor.is_some() {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    append_completeness_warnings(&mut metadata.warnings, &completeness)?;
    let coverage_status = coverage_status(context.coverage_status);
    let claim_assessment = rootlight_agent::claim_safety::assess_public_claim(
        rootlight_agent::claim_safety::ClaimKind::NegativeExistence,
        Some(&completeness),
        Some(coverage_status),
    );
    if claim_assessment.disposition()
        == rootlight_agent::claim_safety::ClaimDisposition::Inconclusive
    {
        push_completeness_warning(
            &mut metadata.warnings,
            "negative_claims_inconclusive",
            "negative and exhaustive claims are inconclusive",
        )?;
    }
    if metadata.warnings.len() > 100 {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    metadata.warnings.sort_by(|left, right| {
        left.code
            .as_str()
            .cmp(right.code.as_str())
            .then_with(|| left.message.as_str().cmp(right.message.as_str()))
    });
    let wall_time_ms = context.usage.elapsed_micros.div_ceil(1_000);
    let truncated = completeness.state == CompletenessState::Truncated
        || completeness.limiting_resources.iter().any(|resource| {
            !matches!(
                resource.kind,
                ContractLimitingResourceKind::Capability | ContractLimitingResourceKind::Coverage
            )
        });
    Ok(ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: rootlight_mcp_contract::vertical::ResolvedRepository {
            repository_id: context.repository,
            display_name: metadata.display_name,
        },
        generation: GenerationSummary {
            generation_id: context.generation,
            parent_generation: RequiredNullable(context.parent_generation),
            structural_freshness: metadata.structural_freshness,
            semantic_freshness: metadata.semantic_freshness,
        },
        coverage: CoverageSummary {
            status: coverage_status,
            languages: metadata.languages,
            skipped_inputs: context.skipped_inputs,
        },
        data,
        truncated,
        completeness,
        next_cursor: RequiredNullable(next_cursor),
        usage: UsageSummary {
            rows: context.usage.rows,
            edges: context.usage.edges,
            source_bytes: context.usage.source_bytes,
            json_bytes: context.usage.json_bytes,
            estimated_tokens: context.usage.estimated_tokens,
            wall_time_ms,
            cache_status: metadata.cache_status,
            trace_id: metadata.trace_id,
        },
        warnings: metadata.warnings,
        trust: TrustClassification::UntrustedRepositoryData,
    })
}

fn complete_client_result() -> client::ResultCompleteness {
    client::ResultCompleteness {
        state: client::ResultCompletenessState::Complete,
        limiting_resources: Vec::new(),
        continuation: client::ContinuationAvailability::NotApplicable,
        guidance: Vec::new(),
    }
}

fn contract_completeness(
    value: client::ResultCompleteness,
) -> Result<ResultCompleteness, ToolExecutionError> {
    let state = match value.state {
        client::ResultCompletenessState::Complete => CompletenessState::Complete,
        client::ResultCompletenessState::Truncated => CompletenessState::Truncated,
        client::ResultCompletenessState::UnsupportedPartial => {
            CompletenessState::UnsupportedPartial
        }
        client::ResultCompletenessState::Indeterminate => CompletenessState::Indeterminate,
    };
    let limiting_resources = value
        .limiting_resources
        .into_iter()
        .map(|resource| ContractLimitingResource {
            kind: match resource.kind {
                client::LimitingResourceKind::Rows => ContractLimitingResourceKind::Rows,
                client::LimitingResourceKind::Edges => ContractLimitingResourceKind::Edges,
                client::LimitingResourceKind::Results => ContractLimitingResourceKind::Results,
                client::LimitingResourceKind::Depth => ContractLimitingResourceKind::Depth,
                client::LimitingResourceKind::Paths => ContractLimitingResourceKind::Paths,
                client::LimitingResourceKind::SourceBytes => {
                    ContractLimitingResourceKind::SourceBytes
                }
                client::LimitingResourceKind::ResponseBytes => {
                    ContractLimitingResourceKind::ResponseBytes
                }
                client::LimitingResourceKind::MemoryBytes => {
                    ContractLimitingResourceKind::MemoryBytes
                }
                client::LimitingResourceKind::Deadline => ContractLimitingResourceKind::Deadline,
                client::LimitingResourceKind::EstimatedTokens => {
                    ContractLimitingResourceKind::EstimatedTokens
                }
                client::LimitingResourceKind::Cancellation => {
                    ContractLimitingResourceKind::Cancellation
                }
                client::LimitingResourceKind::Capability => {
                    ContractLimitingResourceKind::Capability
                }
                client::LimitingResourceKind::Coverage => ContractLimitingResourceKind::Coverage,
                client::LimitingResourceKind::PageSize => ContractLimitingResourceKind::PageSize,
            },
            limit: resource.limit,
            observed: resource.observed,
        })
        .collect();
    let continuation = match value.continuation {
        client::ContinuationAvailability::NotApplicable => ContinuationAvailability::NotApplicable,
        client::ContinuationAvailability::Available => ContinuationAvailability::Available,
        client::ContinuationAvailability::Unavailable => ContinuationAvailability::Unavailable,
    };
    let guidance = value
        .guidance
        .into_iter()
        .map(|guidance| match guidance {
            client::ContinuationGuidance::UseCursor => ContinuationGuidance::UseCursor,
            client::ContinuationGuidance::NarrowScope => ContinuationGuidance::NarrowScope,
            client::ContinuationGuidance::SplitRequest => ContinuationGuidance::SplitRequest,
            client::ContinuationGuidance::ReduceDepth => ContinuationGuidance::ReduceDepth,
            client::ContinuationGuidance::ReduceRelations => ContinuationGuidance::ReduceRelations,
            client::ContinuationGuidance::RequestSource => ContinuationGuidance::RequestSource,
            client::ContinuationGuidance::IncreaseBudgetWithinLimit => {
                ContinuationGuidance::IncreaseBudgetWithinLimit
            }
            client::ContinuationGuidance::RefreshCoverage => ContinuationGuidance::RefreshCoverage,
            client::ContinuationGuidance::UnsupportedNoContinuation => {
                ContinuationGuidance::UnsupportedNoContinuation
            }
        })
        .collect();
    ResultCompleteness::new(state, limiting_resources, continuation, guidance)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

fn append_completeness_warnings(
    warnings: &mut Vec<ResponseWarning>,
    completeness: &ResultCompleteness,
) -> Result<(), ToolExecutionError> {
    let state_warning = match completeness.state {
        CompletenessState::Complete => None,
        CompletenessState::Truncated => {
            Some(("result_truncated", "a bounded limit stopped execution"))
        }
        CompletenessState::UnsupportedPartial => Some((
            "unsupported_partial",
            "the result excludes an unsupported portion",
        )),
        CompletenessState::Indeterminate => Some((
            "completeness_indeterminate",
            "result completeness is indeterminate",
        )),
    };
    if let Some((code, message)) = state_warning {
        push_completeness_warning(warnings, code, message)?;
    }
    for resource in &completeness.limiting_resources {
        let (code, message) = match resource.kind {
            ContractLimitingResourceKind::Rows => ("limit_rows", "row limit stopped execution"),
            ContractLimitingResourceKind::Edges => ("limit_edges", "edge limit stopped execution"),
            ContractLimitingResourceKind::Results => {
                ("limit_results", "result limit stopped execution")
            }
            ContractLimitingResourceKind::Depth => ("limit_depth", "depth limit stopped execution"),
            ContractLimitingResourceKind::Paths => ("limit_paths", "path limit stopped execution"),
            ContractLimitingResourceKind::SourceBytes => {
                ("limit_source_bytes", "source byte limit stopped execution")
            }
            ContractLimitingResourceKind::ResponseBytes => (
                "limit_response_bytes",
                "response byte limit stopped execution",
            ),
            ContractLimitingResourceKind::MemoryBytes => {
                ("limit_memory", "memory limit stopped execution")
            }
            ContractLimitingResourceKind::Deadline => {
                ("limit_deadline", "deadline stopped execution")
            }
            ContractLimitingResourceKind::EstimatedTokens => {
                ("limit_tokens", "token limit stopped execution")
            }
            ContractLimitingResourceKind::Cancellation => {
                ("limit_cancellation", "cancellation stopped execution")
            }
            ContractLimitingResourceKind::Capability => (
                "limit_capability",
                "an unavailable capability bounded the result",
            ),
            ContractLimitingResourceKind::Coverage => {
                ("limit_coverage", "coverage bounded the result")
            }
            ContractLimitingResourceKind::PageSize => {
                ("limit_page_size", "page size bounded the result")
            }
        };
        push_completeness_warning(warnings, code, message)?;
    }
    for guidance in &completeness.guidance {
        let (code, message) = match guidance {
            ContinuationGuidance::UseCursor => {
                ("use_cursor", "continue with the authenticated cursor")
            }
            ContinuationGuidance::NarrowScope => ("narrow_scope", "narrow the request scope"),
            ContinuationGuidance::SplitRequest => ("split_request", "split the request"),
            ContinuationGuidance::ReduceDepth => ("reduce_depth", "reduce traversal depth"),
            ContinuationGuidance::ReduceRelations => {
                ("reduce_relations", "reduce the relation projection")
            }
            ContinuationGuidance::RequestSource => ("request_source", "request source separately"),
            ContinuationGuidance::IncreaseBudgetWithinLimit => (
                "increase_budget",
                "increase the budget within the supported limit",
            ),
            ContinuationGuidance::RefreshCoverage => {
                ("refresh_coverage", "refresh indexed coverage")
            }
            ContinuationGuidance::UnsupportedNoContinuation => (
                "no_continuation",
                "the unsupported portion has no continuation",
            ),
        };
        push_completeness_warning(warnings, code, message)?;
    }
    Ok(())
}

fn push_completeness_warning(
    warnings: &mut Vec<ResponseWarning>,
    code: &str,
    message: &str,
) -> Result<(), ToolExecutionError> {
    warnings
        .try_reserve(1)
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    warnings.push(ResponseWarning {
        code: SafeLabel::parse(code)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
        message: SourceFreeMessage::parse(message)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?,
    });
    Ok(())
}

fn validate_query_context(
    context: &client::QueryContext,
    repository: RepositoryId,
    generation: client::GenerationSelector,
) -> Result<(), ToolExecutionError> {
    let generation_matches = match generation {
        client::GenerationSelector::Active => context.active_generation,
        client::GenerationSelector::Generation(expected) => context.generation == expected,
    };
    if context.repository != repository
        || !generation_matches
        || context.parent_generation == Some(context.generation)
    {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    Ok(())
}

const fn operation_state(state: client::OperationState) -> OperationState {
    match state {
        client::OperationState::Queued => OperationState::Queued,
        client::OperationState::Running | client::OperationState::Cancelling => {
            OperationState::Running
        }
        client::OperationState::Succeeded => OperationState::Published,
        client::OperationState::Failed | client::OperationState::Interrupted => {
            OperationState::Failed
        }
        client::OperationState::Cancelled => OperationState::Cancelled,
    }
}

const fn operation_stage(stage: client::OperationStage) -> &'static str {
    match stage {
        client::OperationStage::Accepted => "accepted",
        client::OperationStage::Executing => "executing",
        client::OperationStage::Cleanup => "cleanup",
    }
}

const fn coverage_status(status: client::CoverageStatus) -> rootlight_ir::CoverageStatus {
    match status {
        client::CoverageStatus::Complete => rootlight_ir::CoverageStatus::Complete,
        client::CoverageStatus::Bounded => rootlight_ir::CoverageStatus::Bounded,
        client::CoverageStatus::Sampled => rootlight_ir::CoverageStatus::Sampled,
        client::CoverageStatus::Unknown => rootlight_ir::CoverageStatus::Unknown,
    }
}

fn entity_kind(kind: &str) -> Result<EntityKind, ToolExecutionError> {
    let kind = match kind {
        "file" => EntityKind::File,
        "module" | "namespace" => EntityKind::Module,
        "class" | "struct" | "enum" | "union" | "type_alias" | "trait" | "interface"
        | "protocol" | "type_parameter" => EntityKind::Type,
        "function" | "closure" => EntityKind::Function,
        "method" | "constructor" => EntityKind::Method,
        "field" | "property" => EntityKind::Field,
        "constant" => EntityKind::Constant,
        "variable" | "parameter" => EntityKind::Variable,
        "configuration_key" => EntityKind::Configuration,
        _ => return Err(internal(ToolExecutionFailure::InvalidResponse)),
    };
    Ok(kind)
}

fn client_source_ref(reference: &client::SourceReference) -> Result<SourceRef, ToolExecutionError> {
    let bytes = reference.byte_range();
    let span = SourceSpan::new(reference.file(), bytes.start, bytes.end)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let lines = reference
        .line_range()
        .map(|lines| LineRange::new(*lines.start(), *lines.end()))
        .transpose()
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    Ok(SourceRef::new(
        reference.repository(),
        reference.generation(),
        span,
        reference.content_hash(),
        lines,
    ))
}

fn format_unix_millis(value: u64) -> Result<String, ToolExecutionError> {
    const SECONDS_PER_DAY: u64 = 86_400;
    let seconds = value / 1_000;
    let millis = value % 1_000;
    let days = seconds / SECONDS_PER_DAY;
    let day_seconds = seconds % SECONDS_PER_DAY;
    let days = i64::try_from(days).map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;

    // This is the proleptic Gregorian conversion for nonnegative Unix days.
    // Keeping it local avoids adding a time dependency solely for one wire field.
    let shifted = days
        .checked_add(719_468)
        .ok_or_else(|| internal(ToolExecutionFailure::InvalidResponse))?;
    let era = shifted / 146_097;
    let day_of_era = shifted - (era * 146_097);
    let year_of_era =
        (day_of_era - (day_of_era / 1_460) + (day_of_era / 36_524) - (day_of_era / 146_096)) / 365;
    let mut year = year_of_era + (era * 400);
    let day_of_year = day_of_era - ((365 * year_of_era) + (year_of_era / 4) - (year_of_era / 100));
    let month_prime = ((5 * day_of_year) + 2) / 153;
    let day = day_of_year - (((153 * month_prime) + 2) / 5) + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    if !(1970..=9999).contains(&year) {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    if millis == 0 {
        Ok(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
        ))
    } else {
        Ok(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
        ))
    }
}

fn safe_display_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b' ' || matches!(byte, b'_' | b'-' | b'.')
        })
}

fn safe_label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':' | b'+')
        })
}

fn has_adjacent_duplicates(values: &[String]) -> bool {
    values.windows(2).any(|pair| pair[0] == pair[1])
}

fn serialize_success<T>(output: T) -> Result<Map<String, Value>, ToolExecutionError>
where
    T: Serialize,
{
    let value = serde_json::to_value(ToolResponse::Success(output))
        .map_err(|_| internal(ToolExecutionFailure::Executor))?;
    let Value::Object(output) = value else {
        return Err(internal(ToolExecutionFailure::Executor));
    };
    Ok(output)
}

fn serialize_catalog_success(
    output: CatalogEnvelope<RepoListData>,
    started_at: Instant,
) -> Result<Map<String, Value>, ToolExecutionError> {
    serialize_measured_success(output, started_at, |output| &mut output.usage)
}

fn serialize_measured_read_success<T>(
    output: ReadEnvelope<T>,
    started_at: Instant,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    T: Serialize,
{
    serialize_measured_success(output, started_at, |output| &mut output.usage)
}

fn serialize_profiled_read_success<T>(
    mut output: ReadEnvelope<T>,
    profile: ResponseProfile,
    started_at: Instant,
    shaping: ResponseShaping,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    T: Serialize + rootlight_agent::response_profile::ProfileShape,
{
    match shaping {
        ResponseShaping::Public => {
            rootlight_agent::response_profile::shape_read_envelope(&mut output, profile);
            serialize_measured_read_success(output, started_at)
        }
        ResponseShaping::CanonicalInternal => serialize_success(output),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseShaping {
    Public,
    CanonicalInternal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CursorPresentation {
    shaping: ResponseShaping,
    exposure_profile: ExposureProfile,
}

fn serialize_measured_success<T, F>(
    mut output: T,
    started_at: Instant,
    usage: F,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    T: Serialize,
    F: for<'a> Fn(&'a mut T) -> &'a mut UsageSummary,
{
    for _ in 0..8 {
        // This timestamp covers decode, cursor validation, daemon I/O, mapping,
        // and prior accounting passes. The final serde conversion is excluded
        // because including work performed after a serialized timestamp would
        // make an exact self-describing payload impossible.
        usage(&mut output).wall_time_ms = u64::try_from(started_at.elapsed().as_micros())
            .unwrap_or(u64::MAX)
            .div_ceil(1_000);
        let value = serde_json::to_value(ToolResponse::Success(&output))
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        let serialized =
            serde_json::to_vec(&value).map_err(|_| internal(ToolExecutionFailure::Executor))?;
        let json_bytes = u64::try_from(serialized.len())
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;
        let estimated_tokens =
            rootlight_mcp_contract::accounting::estimate_tokens(serialized.len());
        let counters_match = {
            let usage = usage(&mut output);
            usage.json_bytes == json_bytes && usage.estimated_tokens == estimated_tokens
        };
        if counters_match {
            let Value::Object(output) = value else {
                return Err(internal(ToolExecutionFailure::Executor));
            };
            return Ok(output);
        }
        // Both counters are serialized into the measured document. Iterating
        // to a fixed point keeps the reported byte count exact across digit
        // width changes without excluding the accounting fields themselves.
        let usage = usage(&mut output);
        usage.json_bytes = json_bytes;
        usage.estimated_tokens = estimated_tokens;
    }
    Err(internal(ToolExecutionFailure::Executor))
}

fn map_port_error(error: ClientPortError) -> ToolExecutionError {
    match error {
        ClientPortError::Public(error) => ToolExecutionError::new(*error),
        ClientPortError::Transport => internal(ToolExecutionFailure::Transport),
        ClientPortError::InvalidResponse => internal(ToolExecutionFailure::InvalidResponse),
        ClientPortError::Executor => internal(ToolExecutionFailure::Executor),
    }
}

const fn internal(failure: ToolExecutionFailure) -> ToolExecutionError {
    ToolExecutionError::internal(failure)
}

#[cfg(test)]
mod tests;
