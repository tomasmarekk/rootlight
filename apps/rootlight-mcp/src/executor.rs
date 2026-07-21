//! Typed production mapping between the MCP tool catalog and a daemon client port.
//!
//! The port supplies facts absent from the current client DTOs so this layer
//! never fabricates index-plan, freshness, coverage, cache, or trace metadata.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
};

use rootlight_client::{
    self as client, CodeLocate, LocateMode, RepositoryIndex, RepositoryList,
    RepositoryOperationAction, RepositoryOperationStatus, RepositoryStatus, SourceRead,
    SymbolExplain,
};
use rootlight_ids::{FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{EntityKind as IrEntityKind, LineRange, SourceRef, SourceSpan};
use rootlight_mcp_contract::change::{
    ArchitectureDelta, BreakingCandidate, ChangeClassification, ChangeImpactData,
    ChangeImpactInput, ChangePlanStep, CompareChangeKind, ContextPackRequest, HistoryCompareData,
    HistoryCompareInput, ImpactEntry, ImpactGroup, ImpactRiskSummary, LineageMatch, MatchedStates,
    PlanChangeData, PlanChangeInput, PlanDecision, PlanImpactSummary, PlanObjective,
    PlanTargetSelector, RankedTest, RelationPolicy, ResolvedChange, RevisionSelector, RiskLevel,
    SemanticChange, SemanticChangeKind, TestCandidate, TestCoverageStrategy, TestGap, TestKind,
    TestsSelectData, TestsSelectInput,
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
    DetailKey, ErrorCode, GenerationSelector, McpTool, NextAction, PublicError,
    PublicErrorBuildError, RepoIndexInput, RepositorySelector, SafeLabel, SchemaVersion,
    SourceReadInput, SymbolExplainInput, ToolResponse, TrustClassification, VerticalTool,
    context::{
        BatchOperation, BatchOperationResult, BatchOperationStatus, BatchStatus, BatchTool,
        ColumnSchema, ColumnType, ContextItem, ContextPackData, ContextPackId, ContextPackInput,
        ContextStructure, EvidenceRole as ContextEvidenceRole, FailurePolicy, OmissionSummary,
        PlanExplanation, QueryAdvancedData, QueryAdvancedInput, QueryAstNode, QueryBatchData,
        QueryBatchInput, QueryCompleteness, TokenAccounting, ToolSuggestion,
    },
    pagination::{AuthenticatedCursor, CursorContext},
    repository::{
        CoverageReport, LanguageCoverageReport, RepoListData, RepoListInput, RepoStatusData,
        RepoStatusInput, RepositoryEntry, RepositoryState,
    },
    vertical::{
        ActiveGeneration, AnalysisTier, CacheStatus, CodeLocateData, CodeLocateInput,
        ContinuationCursor, CoverageSummary, DetailHandle, Diagnostic, EntityKind, Freshness,
        GenerationSummary, IndexMode, IndexPlanScope, IndexPlanSummary, IndexScope,
        LanguageCoverage, LocateReason, LocatedItem, OperationAction, OperationDetail,
        OperationProgress, OperationResources, OperationState, OperationStatusData,
        OperationStatusInput, OperationStatusSuccess, ProvenanceLevel, ProvenanceSummary,
        QueryInterpretation, ReadEnvelope, RepoIndexData, RepoIndexSuccess, RequiredNullable,
        ResolvedRepository, ResponseBudget, ResponseProfile, ResponseWarning, SearchMode,
        SourceChunk, SourceElision, SourceEncoding, SourceEncodingRequest, SourceFreeMessage,
        SourceReadData, SourceReadSelector, StaleSourceReference, SymbolExplainData,
        SymbolExplanation, UsageSummary,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::advanced::{AdvancedQueryPlan, MAX_ADVANCED_TRAVERSAL, QueryOperator};
use crate::batch::BatchPlan;
use crate::context_pack::{
    EvidenceCandidate as PackEvidenceCandidate, EvidenceRole as PackEvidenceRole, PackObjective,
    PackResult, optimize_pack,
};
use crate::tools::mcp_tool_for_batch;
use crate::{
    RequestCancellation, ToolExecutionError, ToolExecutionFailure, ToolExecutionFuture,
    ToolExecutor,
};

const DEFAULT_LOCATE_RESULTS: u16 = 20;
const DEFAULT_ADVANCED_RESULTS: u16 = 100;
const CURRENT_SOURCE_CONTEXT_LINES: u8 = 2;
const INVALID_ARGUMENT_MESSAGE: &str = "tool arguments are invalid";
const UNSUPPORTED_MESSAGE: &str = "requested option is not supported";
const BATCH_OPERATION_FAILED_MESSAGE: &str = "batch operation failed";
const INVALID_CURSOR_MESSAGE: &str = "pagination cursor is invalid or expired";

/// Future returned by one injected first-slice client-port operation.
pub type ClientPortFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, ClientPortError>> + Send + 'static>>;

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
    fn repository_list(
        &self,
        request: RepositoryListPortRequest,
        cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryList>;

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

/// Normalized `repo.list` request accepted by the current first-slice daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryListPortRequest {
    max_results: Option<u32>,
    query: Option<String>,
}

impl RepositoryListPortRequest {
    /// Creates a bounded repository list request.
    #[must_use]
    pub fn new(max_results: Option<u32>, query: Option<String>) -> Self {
        Self { max_results, query }
    }

    /// Returns the optional result bound.
    #[must_use]
    pub const fn max_results(&self) -> Option<u32> {
        self.max_results
    }

    /// Returns the optional display-name filter.
    #[must_use]
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
}

/// Normalized `repo.status` request accepted by the current first-slice daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryStatusPortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
}

impl RepositoryStatusPortRequest {
    /// Creates a repository status request for one resolved repository.
    #[must_use]
    pub const fn new(repository: RepositoryId, generation: client::GenerationSelector) -> Self {
        Self {
            repository,
            generation,
        }
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

/// Normalized `plan.change` request ready for the daemon client port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanChangePortRequest {
    repository: RepositoryId,
    generation: client::GenerationSelector,
    objective: String,
    objective_text: String,
    target_symbols: Vec<SymbolId>,
    target_files: Vec<FileId>,
    max_steps: Option<u8>,
}

impl PlanChangePortRequest {
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

    /// Returns the objective wire label.
    #[must_use]
    pub fn objective(&self) -> &str {
        &self.objective
    }

    /// Returns the concrete objective description.
    #[must_use]
    pub fn objective_text(&self) -> &str {
        &self.objective_text
    }

    /// Returns the explicit target symbol identifiers.
    #[must_use]
    pub fn target_symbols(&self) -> &[SymbolId] {
        &self.target_symbols
    }

    /// Returns the explicit target file identifiers.
    #[must_use]
    pub fn target_files(&self) -> &[FileId] {
        &self.target_files
    }

    /// Returns the optional step cap.
    #[must_use]
    pub const fn max_steps(&self) -> Option<u8> {
        self.max_steps
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
}

/// Production MCP executor over an injected asynchronous daemon-client port.
pub struct FirstSliceToolExecutor<P> {
    port: Arc<P>,
    invalid_arguments: PublicError,
    unsupported: PublicError,
    invalid_cursor: PublicError,
    /// Process-local secret used to authenticate pagination cursors.
    ///
    /// It rotates on process restart, gracefully invalidating outstanding
    /// cursors (they fail validation and clients restart the listing).
    cursor_key: [u8; 32],
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
        // A process-local cursor signing key gives cursors per-process
        // rotation. Key generation fails closed: when secure entropy is
        // unavailable, construction fails rather than falling back to a
        // reproducible all-zero key.
        let mut cursor_key = [0_u8; 32];
        getrandom::fill(&mut cursor_key)
            .map_err(|_| ToolExecutorBuildError::CursorKeyInitialization)?;
        Self::build(port, cursor_key)
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
        Self::build(port, cursor_key)
    }

    fn build(port: P, cursor_key: [u8; 32]) -> Result<Self, ToolExecutorBuildError> {
        let field =
            DetailKey::parse("arguments").map_err(ToolExecutorBuildError::UnsupportedError)?;
        let unsupported =
            PublicError::builder(ErrorCode::UnsupportedCapability, UNSUPPORTED_MESSAGE)
                .next_action(NextAction::CorrectField { field })
                .build()
                .map_err(ToolExecutorBuildError::UnsupportedError)?;
        let field =
            DetailKey::parse("arguments").map_err(ToolExecutorBuildError::InvalidArgumentError)?;
        let invalid_arguments =
            PublicError::builder(ErrorCode::InvalidArgument, INVALID_ARGUMENT_MESSAGE)
                .next_action(NextAction::CorrectField { field })
                .build()
                .map_err(ToolExecutorBuildError::InvalidArgumentError)?;
        let invalid_cursor = PublicError::builder(ErrorCode::InvalidCursor, INVALID_CURSOR_MESSAGE)
            .next_action(NextAction::RestartEnumeration)
            .build()
            .map_err(ToolExecutorBuildError::InvalidArgumentError)?;
        Ok(Self {
            port: Arc::new(port),
            invalid_arguments,
            unsupported,
            invalid_cursor,
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
        cancellation: RequestCancellation,
    ) -> ToolExecutionFuture {
        let port = Arc::clone(&self.port);
        let invalid_arguments = self.invalid_arguments.clone();
        let unsupported = self.unsupported.clone();
        let invalid_cursor = self.invalid_cursor.clone();
        let cursor_key = self.cursor_key;
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
                    execute_repo_list(port, arguments, cancellation, &invalid_cursor, cursor_key)
                        .await
                }
                VerticalTool::ChangeImpact => {
                    execute_change_impact(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::PlanChange => {
                    execute_plan_change(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::HistoryCompare => {
                    execute_history_compare(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::QueryAdvanced => {
                    execute_query_advanced(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::SymbolRelationships => {
                    execute_symbol_relationships(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::FlowTrace => {
                    execute_flow_trace(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::ArchitectureCycles => {
                    execute_architecture_cycles(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::CodeDead => {
                    execute_code_dead(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::ArchitectureOverview => {
                    execute_architecture_overview(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::TestsSelect => {
                    execute_tests_select(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::ContextPack => {
                    execute_context_pack(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::QueryBatch => {
                    execute_query_batch(
                        port,
                        arguments,
                        cancellation,
                        &unsupported,
                        &invalid_arguments,
                    )
                    .await
                }
                VerticalTool::OperationStatus => {
                    execute_operation_status(port, arguments, cancellation).await
                }
                VerticalTool::CodeLocate => {
                    execute_code_locate(port, arguments, cancellation, &unsupported).await
                }
                VerticalTool::SymbolExplain => {
                    execute_symbol_explain(port, arguments, cancellation, &unsupported).await
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

/// Fails intent tools that have no production engine behind this bridge yet.
///
/// These tools are advertised in the catalog but cannot produce a provable
/// generation-pinned result here, so they return a checked, schema-valid
/// capability error instead of fabricating repository or generation identity,
/// coverage, or data that Rootlight cannot prove. The router validates the
/// input schema before execution, so malformed requests are rejected before
/// this point.
/// Executes a bounded `query.batch` by composing the read tools that already
/// have a production engine behind this bridge.
///
/// Operations run in dependency order under one pinned generation. Subtools
/// without an engine fail locally with a checked capability error and their
/// dependents are skipped. The batch pins its generation from the first
/// successful operation; when nothing succeeds the batch itself fails rather
/// than fabricating an identity.
async fn execute_query_batch<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: QueryBatchInput = decode_input(arguments)?;
    let repository = repository_id(input.repository.clone(), unsupported)?;
    // Shared budgets and aggregate response profiles are not enforced by this
    // slice; each is rejected by name rather than silently ignored.
    if input.budget.is_some() {
        return Err(unsupported_field("budget"));
    }
    if !compact_profile(input.response_profile) {
        return Err(unsupported_field("response_profile"));
    }
    let operation_failed =
        PublicError::builder(ErrorCode::Internal, BATCH_OPERATION_FAILED_MESSAGE)
            .build()
            .map_err(|_| internal(ToolExecutionFailure::Executor))?;

    let tools: Vec<McpTool> = input
        .operations
        .iter()
        .map(|operation| mcp_tool_for_batch(operation.tool))
        .collect();
    let dependencies = resolve_batch_dependencies(&input.operations)
        .ok_or_else(|| ToolExecutionError::new(invalid_arguments.clone()))?;
    let plan = BatchPlan::validate(&tools, &dependencies)
        .map_err(|_| ToolExecutionError::new(invalid_arguments.clone()))?;

    let fail_fast = matches!(input.failure_policy, Some(FailurePolicy::FailFast));
    let count = input.operations.len();
    let mut results: Vec<Option<BatchOperationResult>> = vec![None; count];
    let mut envelopes: Vec<Option<ReadEnvelope<Value>>> = vec![None; count];
    let mut stop_scheduling = false;

    for index in plan.execution_order.clone() {
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
        let sub_arguments =
            resolve_batch_arguments(operation, &envelopes, &input, &dependencies[index]);
        let sub_arguments = match sub_arguments {
            Ok(arguments) => arguments,
            Err(()) => {
                results[index] = Some(error_result(operation, invalid_arguments));
                stop_scheduling |= fail_fast;
                continue;
            }
        };
        match execute_batch_subtool(
            operation.tool,
            port.clone(),
            sub_arguments,
            cancellation.clone(),
            unsupported,
            invalid_arguments,
        )
        .await
        {
            Ok(response) => {
                let envelope: ReadEnvelope<Value> = serde_json::from_value(Value::Object(response))
                    .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
                results[index] = Some(success_result(operation, &envelope));
                envelopes[index] = Some(envelope);
            }
            Err(error) => {
                let fallback = error
                    .public_error()
                    .cloned()
                    .unwrap_or_else(|| operation_failed.clone());
                results[index] = Some(error_result(operation, &fallback));
                stop_scheduling |= fail_fast;
            }
        }
    }

    let operation_results: Vec<BatchOperationResult> = results.into_iter().flatten().collect();
    let Some(source) = envelopes.iter().flatten().next() else {
        let first_error = operation_results
            .iter()
            .find_map(|result| result.error.clone())
            .unwrap_or(operation_failed);
        return Err(ToolExecutionError::new(first_error));
    };

    let truncated = operation_results.iter().any(|result| result.truncated);
    let batch_status = aggregate_batch_status(&operation_results);
    let usage = aggregate_batch_usage(&envelopes);
    let data = QueryBatchData {
        batch_status,
        generation_id: source.generation.generation_id,
        operation_results,
    };
    let envelope = ReadEnvelope {
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
    };
    serialize_success(envelope)
}

/// Resolves batch operation dependencies to indices, rejecting duplicate
/// operation ids and references to unknown operations.
fn resolve_batch_dependencies(operations: &[BatchOperation]) -> Option<Vec<Vec<usize>>> {
    let mut seen = BTreeSet::new();
    for operation in operations {
        if !seen.insert(operation.id.clone()) {
            return None;
        }
    }
    let mut dependencies = Vec::with_capacity(operations.len());
    for operation in operations {
        let mut resolved = Vec::new();
        if let Some(declared) = &operation.depends_on {
            for name in declared {
                let index = operations.iter().position(|other| other.id == *name)?;
                resolved.push(index);
            }
        }
        dependencies.push(resolved);
    }
    Some(dependencies)
}

/// Reports whether any declared dependency did not complete successfully.
fn dependency_failed(dependencies: &[usize], results: &[Option<BatchOperationResult>]) -> bool {
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

/// Builds subtool arguments by resolving typed bindings and injecting the
/// batch-inherited repository and generation.
fn resolve_batch_arguments(
    operation: &BatchOperation,
    envelopes: &[Option<ReadEnvelope<Value>>],
    input: &QueryBatchInput,
    declared: &[usize],
) -> Result<Map<String, Value>, ()> {
    let mut arguments = Map::new();
    for (key, value) in &operation.arguments {
        let resolved = resolve_batch_binding(value, envelopes, &input.operations, declared)?;
        arguments.insert(key.clone(), resolved);
    }
    arguments.insert(
        "repository".to_owned(),
        serde_json::to_value(&input.repository).map_err(|_| ())?,
    );
    if let Some(generation) = &input.generation {
        arguments.insert(
            "generation".to_owned(),
            serde_json::to_value(generation).map_err(|_| ())?,
        );
    }
    Ok(arguments)
}

/// Recursively replaces `{"$from", "pointer"}` binding leaves with the value at
/// the referenced JSON pointer in the completed dependency response.
fn resolve_batch_binding(
    value: &Value,
    envelopes: &[Option<ReadEnvelope<Value>>],
    operations: &[BatchOperation],
    declared: &[usize],
) -> Result<Value, ()> {
    match value {
        Value::Object(map) => {
            if let Some(from) = map.get("$from") {
                let from_name = from.as_str().ok_or(())?;
                let pointer = map.get("pointer").and_then(Value::as_str).ok_or(())?;
                let dependency = declared
                    .iter()
                    .find(|&&index| operations[index].id == from_name)
                    .ok_or(())?;
                let envelope = envelopes[*dependency].as_ref().ok_or(())?;
                let encoded = serde_json::to_value(envelope).map_err(|_| ())?;
                encoded.pointer(pointer).cloned().ok_or(())
            } else {
                let mut resolved = Map::new();
                for (key, inner) in map {
                    resolved.insert(
                        key.clone(),
                        resolve_batch_binding(inner, envelopes, operations, declared)?,
                    );
                }
                Ok(Value::Object(resolved))
            }
        }
        Value::Array(items) => {
            let mut resolved = Vec::with_capacity(items.len());
            for inner in items {
                resolved.push(resolve_batch_binding(
                    inner, envelopes, operations, declared,
                )?);
            }
            Ok(Value::Array(resolved))
        }
        scalar => Ok(scalar.clone()),
    }
}

/// Dispatches one batch subtool to its production handler, failing with a
/// checked capability error for subtools that have no engine yet.
async fn execute_batch_subtool<P>(
    tool: BatchTool,
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    match tool {
        BatchTool::CodeLocate => {
            execute_code_locate(port, arguments, cancellation, unsupported).await
        }
        BatchTool::SymbolExplain => {
            execute_symbol_explain(port, arguments, cancellation, unsupported).await
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
        BatchTool::SymbolRelationships
        | BatchTool::FlowTrace
        | BatchTool::ChangeImpact
        | BatchTool::TestsSelect
        | BatchTool::ArchitectureOverview
        | BatchTool::ArchitectureCycles
        | BatchTool::CodeDead
        | BatchTool::PlanChange
        | BatchTool::ContextPack => Err(ToolExecutionError::new(unsupported.clone())),
    }
}

fn success_result(
    operation: &BatchOperation,
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

fn error_result(operation: &BatchOperation, error: &PublicError) -> BatchOperationResult {
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

fn terminal_result(
    operation: &BatchOperation,
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

fn aggregate_batch_status(results: &[BatchOperationResult]) -> BatchStatus {
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

fn aggregate_batch_usage(envelopes: &[Option<ReadEnvelope<Value>>]) -> UsageSummary {
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

/// Assembles a bounded `context.pack` from seed-symbol definition evidence.
///
/// Retrieval is served through `symbol.explain`; the deterministic pack
/// optimizer ranks the evidence and enforces minimum representation for the
/// objective-derived required roles under the requested token budget. Source
/// snippets are not included in this slice. Repository-derived content is
/// labeled untrusted and the guidance stays source-free.
async fn execute_context_pack<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: ContextPackInput = decode_input(arguments)?;
    // Alias selectors cannot be resolved behind this bridge.
    repository_id(input.repository.clone(), unsupported)?;

    // This slice assembles packs from symbol/test seeds only. The other seed
    // kinds and selection controls are not served; each is rejected by name so
    // it is never silently ignored.
    if input.seeds.paths.is_some() {
        return Err(unsupported_field("paths"));
    }
    if input.seeds.routes.is_some() {
        return Err(unsupported_field("routes"));
    }
    if input.seeds.located.is_some() {
        return Err(unsupported_field("located"));
    }
    if input.seeds.change.is_some() {
        return Err(unsupported_field("change"));
    }
    if input.seeds.plan.is_some() {
        return Err(unsupported_field("plan"));
    }
    if input.source_policy.is_some() {
        return Err(unsupported_field("source_policy"));
    }
    if input.sections.is_some() {
        return Err(unsupported_field("sections"));
    }
    if input.diversity.is_some() {
        return Err(unsupported_field("diversity"));
    }
    if input.min_confidence.is_some() {
        return Err(unsupported_field("min_confidence"));
    }
    if input.continuation.is_some() {
        return Err(unsupported_field("continuation"));
    }

    let mut seed_symbols: BTreeSet<SymbolId> = BTreeSet::new();
    if let Some(symbols) = &input.seeds.symbols {
        seed_symbols.extend(symbols.iter().copied());
    }
    if let Some(tests) = &input.seeds.tests {
        seed_symbols.extend(tests.iter().copied());
    }
    if seed_symbols.is_empty() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // symbol.explain bounds a single request to sixteen symbols.
    let seed_symbols: BTreeSet<SymbolId> = seed_symbols.into_iter().take(16).collect();

    let mut explain_arguments = Map::new();
    explain_arguments.insert("repository".to_owned(), serialize_json(&input.repository)?);
    if let Some(generation) = &input.generation {
        explain_arguments.insert("generation".to_owned(), serialize_json(generation)?);
    }
    explain_arguments.insert("symbol_ids".to_owned(), serialize_json(&seed_symbols)?);

    let explain_output =
        execute_symbol_explain(port, explain_arguments, cancellation, unsupported).await?;
    let explain_envelope: ReadEnvelope<SymbolExplainData> =
        serde_json::from_value(Value::Object(explain_output))
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;

    let mut definitions: BTreeMap<String, SourceRef> = BTreeMap::new();
    let mut candidates: Vec<PackEvidenceCandidate> = Vec::new();
    for explanation in &explain_envelope.data.symbols {
        definitions.insert(
            explanation.symbol_id.to_string(),
            explanation.definition.clone(),
        );
        let signature_bytes = explanation.signature.as_ref().map_or(0, String::len);
        let estimated_tokens =
            u32::try_from((signature_bytes + explanation.display_name.len()).div_ceil(4))
                .unwrap_or(u32::MAX);
        candidates.push(PackEvidenceCandidate {
            identity: explanation.symbol_id.to_string(),
            role: PackEvidenceRole::Definition,
            relevance: explanation.confidence,
            confidence: explanation.confidence,
            estimated_tokens,
            source_path: explanation.definition.span().file().to_string(),
        });
    }

    let objective = objective_for_task(&input.task);
    let pack = optimize_pack(objective, &mut candidates, u32::from(input.token_budget))
        .map_err(|_| ToolExecutionError::new(unsupported.clone()))?;

    let data = context_pack_data(&input, &pack, &definitions, &explain_envelope);
    let envelope = ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: explain_envelope.repository.clone(),
        generation: explain_envelope.generation.clone(),
        coverage: explain_envelope.coverage.clone(),
        data,
        truncated: pack.truncated,
        next_cursor: RequiredNullable(None),
        usage: explain_envelope.usage.clone(),
        warnings: explain_envelope.warnings.clone(),
        trust: TrustClassification::UntrustedRepositoryData,
    };
    serialize_success(envelope)
}

fn serialize_json<T: Serialize>(value: &T) -> Result<Value, ToolExecutionError> {
    serde_json::to_value(value).map_err(|_| internal(ToolExecutionFailure::Executor))
}

/// Classifies a free-text task objective into the pack planner's objective set.
///
/// The task is user-supplied guidance, not repository content, so keyword
/// classification stays source-free.
fn objective_for_task(task: &str) -> PackObjective {
    let task = task.to_lowercase();
    if task.contains("fix")
        || task.contains("bug")
        || task.contains("error")
        || task.contains("crash")
        || task.contains("broken")
    {
        PackObjective::BugFix
    } else if task.contains("refactor")
        || task.contains("restructure")
        || task.contains("simplify")
        || task.contains("clean")
    {
        PackObjective::Refactor
    } else if task.contains("migrat")
        || task.contains("upgrade")
        || task.contains("port to")
        || task.contains("move to")
    {
        PackObjective::Migration
    } else if task.contains("review") || task.contains("audit") || task.contains("security") {
        PackObjective::Review
    } else {
        PackObjective::Explanation
    }
}

fn context_pack_data(
    input: &ContextPackInput,
    pack: &PackResult,
    definitions: &BTreeMap<String, SourceRef>,
    explain_envelope: &ReadEnvelope<SymbolExplainData>,
) -> ContextPackData {
    let items: Vec<ContextItem> = pack
        .items
        .iter()
        .map(|item| ContextItem {
            role: context_role(item.candidate.role),
            symbol_id: item.candidate.identity.parse::<SymbolId>().ok(),
            source_ref: definitions.get(&item.candidate.identity).cloned(),
            score: item.candidate.relevance,
            tokens: item.candidate.estimated_tokens,
            trust: TrustClassification::UntrustedRepositoryData,
            snippet: None,
        })
        .collect();

    let reading_order: Vec<SourceFreeMessage> = pack
        .items
        .iter()
        .filter_map(|item| {
            SourceFreeMessage::parse(&format!(
                "review {}",
                context_role_label(context_role(item.candidate.role))
            ))
            .ok()
        })
        .collect();
    let structure = ContextStructure {
        reading_order,
        dependencies: Vec::new(),
    };

    let omitted: Vec<OmissionSummary> = pack
        .omissions
        .iter()
        .filter_map(|omission| {
            Some(OmissionSummary {
                reason: SafeLabel::parse(context_role_label(context_role(omission.role))).ok()?,
                count: u32::try_from(omission.count).unwrap_or(u32::MAX),
                continuation: None,
            })
        })
        .collect();

    let mut followups: Vec<ToolSuggestion> = Vec::new();
    if let Ok(reason) =
        SourceFreeMessage::parse("expand callers and callees for the target symbols")
    {
        followups.push(ToolSuggestion {
            tool: "symbol.relationships".to_owned(),
            reason,
            continuation: None,
        });
    }
    if !items.is_empty()
        && let Ok(reason) =
            SourceFreeMessage::parse("read full definitions for the included evidence")
    {
        followups.push(ToolSuggestion {
            tool: "source.read".to_owned(),
            reason,
            continuation: None,
        });
    }

    ContextPackData {
        pack_id: pack_id(input, explain_envelope),
        items,
        structure,
        omitted,
        followups,
        token_accounting: TokenAccounting {
            estimated_total: pack.total_tokens,
            by_section: BTreeMap::new(),
        },
    }
}

/// Derives a deterministic pack identity from the pinned generation, task, and
/// seeds so an identical request yields the same pack identifier.
fn pack_id(input: &ContextPackInput, envelope: &ReadEnvelope<SymbolExplainData>) -> ContextPackId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(envelope.generation.generation_id.to_string().as_bytes());
    hasher.update(b"\x00");
    hasher.update(input.task.as_bytes());
    hasher.update(b"\x00");
    if let Some(symbols) = &input.seeds.symbols {
        for symbol in symbols {
            hasher.update(symbol.to_string().as_bytes());
            hasher.update(b",");
        }
    }
    hasher.update(b"\x00planner-v1");
    let hex = hasher.finalize().to_hex();
    let short: String = hex.chars().take(32).collect();
    ContextPackId::new(format!("pack1_{short}"))
}

const fn context_role(role: PackEvidenceRole) -> ContextEvidenceRole {
    match role {
        PackEvidenceRole::Definition => ContextEvidenceRole::Definition,
        PackEvidenceRole::Implementation => ContextEvidenceRole::Implementation,
        PackEvidenceRole::Caller => ContextEvidenceRole::Caller,
        PackEvidenceRole::Test => ContextEvidenceRole::Test,
        PackEvidenceRole::Risk => ContextEvidenceRole::Risk,
        PackEvidenceRole::Architecture => ContextEvidenceRole::Architecture,
        PackEvidenceRole::Change => ContextEvidenceRole::Change,
    }
}

const fn context_role_label(role: ContextEvidenceRole) -> &'static str {
    match role {
        ContextEvidenceRole::Definition => "definition",
        ContextEvidenceRole::Implementation => "implementation",
        ContextEvidenceRole::Caller => "caller",
        ContextEvidenceRole::Test => "test",
        ContextEvidenceRole::Risk => "risk",
        ContextEvidenceRole::Architecture => "architecture",
        ContextEvidenceRole::Change => "change",
    }
}

/// Lists the repositories known to the daemon process.
///
/// The envelope identity is borrowed from the first listed repository; when no
/// repository is registered the tool fails with a checked not-found error
/// rather than fabricating an identity.
async fn execute_repo_list<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    invalid_cursor: &PublicError,
    cursor_key: [u8; 32],
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: RepoListInput = decode_input(arguments)?;
    // State filtering and non-compact response profiles are not served by this
    // slice; each is rejected by name rather than silently ignored.
    if input.states.is_some() {
        return Err(unsupported_field("states"));
    }
    if !compact_profile(input.response_profile) {
        return Err(unsupported_field("response_profile"));
    }
    let page_size = input.max_results.unwrap_or(DEFAULT_REPO_LIST_RESULTS);
    let context = repo_list_cursor_context(input.query.as_deref(), page_size);
    let offset = match &input.cursor {
        Some(cursor) => decode_repo_list_cursor(cursor, &context, &cursor_key, invalid_cursor)?,
        None => 0,
    };

    // The daemon returns the full bounded list; the bridge applies the
    // authenticated page window so the continuation cursor is tamper-protected.
    let request = RepositoryListPortRequest::new(None, input.query);
    let future = port.repository_list(request, cancellation.clone());
    let list = await_port(future, cancellation).await?;

    let Some(first) = list.repositories.first() else {
        return Err(ToolExecutionError::new(no_repositories_error()?));
    };

    let total = list.repositories.len();
    let start = offset.min(total);
    let page_end = start.saturating_add(usize::from(page_size)).min(total);
    let truncated = page_end < total;
    let repositories: Vec<RepositoryEntry> = list
        .repositories
        .get(start..page_end)
        .unwrap_or(&[])
        .iter()
        .map(|entry| RepositoryEntry {
            repository_id: entry.repository_id,
            display_name: entry.repository_id.to_string(),
            state: repository_state(&entry.state),
            active_generation: RequiredNullable(Some(entry.active_generation)),
            generation_count: 1,
            alias: RequiredNullable(None),
        })
        .collect();
    let total_count = u64::try_from(total).unwrap_or(u64::MAX);
    let data = RepoListData {
        repositories,
        total_count,
    };
    let next_cursor = if truncated {
        let next = AuthenticatedCursor::create(
            context,
            u32::try_from(page_end)
                .unwrap_or(u32::MAX)
                .to_le_bytes()
                .to_vec(),
            now_unix_ms(),
            &cursor_key,
        );
        RequiredNullable(Some(
            ContinuationCursor::parse(&next.to_wire())
                .map_err(|_| internal(ToolExecutionFailure::Executor))?,
        ))
    } else {
        RequiredNullable(None)
    };
    let envelope = ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: ResolvedRepository {
            repository_id: first.repository_id,
            display_name: first.repository_id.to_string(),
        },
        generation: GenerationSummary {
            generation_id: first.active_generation,
            parent_generation: RequiredNullable(None),
            structural_freshness: freshness_from_label(&first.structural_freshness),
            semantic_freshness: freshness_from_label(&first.semantic_freshness),
        },
        coverage: CoverageSummary {
            status: rootlight_ir::CoverageStatus::Complete,
            languages: Vec::new(),
            skipped_inputs: 0,
        },
        data,
        truncated,
        next_cursor,
        usage: UsageSummary {
            rows: total_count,
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 0,
            cache_status: CacheStatus::Miss,
            trace_id: "repo-list".to_owned(),
        },
        warnings: Vec::new(),
        trust: TrustClassification::UntrustedRepositoryData,
    };
    serialize_success(envelope)
}

/// Default page size for `repo.list`.
const DEFAULT_REPO_LIST_RESULTS: u16 = 20;

/// Builds the list-level cursor context for `repo.list`.
///
/// `repo.list` is not generation-bound, so the cursor binds to a documented
/// list-level sentinel identity and instead authenticates the request shape
/// (the query filter) and the page offset.
fn repo_list_cursor_context(query: Option<&str>, page_size: u16) -> CursorContext {
    CursorContext {
        repository: RepositoryId::from_bytes([0; 16]),
        generation: GenerationId::from_bytes([0; 20]),
        tool_name: "repo.list",
        query_fingerprint: repo_list_fingerprint(query),
        page_size,
    }
}

fn repo_list_fingerprint(query: Option<&str>) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"repo.list");
    hasher.update(query.unwrap_or("").as_bytes());
    *hasher.finalize().as_bytes()
}

fn decode_repo_list_cursor(
    cursor: &ContinuationCursor,
    context: &CursorContext,
    cursor_key: &[u8; 32],
    invalid_cursor: &PublicError,
) -> Result<usize, ToolExecutionError> {
    let parsed = AuthenticatedCursor::from_wire(cursor.as_str())
        .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?;
    parsed
        .validate(context, now_unix_ms(), cursor_key)
        .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?;
    let bytes: [u8; 4] = parsed
        .last_sort_key()
        .try_into()
        .map_err(|_| ToolExecutionError::new(invalid_cursor.clone()))?;
    Ok(usize::try_from(u32::from_le_bytes(bytes)).unwrap_or(usize::MAX))
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Reads one repository's active generation, freshness, and coverage.
async fn execute_repo_status<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: RepoStatusInput = decode_input(arguments)?;
    let repository = repository_id(input.repository.clone(), unsupported)?;
    // Granular coverage, operation lists, freshness gates, custom budgets, and
    // non-compact profiles are not served by this slice; each is rejected by
    // name so a client can correct the request.
    if input.coverage_detail.is_some() {
        return Err(unsupported_field("coverage_detail"));
    }
    if input.require_freshness.is_some() {
        return Err(unsupported_field("require_freshness"));
    }
    if input.include_operations == Some(true) {
        return Err(unsupported_field("include_operations"));
    }
    if input.budget.is_some() {
        return Err(unsupported_field("budget"));
    }
    if !compact_profile(input.response_profile) {
        return Err(unsupported_field("response_profile"));
    }
    let request = RepositoryStatusPortRequest::new(repository, client_generation(input.generation));
    let future = port.repository_status(request, cancellation.clone());
    let status = await_port(future, cancellation).await?;

    let generation_summary = GenerationSummary {
        generation_id: status.active_generation,
        parent_generation: RequiredNullable(status.parent_generation),
        structural_freshness: freshness_from_label(&status.structural_freshness),
        semantic_freshness: freshness_from_label(&status.semantic_freshness),
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
    let data = RepoStatusData {
        repository_state: repository_state(&status.state),
        active_generation: RequiredNullable(Some(generation_summary.clone())),
        coverage: status_coverage_report(&status.coverage),
        operations: Vec::new(),
        recommended_actions: Vec::new(),
    };
    let envelope = ReadEnvelope {
        schema_version: SchemaVersion::V1_0,
        repository: ResolvedRepository {
            repository_id: status.repository_id,
            display_name: status.repository_id.to_string(),
        },
        generation: generation_summary,
        coverage: CoverageSummary {
            status: aggregate_coverage_status(&status.coverage),
            languages: summary_languages,
            skipped_inputs: 0,
        },
        data,
        truncated: false,
        next_cursor: RequiredNullable(None),
        usage: UsageSummary {
            rows: 1,
            edges: 0,
            source_bytes: 0,
            json_bytes: 0,
            estimated_tokens: 0,
            wall_time_ms: 0,
            cache_status: CacheStatus::Miss,
            trace_id: "repo-status".to_owned(),
        },
        warnings: Vec::new(),
        trust: TrustClassification::UntrustedRepositoryData,
    };
    serialize_success(envelope)
}

fn no_repositories_error() -> Result<PublicError, ToolExecutionError> {
    PublicError::builder(ErrorCode::NotFound, "no repositories are registered")
        .build()
        .map_err(|_| internal(ToolExecutionFailure::Executor))
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
    if entries.iter().all(|entry| entry.status == "complete") {
        rootlight_ir::CoverageStatus::Complete
    } else {
        rootlight_ir::CoverageStatus::Bounded
    }
}

fn repository_state(label: &str) -> RepositoryState {
    match label {
        "ready" => RepositoryState::Ready,
        "indexing" => RepositoryState::Indexing,
        "degraded" => RepositoryState::Degraded,
        "corrupt" => RepositoryState::Corrupt,
        "migration_required" => RepositoryState::MigrationRequired,
        "rebuild_required" => RepositoryState::RebuildRequired,
        _ => RepositoryState::Degraded,
    }
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
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: CodeLocateInput = decode_input(arguments)?;
    let explain_only = input.explain == Some(true);
    let request = normalize_code_locate(input, unsupported)?;
    if explain_only {
        let output = explain_code_locate(port, request, cancellation).await?;
        return serialize_success(output);
    }
    let expected = request.clone();
    let future = port.code_locate(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_code_locate(response, &expected)?;
    serialize_success(output)
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
    let explanation = rootlight_agent::explain::code_locate_plan(
        matches!(request.mode, LocateMode::Exact),
        request.maximum_results,
    );
    let languages: Vec<LanguageCoverage> = status
        .coverage
        .iter()
        .map(|entry| LanguageCoverage {
            language: entry.language.clone(),
            tier: analysis_tier(&entry.tier),
            status: coverage_status_from_label(&entry.status),
        })
        .collect();
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
    map_read_envelope(context, metadata, data, false)
}

async fn execute_symbol_explain<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: SymbolExplainInput = decode_input(arguments)?;
    let request = normalize_symbol_explain(input, unsupported)?;
    let expected = request.clone();
    let future = port.symbol_explain(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_symbol_explain(response, &expected)?;
    serialize_success(output)
}

async fn execute_symbol_relationships<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: SymbolRelationshipsInput = decode_input(arguments)?;
    let request = normalize_symbol_relationships(input, unsupported)?;
    let expected = request.clone();
    let future = port.symbol_relationships(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_symbol_relationships(response, &expected)?;
    serialize_success(output)
}

fn normalize_symbol_relationships(
    input: SymbolRelationshipsInput,
    unsupported: &PublicError,
) -> Result<SymbolRelationshipsPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, ambiguous candidates, paging, and custom budgets are not
    // served by this slice.
    if input.scope.is_some()
        || input.include_candidates == Some(true)
        || input.budget.is_some()
        || input.cursor.is_some()
        || !compact_profile(input.response_profile)
    {
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
    })
}

fn map_symbol_relationships(
    response: SymbolRelationshipsPortResponse,
    request: &SymbolRelationshipsPortRequest,
) -> Result<ReadEnvelope<SymbolRelationshipsData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
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
    let returned_edges = u32::try_from(response.result.returned_edges)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let total_edges = u32::try_from(response.result.total_edges)
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?;
    let data = SymbolRelationshipsData {
        groups,
        unresolved: Vec::new(),
        totals: RelationshipTotals {
            returned_edges,
            total_edges,
            exact: response.result.exact,
        },
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.truncated,
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

async fn execute_flow_trace<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: FlowTraceInput = decode_input(arguments)?;
    let request = normalize_flow_trace(input, unsupported)?;
    let expected = request.clone();
    let future = port.flow_trace(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_flow_trace(response, &expected)?;
    serialize_success(output)
}

fn normalize_flow_trace(
    input: FlowTraceInput,
    unsupported: &PublicError,
) -> Result<FlowTracePortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Cross-repository traversal, explicit path policies, custom budgets, and
    // non-compact profiles are not served by this slice.
    if input.cross_repository == Some(true)
        || input.path_policy.is_some()
        || input.budget.is_some()
        || !compact_profile(input.response_profile)
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
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        frontier.truncated,
    )
}

async fn execute_architecture_cycles<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: ArchitectureCyclesInput = decode_input(arguments)?;
    let request = normalize_architecture_cycles(input, unsupported)?;
    let expected = request.clone();
    let future = port.architecture_cycles(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_architecture_cycles(response, &expected)?;
    serialize_success(output)
}

fn normalize_architecture_cycles(
    input: ArchitectureCyclesInput,
    unsupported: &PublicError,
) -> Result<ArchitectureCyclesPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, ranking strategies, custom budgets, and non-compact
    // profiles are not served by this slice. The projection level is accepted
    // as a descriptive label; detection runs at symbol granularity.
    if input.scope.is_some()
        || input.rank_by.is_some()
        || input.budget.is_some()
        || !compact_profile(input.response_profile)
    {
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
    };
    // The requested cycle cap is an explicit bound honored by the daemon; this
    // slice does not surface separate budget-truncation through the wire.
    map_read_envelope(response.result.context, response.metadata, data, false)
}

async fn execute_code_dead<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: CodeDeadInput = decode_input(arguments)?;
    let request = normalize_code_dead(input, unsupported)?;
    let expected = request.clone();
    let future = port.code_dead(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_code_dead(response, &expected)?;
    serialize_success(output)
}

fn normalize_code_dead(
    input: CodeDeadInput,
    unsupported: &PublicError,
) -> Result<CodeDeadPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, custom budgets, and non-compact profiles are not served
    // by this slice.
    if input.scope.is_some() || input.budget.is_some() || !compact_profile(input.response_profile) {
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
    };
    // The requested candidate cap is an explicit bound honored by the daemon;
    // this slice does not surface separate budget-truncation through the wire.
    map_read_envelope(response.result.context, response.metadata, data, false)
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

async fn execute_architecture_overview<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: ArchitectureOverviewInput = decode_input(arguments)?;
    let request = normalize_architecture_overview(input, unsupported)?;
    let expected = request.clone();
    let future = port.architecture_overview(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_architecture_overview(response, &expected)?;
    serialize_success(output)
}

fn normalize_architecture_overview(
    input: ArchitectureOverviewInput,
    unsupported: &PublicError,
) -> Result<ArchitectureOverviewPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Structural scope, explicit detail levels, custom budgets, and non-compact
    // profiles are not served by this slice. The base file-granularity model is
    // always returned; only the hotspot derived view is honored.
    if input.scope.is_some()
        || input.detail.is_some()
        || input.budget.is_some()
        || !compact_profile(input.response_profile)
    {
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
    };
    // The requested component cap is an explicit bound honored by the daemon;
    // this slice does not surface separate budget-truncation through the wire.
    map_read_envelope(response.result.context, response.metadata, data, false)
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

async fn execute_tests_select<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: TestsSelectInput = decode_input(arguments)?;
    let request = normalize_tests_select(input, unsupported)?;
    let expected = request.clone();
    let future = port.tests_select(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_tests_select(response, &expected)?;
    serialize_success(output)
}

fn normalize_tests_select(
    input: TestsSelectInput,
    unsupported: &PublicError,
) -> Result<TestsSelectPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Custom budgets, execution budgets, framework filters, and non-compact
    // profiles are not served by this slice.
    if input.budget.is_some()
        || input.execution_budget.is_some()
        || input.frameworks.is_some()
        || !compact_profile(input.profile)
    {
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
    };
    // The requested test cap is an explicit bound honored by the daemon; this
    // slice does not surface separate budget-truncation through the wire.
    map_read_envelope(response.result.context, response.metadata, data, false)
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

async fn execute_change_impact<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: ChangeImpactInput = decode_input(arguments)?;
    let request = normalize_change_impact(input, unsupported)?;
    let expected = request.clone();
    let future = port.change_impact(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_change_impact(response, &expected)?;
    serialize_success(output)
}

fn normalize_change_impact(
    input: ChangeImpactInput,
    unsupported: &PublicError,
) -> Result<ChangeImpactPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Scope bounding, custom budgets, and non-compact profiles are not served by
    // this slice.
    if input.scope.is_some() || input.budget.is_some() || !compact_profile(input.profile) {
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
    };
    map_read_envelope(response.result.context, response.metadata, data, false)
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

async fn execute_plan_change<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: PlanChangeInput = decode_input(arguments)?;
    let request = normalize_plan_change(input, unsupported)?;
    let expected = request.clone();
    let future = port.plan_change(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_plan_change(response, &expected)?;
    serialize_success(output)
}

fn normalize_plan_change(
    input: PlanChangeInput,
    unsupported: &PublicError,
) -> Result<PlanChangePortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Change context, user constraints, custom budgets, and non-compact profiles
    // are not served by this slice.
    if input.change_context.is_some()
        || input.constraints.is_some()
        || input.budget.is_some()
        || !compact_profile(input.profile)
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    let mut target_symbols = Vec::new();
    let mut target_files = Vec::new();
    for target in input.targets {
        match target {
            PlanTargetSelector::Symbol(symbol) => target_symbols.push(symbol.symbol_id),
            PlanTargetSelector::File(file) => target_files.push(file.file_id),
        }
    }
    // An empty target set carries no resolvable target.
    if target_symbols.is_empty() && target_files.is_empty() {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    Ok(PlanChangePortRequest {
        repository,
        generation: client_generation(input.generation),
        objective: plan_objective_label(input.objective).to_owned(),
        objective_text: input.objective_text,
        target_symbols,
        target_files,
        max_steps: input.max_steps,
    })
}

/// Returns the stable wire label for one typed plan objective.
const fn plan_objective_label(objective: PlanObjective) -> &'static str {
    match objective {
        PlanObjective::BugFix => "bug_fix",
        PlanObjective::Refactor => "refactor",
        PlanObjective::Explanation => "explanation",
        PlanObjective::Migration => "migration",
        PlanObjective::Review => "review",
    }
}

fn map_plan_change(
    response: PlanChangePortResponse,
    request: &PlanChangePortRequest,
) -> Result<ReadEnvelope<PlanChangeData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
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
    let data = PlanChangeData {
        plan,
        affected_scope: PlanImpactSummary {
            affected_symbols: scope.affected_symbols,
            affected_files: scope.affected_files,
            risk_level: risk_level_from_label(&scope.risk_level)?,
            touches_public_surface: scope.touches_public_surface,
        },
        test_plan,
        open_decisions,
        context_pack_request: ContextPackRequest {
            symbols: pack.symbols,
            files: pack.files,
        },
    };
    map_read_envelope(response.result.context, response.metadata, data, false)
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
    let request = normalize_history_compare(input, unsupported)?;
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
    // Scope bounding, unchanged-context inclusion, custom budgets, and
    // non-compact profiles are not served by this slice.
    if input.scope.is_some()
        || input.include_unchanged_context == Some(true)
        || input.budget.is_some()
        || !compact_profile(input.profile)
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
    };
    map_read_envelope(response.result.context, response.metadata, data, false)
}

fn semantic_change_kind_from_label(label: &str) -> Result<SemanticChangeKind, ToolExecutionError> {
    serde_json::from_value(Value::String(label.to_owned()))
        .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))
}

async fn execute_query_advanced<P>(
    port: Arc<P>,
    arguments: Map<String, Value>,
    cancellation: RequestCancellation,
    unsupported: &PublicError,
) -> Result<Map<String, Value>, ToolExecutionError>
where
    P: FirstSliceClientPort,
{
    let input: QueryAdvancedInput = decode_input(arguments)?;
    let request = normalize_query_advanced(input, unsupported)?;
    let expected = request.clone();
    let future = port.query_advanced(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_query_advanced(response, &expected)?;
    serialize_success(output)
}

fn normalize_query_advanced(
    input: QueryAdvancedInput,
    unsupported: &PublicError,
) -> Result<QueryAdvancedPortRequest, ToolExecutionError> {
    let repository = repository_id(input.repository, unsupported)?;
    // Paging cursors and bound parameters are not served by this slice.
    if input.cursor.is_some()
        || input
            .parameters
            .as_ref()
            .is_some_and(|parameters| !parameters.is_empty())
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
    }
    // Derive the operator sequence and nesting depth, then validate the static
    // plan against the resource ceilings before crossing the daemon boundary.
    let mut operators = Vec::new();
    let depth = derive_query_operators(&input.query, &mut operators);
    let max_rows = usize::from(input.max_results.unwrap_or(DEFAULT_ADVANCED_RESULTS));
    let plan = AdvancedQueryPlan::validate(&operators, max_rows, MAX_ADVANCED_TRAVERSAL, depth)
        .map_err(|_| ToolExecutionError::new(unsupported.clone()))?;
    // Honor the optional client cost ceiling against the static estimate.
    if input
        .cost_limit
        .is_some_and(|limit| plan.estimated_cost > limit)
    {
        return Err(ToolExecutionError::new(unsupported.clone()));
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
    })
}

/// Walks a safe typed AST, appending each operator innermost-first and
/// returning the nesting depth.
fn derive_query_operators(node: &QueryAstNode, operators: &mut Vec<QueryOperator>) -> usize {
    match node {
        QueryAstNode::Scan { .. } => {
            operators.push(QueryOperator::Scan);
            1
        }
        QueryAstNode::Filter { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Filter);
            depth + 1
        }
        QueryAstNode::Project { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Project);
            depth + 1
        }
        QueryAstNode::Join { left, right, .. } => {
            let left_depth = derive_query_operators(left, operators);
            let right_depth = derive_query_operators(right, operators);
            operators.push(QueryOperator::Join);
            left_depth.max(right_depth) + 1
        }
        QueryAstNode::Aggregate { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Aggregate);
            depth + 1
        }
        QueryAstNode::Traverse { .. } => {
            operators.push(QueryOperator::Traverse);
            1
        }
        QueryAstNode::Sort { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Sort);
            depth + 1
        }
        QueryAstNode::Limit { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Limit);
            depth + 1
        }
    }
}

fn map_query_advanced(
    response: QueryAdvancedPortResponse,
    request: &QueryAdvancedPortRequest,
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
    if response.result.rows.len() > 1_000 {
        return Err(internal(ToolExecutionFailure::InvalidResponse));
    }
    let rows = response.result.rows;
    let plan = match response.result.plan {
        Some(plan) => {
            if plan.estimated_cost > 10_000_000
                || plan.operators.len() > 64
                || plan.applied_limits.len() > 16
            {
                return Err(internal(ToolExecutionFailure::InvalidResponse));
            }
            RequiredNullable(Some(PlanExplanation {
                estimated_cost: plan.estimated_cost,
                operators: plan.operators,
                applied_limits: plan.applied_limits,
            }))
        }
        None => RequiredNullable(None),
    };
    let completeness = query_completeness_from_label(&response.result.completeness)?;
    let truncated = matches!(completeness, QueryCompleteness::Truncated);
    let data = QueryAdvancedData {
        columns,
        rows,
        plan,
        completeness,
    };
    map_read_envelope(response.result.context, response.metadata, data, truncated)
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
    let request = normalize_source_read(input, unsupported, invalid_arguments)?;
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
    serde_json::from_value(Value::Object(arguments)).map_err(|_| invalid_input())
}

/// Builds the client-correctable error for malformed tool arguments.
///
/// Argument decoding failures are caller errors, not internal failures, so they
/// are reported as invalid arguments with a stable correct-field action rather
/// than collapsed into an opaque internal error.
fn invalid_input() -> ToolExecutionError {
    let field = DetailKey::parse("arguments").expect("static detail key is valid");
    let error = PublicError::builder(ErrorCode::InvalidArgument, INVALID_ARGUMENT_MESSAGE)
        .next_action(NextAction::CorrectField { field })
        .build()
        .expect("static invalid-argument template is valid");
    ToolExecutionError::new(error)
}

/// Builds the pre-execution error for a schema-valid field this slice does not
/// serve, naming the offending field so a client can correct the request
/// instead of seeing a generic arguments-level rejection.
fn unsupported_field(field: &'static str) -> ToolExecutionError {
    let field = DetailKey::parse(field).expect("static field name is valid");
    let error = PublicError::builder(ErrorCode::UnsupportedCapability, UNSUPPORTED_MESSAGE)
        .next_action(NextAction::CorrectField { field })
        .build()
        .expect("static unsupported-field template is valid");
    ToolExecutionError::new(error)
}
fn normalize_repository_index(
    input: RepoIndexInput,
    unsupported: &PublicError,
    invalid_arguments: &PublicError,
) -> Result<RepositoryIndexPortRequest, ToolExecutionError> {
    if input.repository_id.is_some()
        || !matches!(input.scope, None | Some(IndexScope::Repository(_)))
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
        detached: input.detached.unwrap_or(false),
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
        || input.cursor.is_some()
        || !compact_profile(input.response_profile)
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
        || !compact_profile(input.response_profile)
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
    if !matches!(
        (input.context_lines_before, input.context_lines_after),
        (None, None)
            | (
                Some(CURRENT_SOURCE_CONTEXT_LINES),
                Some(CURRENT_SOURCE_CONTEXT_LINES)
            )
    ) || input.merge_overlaps == Some(true)
        || input.max_source_bytes.is_some()
        || input.include_line_numbers == Some(false)
        || matches!(input.encoding, Some(SourceEncodingRequest::BytesBase64))
        || input.budget.is_some()
        || !compact_profile(input.response_profile)
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

const fn compact_profile(profile: Option<ResponseProfile>) -> bool {
    matches!(profile, None | Some(ResponseProfile::Compact))
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
) -> Result<ReadEnvelope<CodeLocateData>, ToolExecutionError> {
    validate_query_context(
        &response.result.context,
        request.repository,
        request.generation,
    )?;
    if response.result.hits.len()
        > usize::try_from(request.maximum_results)
            .map_err(|_| internal(ToolExecutionFailure::InvalidResponse))?
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
        response.result.truncated,
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
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.truncated,
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
    };
    map_read_envelope(
        response.result.context,
        response.metadata,
        data,
        response.result.truncated,
    )
}

fn map_read_envelope<T>(
    context: client::QueryContext,
    mut metadata: ReadResponseMetadata,
    data: T,
    truncated: bool,
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
    metadata.warnings.sort_by(|left, right| {
        left.code
            .as_str()
            .cmp(right.code.as_str())
            .then_with(|| left.message.as_str().cmp(right.message.as_str()))
    });
    let wall_time_ms = context.usage.elapsed_micros.div_ceil(1_000);
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
            status: coverage_status(context.coverage_status),
            languages: metadata.languages,
            skipped_inputs: context.skipped_inputs,
        },
        data,
        truncated,
        next_cursor: RequiredNullable(None),
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
