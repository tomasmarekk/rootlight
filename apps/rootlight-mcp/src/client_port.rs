//! Native asynchronous daemon-client port for the production MCP executor.
//!
//! This boundary enriches checked client DTOs only with facts Rootlight can
//! prove locally; unavailable startup remains a source-free transport failure.

use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use rootlight_client::{
    AnalysisTier as ClientAnalysisTier, ArchitectureCycles, ArchitectureOverview, ChangeImpact,
    Client, ClientError, CodeDead, CodeLocate, CoverageStatus, FlowTrace, GenerationSelector,
    LocateMode, PlanChange, RepositoryIndex, RepositoryList, RepositoryOperationAction,
    RepositoryOperationStatus, RepositoryStatus, RequestTimeout, SourceRead, SourceReference,
    SymbolExplain, SymbolRelationships, TestsSelect,
};
use rootlight_ids::{FileId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::CoverageStatus as IrCoverageStatus;
use rootlight_mcp_contract::vertical::{
    AnalysisTier, CacheStatus, Freshness, IndexMode, IndexPlanScope, IndexPlanSummary,
    LanguageCoverage, RequiredNullable,
};

use crate::{
    ArchitectureCyclesPortRequest, ArchitectureCyclesPortResponse, ArchitectureOverviewPortRequest,
    ArchitectureOverviewPortResponse, ChangeImpactPortRequest, ChangeImpactPortResponse,
    ClientPortError, ClientPortFuture, CodeDeadPortRequest, CodeDeadPortResponse,
    CodeLocatePortRequest, CodeLocatePortResponse, FirstSliceClientPort, FlowTracePortRequest,
    FlowTracePortResponse, OperationStatusPortRequest, PlanChangePortRequest,
    PlanChangePortResponse, ReadResponseMetadata, RepositoryIndexPortRequest,
    RepositoryIndexPortResponse, RepositoryListPortRequest, RepositoryStatusPortRequest,
    RequestCancellation, SourceReadPortRequest, SourceReadPortResponse, SymbolExplainPortRequest,
    SymbolExplainPortResponse, SymbolRelationshipsPortRequest, SymbolRelationshipsPortResponse,
    TestsSelectPortRequest, TestsSelectPortResponse,
};

const CLIENT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_SLICE_PROVIDER: &str = "rootlight-first-slice-treesitter";
const FIRST_SLICE_LANGUAGE: &str = "rust";
const BRIDGE_TRACE_PREFIX: &str = "bridge-";

type AsyncClientFuture<T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'static>>;

trait AsyncFirstSliceClient: Send + Sync + 'static {
    fn repository_index(
        &self,
        root: String,
        operation: OperationId,
        detached: bool,
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

    fn code_locate(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        query: String,
        mode: LocateMode,
        maximum_results: u32,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<CodeLocate>;

    fn symbol_explain(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: Vec<SymbolId>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SymbolExplain>;

    fn source_read(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: Vec<SourceReference>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SourceRead>;

    fn repository_list(
        &self,
        max_results: Option<u32>,
        query: Option<String>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryList>;

    fn repository_status(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<PlanChange>;
}

struct LiveAsyncFirstSliceClient {
    client: Arc<Client>,
}

impl AsyncFirstSliceClient for LiveAsyncFirstSliceClient {
    fn repository_index(
        &self,
        root: String,
        operation: OperationId,
        detached: bool,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryIndex> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .repository_index_async(&root, operation, detached, timeout)
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<CodeLocate> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .code_locate_async(
                    repository,
                    generation,
                    &query,
                    mode,
                    maximum_results,
                    timeout,
                )
                .await
        })
    }

    fn symbol_explain(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: Vec<SymbolId>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SymbolExplain> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .symbol_explain_async(repository, generation, &symbols, timeout)
                .await
        })
    }

    fn source_read(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: Vec<SourceReference>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SourceRead> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .source_read_async(repository, generation, &references, timeout)
                .await
        })
    }

    fn repository_list(
        &self,
        max_results: Option<u32>,
        query: Option<String>,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryList> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .repository_list_async(max_results, query.as_deref(), timeout)
                .await
        })
    }

    fn repository_status(
        &self,
        repository: RepositoryId,
        generation: GenerationSelector,
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<RepositoryStatus> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .repository_status_async(repository, generation, timeout)
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<SymbolRelationships> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .symbol_relationships_async(
                    repository,
                    generation,
                    &seeds,
                    &relations,
                    direction.as_deref(),
                    min_confidence,
                    max_results,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<FlowTrace> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .flow_trace_async(
                    repository,
                    generation,
                    from,
                    to,
                    &relations,
                    direction.as_deref(),
                    max_depth,
                    max_paths,
                    min_confidence,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<ArchitectureCycles> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .architecture_cycles_async(
                    repository,
                    generation,
                    &relations,
                    min_size,
                    max_cycles,
                    include_self_cycles,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<CodeDead> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .code_dead_async(
                    repository,
                    generation,
                    entry_point_policy.as_deref(),
                    include_exported,
                    include_tests,
                    min_confidence,
                    max_candidates,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<ArchitectureOverview> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .architecture_overview_async(
                    repository,
                    generation,
                    &views,
                    max_components,
                    include_edges,
                    min_confidence,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<TestsSelect> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .tests_select_async(
                    repository,
                    generation,
                    &seeds,
                    &test_kinds,
                    max_tests,
                    include_commands,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<ChangeImpact> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .change_impact_async(
                    repository,
                    generation,
                    &changed_symbols,
                    &changed_paths,
                    max_depth,
                    min_confidence,
                    include_tests,
                    max_dependents,
                    timeout,
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
        timeout: RequestTimeout,
    ) -> AsyncClientFuture<PlanChange> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .plan_change_async(
                    repository,
                    generation,
                    &objective,
                    &objective_text,
                    &target_symbols,
                    &target_files,
                    max_steps,
                    timeout,
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
                client: Arc::new(client),
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
            let result = client
                .repository_index(
                    request.root().to_owned(),
                    operation,
                    request.detached(),
                    timeout,
                )
                .await
                .map_err(map_client_error)?;
            let accepted_plan = IndexPlanSummary {
                scope: IndexPlanScope::Repository,
                mode: IndexMode::Structural,
                providers: vec![FIRST_SLICE_PROVIDER.to_owned()],
                parent_generation: RequiredNullable(result.parent_generation),
                // The current fallback publishes SQLite and lexical state only
                // in memory, so generation staging writes no disk bytes.
                estimated_disk_bytes: 0,
            };
            Ok(RepositoryIndexPortResponse::new(
                result,
                accepted_plan,
                Vec::new(),
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
                    request_timeout()?,
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
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .symbol_explain(
                    request.repository(),
                    request.generation(),
                    request.symbols().to_vec(),
                    request_timeout()?,
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
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .source_read(
                    request.repository(),
                    request.generation(),
                    request.references().to_vec(),
                    request_timeout()?,
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

    fn repository_list(
        &self,
        request: RepositoryListPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryList> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            client
                .repository_list(
                    request.max_results(),
                    request.query().map(str::to_owned),
                    request_timeout()?,
                )
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
            client
                .repository_status(
                    request.repository(),
                    request.generation(),
                    request_timeout()?,
                )
                .await
                .map_err(map_client_error)
        })
    }

    fn symbol_relationships(
        &self,
        request: SymbolRelationshipsPortRequest,
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
                    request_timeout()?,
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
                    request_timeout()?,
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
                    request_timeout()?,
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
                    request_timeout()?,
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
                    request_timeout()?,
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
                    request_timeout()?,
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
                    request_timeout()?,
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
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse> {
        let client = Arc::clone(&self.client);
        Box::pin(async move {
            let result = client
                .plan_change(
                    request.repository(),
                    request.generation(),
                    request.objective().to_owned(),
                    request.objective_text().to_owned(),
                    request.target_symbols().to_vec(),
                    request.target_files().to_vec(),
                    request.max_steps(),
                    request_timeout()?,
                )
                .await
                .map_err(map_client_error)?;
            let metadata = read_metadata(&result.context, service_languages(&result.context))?;
            Ok(PlanChangePortResponse::new(result, metadata))
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
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeLocatePortResponse> {
        unavailable()
    }

    fn symbol_explain(
        &self,
        _request: SymbolExplainPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolExplainPortResponse> {
        unavailable()
    }

    fn source_read(
        &self,
        _request: SourceReadPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SourceReadPortResponse> {
        unavailable()
    }

    fn repository_list(
        &self,
        _request: RepositoryListPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<RepositoryList> {
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
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<SymbolRelationshipsPortResponse> {
        unavailable()
    }

    fn flow_trace(
        &self,
        _request: FlowTracePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<FlowTracePortResponse> {
        unavailable()
    }

    fn architecture_cycles(
        &self,
        _request: ArchitectureCyclesPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureCyclesPortResponse> {
        unavailable()
    }

    fn code_dead(
        &self,
        _request: CodeDeadPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<CodeDeadPortResponse> {
        unavailable()
    }

    fn architecture_overview(
        &self,
        _request: ArchitectureOverviewPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ArchitectureOverviewPortResponse> {
        unavailable()
    }

    fn tests_select(
        &self,
        _request: TestsSelectPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<TestsSelectPortResponse> {
        unavailable()
    }

    fn change_impact(
        &self,
        _request: ChangeImpactPortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<ChangeImpactPortResponse> {
        unavailable()
    }

    fn plan_change(
        &self,
        _request: PlanChangePortRequest,
        _cancellation: RequestCancellation,
    ) -> ClientPortFuture<PlanChangePortResponse> {
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
        | ClientError::InvalidSourceReference
        | ClientError::InvalidRequestTimeout
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
