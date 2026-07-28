//! Native asynchronous daemon-client port for the production MCP executor.
//!
//! This boundary enriches checked client DTOs only with facts Rootlight can
//! prove locally; unavailable startup remains a source-free transport failure.

use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use rootlight_client::{
    AdvancedQuery, AnalysisTier as ClientAnalysisTier, ArchitectureCycles, ArchitectureOverview,
    ChangeImpact, Client, ClientError, CodeDead, CodeLocate, CoverageStatus, FlowTrace,
    GenerationSelector, HistoryCompare, LocateMode, PlanChange, RepositoryCatalogPage,
    RepositoryCatalogPageRequest, RepositoryIndex,
    RepositoryIndexMode as ClientRepositoryIndexMode, RepositoryOperationAction,
    RepositoryOperationStatus, RepositoryStatus, RepositoryStatusRequest, RequestOptions,
    RequestTimeout, SourceEncoding as ClientSourceEncoding, SourceRead,
    SourceReadOptions as ClientSourceReadOptions, SourceReference, SymbolExplain,
    SymbolRelationships, TestsSelect,
};
use rootlight_ids::{FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::CoverageStatus as IrCoverageStatus;
use rootlight_mcp_contract::{
    SafeLabel,
    vertical::{
        AnalysisTier, CacheStatus, Diagnostic, Freshness, IndexMode, IndexPlanScope,
        IndexPlanSummary, LanguageCoverage, RequiredNullable, SourceFreeMessage,
    },
};

use crate::{
    ArchitectureCyclesPortRequest, ArchitectureCyclesPortResponse, ArchitectureOverviewPortRequest,
    ArchitectureOverviewPortResponse, ChangeImpactPortRequest, ChangeImpactPortResponse,
    ClientPortError, ClientPortFuture, CodeDeadPortRequest, CodeDeadPortResponse,
    CodeLocatePortRequest, CodeLocatePortResponse, FirstSliceClientPort, FlowTracePortRequest,
    FlowTracePortResponse, HistoryComparePortRequest, HistoryComparePortResponse,
    OperationStatusPortRequest, PlanChangePortRequest, PlanChangePortResponse,
    QueryAdvancedPortRequest, QueryAdvancedPortResponse, ReadResponseMetadata,
    RepositoryCatalogPagePortRequest, RepositoryIndexPortRequest, RepositoryIndexPortResponse,
    RepositoryStatusPortRequest, RequestCancellation, SourceReadPortRequest,
    SourceReadPortResponse, SymbolExplainPortRequest, SymbolExplainPortResponse,
    SymbolRelationshipsPortRequest, SymbolRelationshipsPortResponse, TestsSelectPortRequest,
    TestsSelectPortResponse,
};

const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_SLICE_PROVIDER: &str = "rootlight-first-slice-treesitter";
const PROJECT_SEMANTICS_PROVIDER: &str = "rootlight-project-semantics";
const FIRST_SLICE_LANGUAGE: &str = "rust";
const BRIDGE_TRACE_PREFIX: &str = "bridge-";

type AsyncClientFuture<T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'static>>;

trait AsyncFirstSliceClient: Send + Sync + 'static {
    fn repository_index(
        &self,
        root: String,
        operation: OperationId,
        detached: bool,
        mode: ClientRepositoryIndexMode,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryIndex>;

    fn operation_status(
        &self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryOperationStatus>;

    #[expect(
        clippy::too_many_arguments,
        reason = "the client boundary carries each bounded lookup dimension explicitly"
    )]
    fn code_locate(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query: String,
        mode: LocateMode,
        maximum_results: u32,
        page_offset: u64,
        options: RequestOptions,
    ) -> AsyncClientFuture<CodeLocate>;

    fn symbol_explain(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: Vec<SymbolId>,
        options: RequestOptions,
    ) -> AsyncClientFuture<SymbolExplain>;

    fn source_read(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: Vec<SourceReference>,
        projection: ClientSourceReadOptions,
        options: RequestOptions,
    ) -> AsyncClientFuture<SourceRead>;

    fn repository_catalog_page(
        &self,
        request: RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryCatalogPage>;

    fn repository_status(
        &self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryStatus>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded relationships query dimension"
    )]
    fn symbol_relationships(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: Vec<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        min_confidence: Option<u16>,
        max_results: Option<u16>,
        page_offset: u64,
        options: RequestOptions,
    ) -> AsyncClientFuture<SymbolRelationships>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded flow trace dimension"
    )]
    fn flow_trace(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        from: SymbolId,
        to: Option<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        max_depth: Option<u8>,
        max_paths: Option<u16>,
        min_confidence: Option<u16>,
        cross_repository: bool,
        options: RequestOptions,
    ) -> AsyncClientFuture<FlowTrace>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded architecture cycles dimension"
    )]
    fn architecture_cycles(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        relations: Vec<String>,
        min_size: Option<u8>,
        max_cycles: Option<u16>,
        include_self_cycles: Option<bool>,
        options: RequestOptions,
    ) -> AsyncClientFuture<ArchitectureCycles>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded code dead dimension"
    )]
    fn code_dead(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        entry_point_policy: Option<String>,
        include_exported: Option<bool>,
        include_tests: Option<bool>,
        min_confidence: Option<u16>,
        max_candidates: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<CodeDead>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded architecture overview dimension"
    )]
    fn architecture_overview(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        views: Vec<String>,
        max_components: Option<u16>,
        include_edges: Option<bool>,
        min_confidence: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<ArchitectureOverview>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded tests select dimension"
    )]
    fn tests_select(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: Vec<SymbolId>,
        test_kinds: Vec<String>,
        max_tests: Option<u16>,
        include_commands: Option<bool>,
        options: RequestOptions,
    ) -> AsyncClientFuture<TestsSelect>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded change impact dimension"
    )]
    fn change_impact(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        changed_symbols: Vec<SymbolId>,
        changed_paths: Vec<String>,
        max_depth: Option<u8>,
        min_confidence: Option<u16>,
        include_tests: Option<bool>,
        max_dependents: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<ChangeImpact>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded plan change dimension"
    )]
    fn plan_change(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        objective: String,
        objective_text: String,
        target_symbols: Vec<SymbolId>,
        target_files: Vec<FileId>,
        max_steps: Option<u8>,
        options: RequestOptions,
    ) -> AsyncClientFuture<PlanChange>;

    fn history_compare(
        &self,
        repository: RepositoryId,
        base: GenerationId,
        head: GenerationId,
        change_kinds: Vec<String>,
        max_results: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<HistoryCompare>;

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded advanced query dimension"
    )]
    fn query_advanced(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query_ast: String,
        explain: Option<bool>,
        max_results: Option<u16>,
        max_depth: Option<u8>,
        cost_limit: Option<u64>,
        page_offset: u64,
        options: RequestOptions,
    ) -> AsyncClientFuture<AdvancedQuery>;
}

