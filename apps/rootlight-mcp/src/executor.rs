//! Typed production mapping between the MCP tool catalog and a daemon client port.
//!
//! The port supplies facts absent from the current client DTOs so this layer
//! never fabricates index-plan, freshness, coverage, cache, or trace metadata.

use std::{collections::BTreeSet, fmt, future::Future, pin::Pin, sync::Arc};

use rootlight_client::{
    self as client, CodeLocate, LocateMode, RepositoryIndex, RepositoryOperationAction,
    RepositoryOperationStatus, SourceRead, SymbolExplain,
};
use rootlight_ids::{OperationId, RepositoryId, SymbolId};
use rootlight_ir::{LineRange, SourceRef, SourceSpan};
use rootlight_mcp_contract::{
    DetailKey, ErrorCode, GenerationSelector, NextAction, PublicError, PublicErrorBuildError,
    RepoIndexInput, RepositorySelector, SchemaVersion, SourceReadInput, SymbolExplainInput,
    ToolResponse, TrustClassification, VerticalTool,
    vertical::{
        ActiveGeneration, CacheStatus, CodeLocateData, CodeLocateInput, CoverageSummary,
        DetailHandle, Diagnostic, EntityKind, Freshness, GenerationSummary, IndexMode,
        IndexPlanScope, IndexPlanSummary, IndexScope, LanguageCoverage, LocateReason, LocatedItem,
        OperationAction, OperationDetail, OperationProgress, OperationResources, OperationState,
        OperationStatusData, OperationStatusInput, OperationStatusSuccess, ProvenanceLevel,
        ProvenanceSummary, QueryInterpretation, ReadEnvelope, RepoIndexData, RepoIndexSuccess,
        RequiredNullable, ResponseBudget, ResponseProfile, ResponseWarning, SearchMode,
        SourceChunk, SourceElision, SourceEncoding, SourceEncodingRequest, SourceReadData,
        SourceReadSelector, StaleSourceReference, SymbolExplainData, SymbolExplanation,
        UsageSummary,
    },
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};
use thiserror::Error;

use crate::{
    RequestCancellation, ToolExecutionError, ToolExecutionFailure, ToolExecutionFuture,
    ToolExecutor,
};

const DEFAULT_LOCATE_RESULTS: u16 = 20;
const CURRENT_SOURCE_CONTEXT_LINES: u8 = 2;
const INVALID_ARGUMENT_MESSAGE: &str = "tool arguments are invalid";
const UNSUPPORTED_MESSAGE: &str = "requested option is not supported";

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
}

/// Production MCP executor over an injected asynchronous daemon-client port.
pub struct FirstSliceToolExecutor<P> {
    port: Arc<P>,
    invalid_arguments: PublicError,
    unsupported: PublicError,
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
    /// cannot be represented by the shared public error contract.
    pub fn new(port: P) -> Result<Self, ToolExecutorBuildError> {
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
        Ok(Self {
            port: Arc::new(port),
            invalid_arguments,
            unsupported,
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
                | VerticalTool::QueryBatch => {
                    Err(ToolExecutionError::new(unsupported.clone()))
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
    let request = normalize_code_locate(input, unsupported)?;
    let expected = request.clone();
    let future = port.code_locate(request, cancellation.clone());
    let response = await_port(future, cancellation).await?;
    let output = map_code_locate(response, &expected)?;
    serialize_success(output)
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
    serde_json::from_value(Value::Object(arguments))
        .map_err(|_| internal(ToolExecutionFailure::Executor))
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
