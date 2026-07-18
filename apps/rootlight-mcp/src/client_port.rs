//! Native asynchronous daemon-client port for the production MCP executor.
//!
//! This boundary enriches checked client DTOs only with facts Rootlight can
//! prove locally; unavailable startup remains a source-free transport failure.

use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc, time::Duration};

use rootlight_client::{
    AnalysisTier as ClientAnalysisTier, Client, ClientError, CodeLocate, CoverageStatus,
    GenerationSelector, LocateMode, RepositoryIndex, RepositoryOperationAction,
    RepositoryOperationStatus, RequestTimeout, SourceRead, SourceReference, SymbolExplain,
};
use rootlight_ids::{OperationId, RepositoryId, SymbolId};
use rootlight_ir::CoverageStatus as IrCoverageStatus;
use rootlight_mcp_contract::vertical::{
    AnalysisTier, CacheStatus, Freshness, IndexMode, IndexPlanScope, IndexPlanSummary,
    LanguageCoverage, RequiredNullable,
};

use crate::{
    ClientPortError, ClientPortFuture, CodeLocatePortRequest, CodeLocatePortResponse,
    FirstSliceClientPort, OperationStatusPortRequest, ReadResponseMetadata,
    RepositoryIndexPortRequest, RepositoryIndexPortResponse, RequestCancellation,
    SourceReadPortRequest, SourceReadPortResponse, SymbolExplainPortRequest,
    SymbolExplainPortResponse,
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
        | ClientError::ResponseAllocationFailed
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
        ClientError::InvalidFirstSliceRequest
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
        | ClientError::DaemonStartTimedOut => ClientPortError::Transport,
    }
}

#[cfg(test)]
mod tests;