type ClientConnector = dyn Fn() -> Result<Client, ClientError> + Send + Sync + 'static;

struct ClientProvider {
    client: OnceLock<Arc<Client>>,
    connector: Option<Arc<ClientConnector>>,
    initialization: Mutex<()>,
}

impl ClientProvider {
    fn ready(client: Client) -> Self {
        let ready = OnceLock::new();
        let initialized = ready.set(Arc::new(client));
        debug_assert!(initialized.is_ok());
        Self {
            client: ready,
            connector: None,
            initialization: Mutex::new(()),
        }
    }

    fn deferred(
        connector: impl Fn() -> Result<Client, ClientError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            client: OnceLock::new(),
            connector: Some(Arc::new(connector)),
            initialization: Mutex::new(()),
        }
    }

    async fn resolve(self: Arc<Self>) -> Result<Arc<Client>, ClientError> {
        if let Some(client) = self.client.get() {
            return Ok(Arc::clone(client));
        }
        tokio::task::spawn_blocking(move || self.resolve_blocking())
            .await
            .map_err(|_| ClientError::UnexpectedResponse)?
    }

    fn resolve_blocking(&self) -> Result<Arc<Client>, ClientError> {
        if let Some(client) = self.client.get() {
            return Ok(Arc::clone(client));
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| ClientError::UnexpectedResponse)?;
        if let Some(client) = self.client.get() {
            return Ok(Arc::clone(client));
        }
        let connector = self
            .connector
            .as_ref()
            .ok_or(ClientError::UnexpectedResponse)?;
        let client = Arc::new(connector()?);
        self.client
            .set(Arc::clone(&client))
            .map_err(|_| ClientError::UnexpectedResponse)?;
        Ok(client)
    }
}

struct LiveAsyncFirstSliceClient {
    client: Arc<ClientProvider>,
}

impl AsyncFirstSliceClient for LiveAsyncFirstSliceClient {
    fn repository_index(
        &self,
        root: String,
        operation: OperationId,
        detached: bool,
        mode: ClientRepositoryIndexMode,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryIndex> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .repository_index_async_with_mode(&root, operation, detached, mode, timeout)
                .await
        })
    }

    fn operation_status(
        &self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryOperationStatus> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .repository_operation_status_async(
                    operation,
                    action,
                    wait_ms,
                    after_revision,
                    timeout,
                )
                .await
        })
    }

    fn code_locate(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query: String,
        mode: LocateMode,
        maximum_results: u32,
        page_offset: u64,
        options: RequestOptions,
    ) -> AsyncClientFuture<CodeLocate> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .code_locate_async_with_options(
                    repository,
                    generation,
                    &query,
                    mode,
                    maximum_results,
                    page_offset,
                    options,
                )
                .await
        })
    }

    fn symbol_explain(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: Vec<SymbolId>,
        options: RequestOptions,
    ) -> AsyncClientFuture<SymbolExplain> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .symbol_explain_async_with_options(repository, generation, &symbols, options)
                .await
        })
    }

    fn source_read(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: Vec<SourceReference>,
        projection: ClientSourceReadOptions,
        options: RequestOptions,
    ) -> AsyncClientFuture<SourceRead> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .source_read_projected_async_with_options(
                    repository,
                    generation,
                    &references,
                    projection,
                    options,
                )
                .await
        })
    }

    fn repository_catalog_page(
        &self,
        request: RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryCatalogPage> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .repository_catalog_page_async(&request, timeout)
                .await
        })
    }

    fn repository_status(
        &self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryStatus> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .repository_status_with_options_async(request, timeout)
                .await
        })
    }

    fn symbol_relationships(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: Vec<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        min_confidence: Option<u16>,
        max_results: Option<u16>,
        page_offset: u64,
        options: RequestOptions,
    ) -> AsyncClientFuture<SymbolRelationships> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .symbol_relationships_async_with_options(
                    repository,
                    generation,
                    &seeds,
                    &relations,
                    direction.as_deref(),
                    min_confidence,
                    max_results,
                    page_offset,
                    options,
                )
                .await
        })
    }

    fn flow_trace(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        from: SymbolId,
        to: Option<SymbolId>,
        relations: Vec<String>,
        direction: Option<String>,
        max_depth: Option<u8>,
        max_paths: Option<u16>,
        min_confidence: Option<u16>,
        cross_repository: bool,
        options: RequestOptions,
    ) -> AsyncClientFuture<FlowTrace> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .flow_trace_async_with_options_cross_repository(
                    repository,
                    generation,
                    from,
                    to,
                    &relations,
                    direction.as_deref(),
                    max_depth,
                    max_paths,
                    min_confidence,
                    cross_repository,
                    options,
                )
                .await
        })
    }

    fn architecture_cycles(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        relations: Vec<String>,
        min_size: Option<u8>,
        max_cycles: Option<u16>,
        include_self_cycles: Option<bool>,
        options: RequestOptions,
    ) -> AsyncClientFuture<ArchitectureCycles> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .architecture_cycles_async_with_options(
                    repository,
                    generation,
                    &relations,
                    min_size,
                    max_cycles,
                    include_self_cycles,
                    options,
                )
                .await
        })
    }

    fn code_dead(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        entry_point_policy: Option<String>,
        include_exported: Option<bool>,
        include_tests: Option<bool>,
        min_confidence: Option<u16>,
        max_candidates: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<CodeDead> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .code_dead_async_with_options(
                    repository,
                    generation,
                    entry_point_policy.as_deref(),
                    include_exported,
                    include_tests,
                    min_confidence,
                    max_candidates,
                    options,
                )
                .await
        })
    }

    fn architecture_overview(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        views: Vec<String>,
        max_components: Option<u16>,
        include_edges: Option<bool>,
        min_confidence: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<ArchitectureOverview> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .architecture_overview_async_with_options(
                    repository,
                    generation,
                    &views,
                    max_components,
                    include_edges,
                    min_confidence,
                    options,
                )
                .await
        })
    }

    fn tests_select(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: Vec<SymbolId>,
        test_kinds: Vec<String>,
        max_tests: Option<u16>,
        include_commands: Option<bool>,
        options: RequestOptions,
    ) -> AsyncClientFuture<TestsSelect> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .tests_select_async_with_options(
                    repository,
                    generation,
                    &seeds,
                    &test_kinds,
                    max_tests,
                    include_commands,
                    options,
                )
                .await
        })
    }

    fn change_impact(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        changed_symbols: Vec<SymbolId>,
        changed_paths: Vec<String>,
        max_depth: Option<u8>,
        min_confidence: Option<u16>,
        include_tests: Option<bool>,
        max_dependents: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<ChangeImpact> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .change_impact_async_with_options(
                    repository,
                    generation,
                    &changed_symbols,
                    &changed_paths,
                    max_depth,
                    min_confidence,
                    include_tests,
                    max_dependents,
                    options,
                )
                .await
        })
    }

    fn plan_change(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        objective: String,
        objective_text: String,
        target_symbols: Vec<SymbolId>,
        target_files: Vec<FileId>,
        max_steps: Option<u8>,
        options: RequestOptions,
    ) -> AsyncClientFuture<PlanChange> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .plan_change_async_with_options(
                    repository,
                    generation,
                    &objective,
                    &objective_text,
                    &target_symbols,
                    &target_files,
                    max_steps,
                    options,
                )
                .await
        })
    }

    fn history_compare(
        &self,
        repository: RepositoryId,
        base: GenerationId,
        head: GenerationId,
        change_kinds: Vec<String>,
        max_results: Option<u16>,
        options: RequestOptions,
    ) -> AsyncClientFuture<HistoryCompare> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            let kind_labels: Vec<&str> = change_kinds.iter().map(String::as_str).collect();
            client
                .history_compare_async_with_options(
                    repository,
                    base,
                    head,
                    &kind_labels,
                    max_results,
                    options,
                )
                .await
        })
    }

    fn query_advanced(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query_ast: String,
        explain: Option<bool>,
        max_results: Option<u16>,
        max_depth: Option<u8>,
        cost_limit: Option<u64>,
        page_offset: u64,
        options: RequestOptions,
    ) -> AsyncClientFuture<AdvancedQuery> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let client = client.resolve().await?;
            client
                .advanced_query_async_with_options(
                    repository,
                    generation,
                    &query_ast,
                    explain,
                    max_results,
                    max_depth,
                    cost_limit,
                    page_offset,
                    options,
                )
                .await
        })
    }
}

/// Native asynchronous adapter from MCP's first-slice port to [`Client`].
///
/// Each call uses one native async client exchange. Dropping a pending port
/// future therefore closes that exchange without a blocking worker.
pub struct NativeFirstSliceClientPort {
    client: Arc<dyn AsyncFirstSliceClient>,
}

impl NativeFirstSliceClientPort {
    /// Creates a native port over one synchronously resolved daemon client.
    #[must_use]
    pub fn new(client: Client) -> Self {
        Self {
            client: Arc::new(LiveAsyncFirstSliceClient {
                client: Arc::new(ClientProvider::ready(client)),
            }),
        }
    }

    /// Creates a native port that resolves its daemon client on the first tool call.
    ///
    /// The connector is serialized and a successful client is reused. Failures
    /// are not cached, so a later tool call may recover from transient startup.
    #[must_use]
    pub fn connect_on_first_request(
        connector: impl Fn() -> Result<Client, ClientError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            client: Arc::new(LiveAsyncFirstSliceClient {
                client: Arc::new(ClientProvider::deferred(connector)),
            }),
        }
    }

    #[cfg(test)]
    fn with_client(client: impl AsyncFirstSliceClient) -> Self {
        Self {
            client: Arc::new(client),
        }
    }
}

impl fmt::Debug for NativeFirstSliceClientPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeFirstSliceClientPort")
            .finish_non_exhaustive()
    }
}

impl FirstSliceClientPort for NativeFirstSliceClientPort {
    fn repository_index(
        &self,
        request: RepositoryIndexPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryIndexPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let operation = random_operation_id()?;
            let timeout = request_timeout()?;
            let requested_mode = match request.mode() {
                IndexMode::Auto => ClientRepositoryIndexMode::Auto,
                IndexMode::Structural => ClientRepositoryIndexMode::Structural,
                IndexMode::Deep => ClientRepositoryIndexMode::Deep,
                IndexMode::Rebuild => return Err(ClientPortError::Executor),
            };
            let result = client
                .repository_index(
                    request.root().to_owned(),
                    operation,
                    request.detached(),
                    requested_mode,
                    timeout,
                )
                .await
                .map_err(map_client_error)?;
            let (mode, providers) = match result.mode {
                ClientRepositoryIndexMode::Structural => {
                    (IndexMode::Structural, vec![FIRST_SLICE_PROVIDER.to_owned()])
                }
                ClientRepositoryIndexMode::Deep => (
                    IndexMode::Deep,
                    vec![
                        FIRST_SLICE_PROVIDER.to_owned(),
                        PROJECT_SEMANTICS_PROVIDER.to_owned(),
                    ],
                ),
                ClientRepositoryIndexMode::Auto => {
                    return Err(ClientPortError::InvalidResponse);
                }
            };
            let accepted_plan = IndexPlanSummary {
                scope: IndexPlanScope::Repository,
                mode,
                providers,
                parent_generation: RequiredNullable(result.parent_generation),
                estimated_disk_bytes: result.estimated_disk_bytes,
            };
            let mut diagnostics = Vec::new();
            diagnostics
                .try_reserve_exact(result.diagnostics.len())
                .map_err(|_| ClientPortError::Executor)?;
            for diagnostic in &result.diagnostics {
                diagnostics.push(Diagnostic {
                    code: SafeLabel::parse(&diagnostic.code)
                        .map_err(|_| ClientPortError::InvalidResponse)?,
                    message: SourceFreeMessage::parse(&diagnostic.message)
                        .map_err(|_| ClientPortError::InvalidResponse)?,
                });
            }
            Ok(RepositoryIndexPortResponse::new(
                result,
                accepted_plan,
                diagnostics,
            ))
        })
    }

    fn operation_status(
        &self,
        request: OperationStatusPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryOperationStatus> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .operation_status(
                    request.operation(),
                    request.action(),
                    request.wait_ms(),
                    request.after_revision(),
                    request_timeout()?,
                )
                .await
                .map_err(map_client_error)
        })
    }

    fn code_locate(
        &self,
        request: CodeLocatePortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .code_locate(
                    request.repository(),
                    request.generation(),
                    request.query().to_owned(),
                    request.mode(),
                    request.maximum_results(),
                    request.page_offset(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let languages = locate_languages(&result)?;
            let metadata = read_metadata(&result.context, languages)?;
            // The daemon response does not expose its normalized query tokens.
            // An empty set is safer than presenting user text as server analysis.
            Ok(CodeLocatePortResponse::new(result, metadata, Vec::new()))
        })
    }

    fn symbol_explain(
        &self,
        request: SymbolExplainPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .symbol_explain(
                    request.repository(),
                    request.generation(),
                    request.symbols().to_vec(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(SymbolExplainPortResponse::new(result, metadata))
        })
    }

    fn source_read(
        &self,
        request: SourceReadPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .source_read(
                    request.repository(),
                    request.generation(),
                    request.references().to_vec(),
                    ClientSourceReadOptions {
                        context_lines_before: request.context_lines_before(),
                        context_lines_after: request.context_lines_after(),
                        merge_overlaps: request.merge_overlaps(),
                        include_line_numbers: request.include_line_numbers(),
                        encoding: match request.encoding() {
                            rootlight_mcp_contract::vertical::SourceEncodingRequest::Utf8LosslessWhenValid => {
                                ClientSourceEncoding::Utf8
                            }
                            rootlight_mcp_contract::vertical::SourceEncodingRequest::BytesBase64 => {
                                ClientSourceEncoding::Bytes
                            }
                        },
                    },
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let languages = source_languages(&result)?;
            let metadata = read_metadata(&result.context, languages)?;
            Ok(SourceReadPortResponse::new(
                result,
                metadata,
                Vec::new(),
                Vec::new(),
            ))
        })
    }

    fn repository_catalog_page(
        &self,
        request: RepositoryCatalogPagePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryCatalogPage> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .repository_catalog_page(request, request_timeout()?)
                .await
                .map_err(map_client_error)
        })
    }

    fn repository_status(
        &self,
        request: RepositoryStatusPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryStatus> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let status_request = rootlight_client::RepositoryStatusRequest::new(
                request.repository(),
                request.generation(),
            )
            .with_coverage_detail(request.coverage_detail())
            .with_operations(request.include_operations())
            .with_freshness_requirement(request.freshness_requirement());
            client
                .repository_status(status_request, request_timeout()?)
                .await
                .map_err(map_client_error)
        })
    }

    fn symbol_relationships(
        &self,
        request: SymbolRelationshipsPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolRelationshipsPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .symbol_relationships(
                    request.repository(),
                    request.generation(),
                    request.seeds().to_vec(),
                    request.relations().to_vec(),
                    request.direction().map(str::to_owned),
                    request.min_confidence(),
                    request.max_results(),
                    request.page_offset(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(SymbolRelationshipsPortResponse::new(result, metadata))
        })
    }

    fn flow_trace(
        &self,
        request: FlowTracePortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<FlowTracePortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .flow_trace(
                    request.repository(),
                    request.generation(),
                    request.from(),
                    request.to(),
                    request.relations().to_vec(),
                    request.direction().map(str::to_owned),
                    request.max_depth(),
                    request.max_paths(),
                    request.min_confidence(),
                    request.cross_repository(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(FlowTracePortResponse::new(result, metadata))
        })
    }

    fn architecture_cycles(
        &self,
        request: ArchitectureCyclesPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureCyclesPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .architecture_cycles(
                    request.repository(),
                    request.generation(),
                    request.relations().to_vec(),
                    request.min_size(),
                    request.max_cycles(),
                    request.include_self_cycles(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(ArchitectureCyclesPortResponse::new(result, metadata))
        })
    }

    fn code_dead(
        &self,
        request: CodeDeadPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeDeadPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .code_dead(
                    request.repository(),
                    request.generation(),
                    request.entry_point_policy().map(str::to_owned),
                    request.include_exported(),
                    request.include_tests(),
                    request.min_confidence(),
                    request.max_candidates(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(CodeDeadPortResponse::new(result, metadata))
        })
    }

    fn architecture_overview(
        &self,
        request: ArchitectureOverviewPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureOverviewPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .architecture_overview(
                    request.repository(),
                    request.generation(),
                    request.views().to_vec(),
                    request.max_components(),
                    request.include_edges(),
                    request.min_confidence(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(ArchitectureOverviewPortResponse::new(result, metadata))
        })
    }

    fn tests_select(
        &self,
        request: TestsSelectPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<TestsSelectPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .tests_select(
                    request.repository(),
                    request.generation(),
                    request.seeds().to_vec(),
                    request.test_kinds().to_vec(),
                    request.max_tests(),
                    request.include_commands(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(TestsSelectPortResponse::new(result, metadata))
        })
    }

    fn change_impact(
        &self,
        request: ChangeImpactPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ChangeImpactPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .change_impact(
                    request.repository(),
                    request.generation(),
                    request.changed_symbols().to_vec(),
                    request.changed_paths().to_vec(),
                    request.max_depth(),
                    request.min_confidence(),
                    request.include_tests(),
                    request.max_dependents(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(ChangeImpactPortResponse::new(result, metadata))
        })
    }

    fn plan_change(
        &self,
        request: PlanChangePortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .plan_change(
                    request.repository(),
                    match request.generation() {
                        rootlight_mcp_contract::GenerationSelector::Active(_) => {
                            rootlight_client::GenerationSelector::Active
                        }
                        rootlight_mcp_contract::GenerationSelector::Explicit(generation) => {
                            rootlight_client::GenerationSelector::Generation(*generation)
                        }
                    },
                    request.objective().to_owned(),
                    request.objective_text().to_owned(),
                    request.target_symbols().to_vec(),
                    request.target_files().to_vec(),
                    request.max_steps(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(PlanChangePortResponse::new(result, metadata))
        })
    }

    fn history_compare(
        &self,
        request: HistoryComparePortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<HistoryComparePortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .history_compare(
                    request.repository(),
                    request.base(),
                    request.head(),
                    request.change_kinds().to_vec(),
                    request.max_results(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(HistoryComparePortResponse::new(result, metadata))
        })
    }

    fn query_advanced(
        &self,
        request: QueryAdvancedPortRequest,
        options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<QueryAdvancedPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .query_advanced(
                    request.repository(),
                    request.generation(),
                    request.query_ast().to_owned(),
                    request.explain(),
                    request.max_results(),
                    request.max_depth(),
                    request.cost_limit(),
                    request.page_offset(),
                    options,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(QueryAdvancedPortResponse::new(result, metadata))
        })
    }
}

/// Source-free first-slice port used when synchronous daemon setup is unavailable.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnavailableFirstSliceClientPort;

impl FirstSliceClientPort for UnavailableFirstSliceClientPort {
    fn repository_index(
        &self,
        _request: RepositoryIndexPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryIndexPortResponse> {
        unavailable()
    }

    fn operation_status(
        &self,
        _request: OperationStatusPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryOperationStatus> {
        unavailable()
    }

    fn code_locate(
        &self,
        _request: CodeLocatePortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse> {
        unavailable()
    }

    fn symbol_explain(
        &self,
        _request: SymbolExplainPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        unavailable()
    }

    fn source_read(
        &self,
        _request: SourceReadPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse> {
        unavailable()
    }

    fn repository_catalog_page(
        &self,
        _request: RepositoryCatalogPagePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryCatalogPage> {
        unavailable()
    }

    fn repository_status(
        &self,
        _request: RepositoryStatusPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryStatus> {
        unavailable()
    }

    fn symbol_relationships(
        &self,
        _request: SymbolRelationshipsPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolRelationshipsPortResponse> {
        unavailable()
    }

    fn flow_trace(
        &self,
        _request: FlowTracePortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<FlowTracePortResponse> {
        unavailable()
    }

    fn architecture_cycles(
        &self,
        _request: ArchitectureCyclesPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureCyclesPortResponse> {
        unavailable()
    }

    fn code_dead(
        &self,
        _request: CodeDeadPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeDeadPortResponse> {
        unavailable()
    }

    fn architecture_overview(
        &self,
        _request: ArchitectureOverviewPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureOverviewPortResponse> {
        unavailable()
    }

    fn tests_select(
        &self,
        _request: TestsSelectPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<TestsSelectPortResponse> {
        unavailable()
    }

    fn change_impact(
        &self,
        _request: ChangeImpactPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ChangeImpactPortResponse> {
        unavailable()
    }

    fn plan_change(
        &self,
        _request: PlanChangePortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse> {
        unavailable()
    }

    fn history_compare(
        &self,
        _request: HistoryComparePortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<HistoryComparePortResponse> {
        unavailable()
    }

    fn query_advanced(
        &self,
        _request: QueryAdvancedPortRequest,
        _options: RequestOptions,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<QueryAdvancedPortResponse> {
        unavailable()
    }
}

fn unavailable<T>() -> ClientPortFuture<T> {
    Box::pin(async { Err(ClientPortError::Transport) })
}

fn request_timeout() -> Result<RequestTimeout, ClientPortError> {
    RequestTimeout::new(CLIENT_REQUEST_TIMEOUT).map_err(|_| ClientPortError::Executor)
}

fn random_operation_id() -> Result<OperationId, ClientPortError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ClientPortError::Executor)?;
    Ok(OperationId::from_bytes(bytes))
}

fn bridge_trace_id() -> Result<String, ClientPortError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| ClientPortError::Executor)?;
    let mut trace = String::new();
    trace
        .try_reserve_exact(BRIDGE_TRACE_PREFIX.len() + (bytes.len() * 2))
        .map_err(|_| ClientPortError::Executor)?;
    trace.push_str(BRIDGE_TRACE_PREFIX);
    for byte in bytes {
        use fmt::Write as _;
        write!(&mut trace, "{byte:02x}").map_err(|_| ClientPortError::Executor)?;
    }
    Ok(trace)
}

fn read_metadata(
    context: &rootlight_client::QueryContext,
    languages: Vec<LanguageCoverage>,
) -> Result<ReadResponseMetadata, ClientPortError> {
    let freshness = if context.active_generation {
        Freshness::Current
    } else {
        Freshness::Superseded
    };
    Ok(ReadResponseMetadata::new(
        context.repository.to_string(),
        freshness,
        freshness,
        languages,
        CacheStatus::NotApplicable,
        bridge_trace_id()?,
        Vec::new(),
    ))
}

fn locate_languages(result: &CodeLocate) -> Result<Vec<LanguageCoverage>, ClientPortError> {
    let mut languages = BTreeMap::from([(FIRST_SLICE_LANGUAGE.to_owned(), result.context.tier)]);
    for hit in &result.hits {
        if hit.language != FIRST_SLICE_LANGUAGE {
            return Err(ClientPortError::InvalidResponse);
        }
        languages
            .entry(hit.language.clone())
            .and_modify(|tier| *tier = weaker_tier(*tier, hit.tier))
            .or_insert(hit.tier);
    }
    Ok(language_coverage(languages, result.context.coverage_status))
}

fn source_languages(result: &SourceRead) -> Result<Vec<LanguageCoverage>, ClientPortError> {
    if result
        .chunks
        .iter()
        .any(|chunk| chunk.language != FIRST_SLICE_LANGUAGE)
    {
        return Err(ClientPortError::InvalidResponse);
    }
    Ok(service_languages(&result.context))
}

fn service_languages(context: &rootlight_client::QueryContext) -> Vec<LanguageCoverage> {
    language_coverage(
        BTreeMap::from([(FIRST_SLICE_LANGUAGE.to_owned(), context.tier)]),
        context.coverage_status,
    )
}

fn language_coverage(
    languages: BTreeMap<String, ClientAnalysisTier>,
    status: CoverageStatus,
) -> Vec<LanguageCoverage> {
    languages
        .into_iter()
        .map(|(language, tier)| LanguageCoverage {
            language,
            tier: analysis_tier(tier),
            status: coverage_status(status),
        })
        .collect()
}

const fn weaker_tier(left: ClientAnalysisTier, right: ClientAnalysisTier) -> ClientAnalysisTier {
    if analysis_tier_rank(left) >= analysis_tier_rank(right) {
        left
    } else {
        right
    }
}

const fn analysis_tier_rank(tier: ClientAnalysisTier) -> u8 {
    match tier {
        ClientAnalysisTier::TierA => 0,
        ClientAnalysisTier::TierB => 1,
        ClientAnalysisTier::TierC => 2,
        ClientAnalysisTier::TierD => 3,
    }
}

const fn analysis_tier(tier: ClientAnalysisTier) -> AnalysisTier {
    match tier {
        ClientAnalysisTier::TierA => AnalysisTier::A,
        ClientAnalysisTier::TierB => AnalysisTier::B,
        ClientAnalysisTier::TierC => AnalysisTier::C,
        ClientAnalysisTier::TierD => AnalysisTier::D,
    }
}

const fn coverage_status(status: CoverageStatus) -> IrCoverageStatus {
    match status {
        CoverageStatus::Complete => IrCoverageStatus::Complete,
        CoverageStatus::Bounded => IrCoverageStatus::Bounded,
        CoverageStatus::Sampled => IrCoverageStatus::Sampled,
        CoverageStatus::Unknown => IrCoverageStatus::Unknown,
    }
}

fn map_client_error(error: ClientError) -> ClientPortError {
    match error {
        ClientError::Public(error) => ClientPortError::Public(error),
        ClientError::MismatchedRequestId
        | ClientError::MissingResponse
        | ClientError::UnexpectedResponse
        | ClientError::InvalidResponseSchema
        | ClientError::InvalidResponseCorrelation
        | ClientError::MissingOperation
        | ClientError::InvalidDaemonLifecycle
        | ClientError::InvalidHealthStatus
        | ClientError::InvalidResourcePressure
        | ClientError::InvalidDiagnostics
        | ClientError::InvalidSupportBundle
        | ClientError::InvalidOperationState
        | ClientError::InvalidOperationKind
        | ClientError::InvalidOperationStage
        | ClientError::InvalidRecoveryClass
        | ClientError::InvalidPlanHash
        | ClientError::InvalidIdentifier
        | ClientError::InvalidPublicError => ClientPortError::InvalidResponse,
        ClientError::ResponseAllocationFailed
        | ClientError::InvalidFirstSliceRequest
        | ClientError::InvalidRepositoryCatalogRequest
        | ClientError::InvalidSourceReference
        | ClientError::InvalidRequestTimeout
        | ClientError::InvalidEffectiveBudget
        | ClientError::InvalidOperationTiming
        | ClientError::InvalidOperationLease
        | ClientError::InvalidSystemClock
        | ClientError::RequestIdExhausted => ClientPortError::Executor,
        ClientError::Ipc(_)
        | ClientError::NonceMismatch
        | ClientError::MissingProtocol
        | ClientError::ProtocolMismatch
        | ClientError::ProtocolFeatureUnavailable
        | ClientError::RequestTimedOut
        | ClientError::Runtime(_)
        | ClientError::DaemonUnavailable
        | ClientError::LaunchIo(_)
        | ClientError::DaemonExecutableMissing
        | ClientError::DaemonLaunchFailed
        | ClientError::DaemonLaunchCleanupTimedOut
        | ClientError::DaemonStartTimedOut => ClientPortError::Transport,
    }
}

#[cfg(test)]
mod tests;
