//! Async thin-client boundary between browser handlers and the Rootlight daemon.

use std::{future::Future, pin::Pin, sync::Arc};

use rootlight_client::{
    ChangeImpact, Client, ClientError, ConnectPolicy, DiagnosticsQuick, EffectiveBudget,
    EffectiveBudgetLimits, GenerationSelector, GraphProjectionContinuation, GraphProjectionId,
    GraphProjectionPage, GraphProjectionRequest, Health, OperationId, RepositoryCatalogPage,
    RepositoryCatalogPageRequest, RepositoryId, RepositoryIndex, RepositoryIndexMode,
    RepositoryOperationAction, RepositoryOperationStatus, RepositoryStatus,
    RepositoryStatusRequest, RequestOptions, RequestTimeout, SourceRead, SourceReadOptions,
    SourceReference, SupportBundle, SymbolExplain, SymbolId, SymbolRelationships,
};
use rootlight_runtime::RuntimePaths;

use crate::error::WebError;

const WEB_CLIENT_INSTANCE_PREFIX: [u8; 8] = *b"rootweb1";
const WEB_SOURCE_BYTES: u64 = 64 * 1024;

type ClientFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ClientError>> + Send + 'a>>;

pub(crate) trait DaemonClient: Send + Sync {
    fn health<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, Health>;

    fn repository_catalog_page<'a>(
        &'a self,
        request: &'a RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryCatalogPage>;

    fn repository_status<'a>(
        &'a self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryStatus>;

    fn repository_index<'a>(
        &'a self,
        _root: &'a str,
        _operation: OperationId,
        _mode: RepositoryIndexMode,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryIndex> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn repository_operation_status<'a>(
        &'a self,
        _operation: OperationId,
        _action: RepositoryOperationAction,
        _wait_ms: Option<u32>,
        _after_revision: Option<u64>,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryOperationStatus> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn graph_projection_open<'a>(
        &'a self,
        _request: &'a GraphProjectionRequest,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, GraphProjectionPage> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn graph_projection_page<'a>(
        &'a self,
        _continuation: &'a GraphProjectionContinuation,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, GraphProjectionPage> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn graph_projection_release<'a>(
        &'a self,
        _projection: GraphProjectionId,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn diagnostics_quick<'a>(
        &'a self,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, DiagnosticsQuick> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn support_bundle<'a>(&'a self, _timeout: RequestTimeout) -> ClientFuture<'a, SupportBundle> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn symbol_explain<'a>(
        &'a self,
        _repository: RepositoryId,
        _generation: GenerationSelector,
        _symbols: &'a [SymbolId],
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, SymbolExplain> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded relationships query dimension"
    )]
    fn symbol_relationships<'a>(
        &'a self,
        _repository: RepositoryId,
        _generation: GenerationSelector,
        _seeds: &'a [SymbolId],
        _relations: &'a [String],
        _direction: Option<&'a str>,
        _min_confidence: Option<u16>,
        _max_results: Option<u16>,
        _page_offset: u64,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, SymbolRelationships> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    fn source_read<'a>(
        &'a self,
        _repository: RepositoryId,
        _generation: GenerationSelector,
        _references: &'a [SourceReference],
        _projection: SourceReadOptions,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, SourceRead> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "each argument is one bounded change-impact query dimension"
    )]
    fn change_impact<'a>(
        &'a self,
        _repository: RepositoryId,
        _generation: GenerationSelector,
        _changed_symbols: &'a [SymbolId],
        _max_depth: Option<u8>,
        _min_confidence: Option<u16>,
        _include_tests: Option<bool>,
        _max_dependents: Option<u16>,
        _timeout: RequestTimeout,
    ) -> ClientFuture<'a, ChangeImpact> {
        Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
    }
}

impl DaemonClient for Client {
    fn health<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, Health> {
        Box::pin(self.health_async(timeout))
    }

    fn repository_catalog_page<'a>(
        &'a self,
        request: &'a RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryCatalogPage> {
        Box::pin(self.repository_catalog_page_async(request, timeout))
    }

    fn repository_status<'a>(
        &'a self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryStatus> {
        Box::pin(self.repository_status_with_options_async(request, timeout))
    }

    fn repository_index<'a>(
        &'a self,
        root: &'a str,
        operation: OperationId,
        mode: RepositoryIndexMode,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryIndex> {
        Box::pin(self.repository_index_async_with_mode(root, operation, true, mode, timeout))
    }

    fn repository_operation_status<'a>(
        &'a self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryOperationStatus> {
        Box::pin(self.repository_operation_status_async(
            operation,
            action,
            wait_ms,
            after_revision,
            timeout,
        ))
    }

    fn graph_projection_open<'a>(
        &'a self,
        request: &'a GraphProjectionRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, GraphProjectionPage> {
        Box::pin(self.graph_projection_open_async(request, timeout))
    }

    fn graph_projection_page<'a>(
        &'a self,
        continuation: &'a GraphProjectionContinuation,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, GraphProjectionPage> {
        Box::pin(self.graph_projection_page_async(continuation, timeout))
    }

    fn graph_projection_release<'a>(
        &'a self,
        projection: GraphProjectionId,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, bool> {
        Box::pin(self.graph_projection_release_async(projection, timeout))
    }

    fn diagnostics_quick<'a>(
        &'a self,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, DiagnosticsQuick> {
        Box::pin(self.diagnostics_quick_async(timeout))
    }

    fn support_bundle<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, SupportBundle> {
        Box::pin(self.support_bundle_async(timeout))
    }

    fn symbol_explain<'a>(
        &'a self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: &'a [SymbolId],
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, SymbolExplain> {
        Box::pin(self.symbol_explain_async(repository, generation, symbols, timeout))
    }

    fn symbol_relationships<'a>(
        &'a self,
        repository: RepositoryId,
        generation: GenerationSelector,
        seeds: &'a [SymbolId],
        relations: &'a [String],
        direction: Option<&'a str>,
        min_confidence: Option<u16>,
        max_results: Option<u16>,
        page_offset: u64,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, SymbolRelationships> {
        Box::pin(self.symbol_relationships_async(
            repository,
            generation,
            seeds,
            relations,
            direction,
            min_confidence,
            max_results,
            page_offset,
            timeout,
        ))
    }

    fn source_read<'a>(
        &'a self,
        repository: RepositoryId,
        generation: GenerationSelector,
        references: &'a [SourceReference],
        projection: SourceReadOptions,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, SourceRead> {
        Box::pin(async move {
            let budget = EffectiveBudget::new(EffectiveBudgetLimits {
                rows: 256,
                edges: 1,
                results: 1,
                source_bytes: WEB_SOURCE_BYTES,
                json_bytes: 256 * 1024,
                estimated_tokens: 256 * 1024,
                memory_bytes: 512 * 1024,
                duration: timeout.duration(),
                depth: None,
                paths: None,
            })?;
            self.source_read_projected_async_with_options(
                repository,
                generation,
                references,
                projection,
                RequestOptions::new()
                    .with_timeout(timeout)
                    .with_effective_budget(budget),
            )
            .await
        })
    }

    fn change_impact<'a>(
        &'a self,
        repository: RepositoryId,
        generation: GenerationSelector,
        changed_symbols: &'a [SymbolId],
        max_depth: Option<u8>,
        min_confidence: Option<u16>,
        include_tests: Option<bool>,
        max_dependents: Option<u16>,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, ChangeImpact> {
        Box::pin(self.change_impact_async(
            repository,
            generation,
            changed_symbols,
            &[],
            max_depth,
            min_confidence,
            include_tests,
            max_dependents,
            timeout,
        ))
    }
}

/// Connects a web-session client to the authenticated local daemon.
///
/// The synchronous discovery/start protocol runs on Tokio's blocking pool so
/// it never occupies an async HTTP worker.
///
/// # Errors
///
/// Returns a source-free [`WebError`] when randomness, discovery, daemon
/// startup, or the blocking task boundary fails.
pub(crate) async fn connect(paths: RuntimePaths) -> Result<Arc<dyn DaemonClient>, WebError> {
    let mut client_instance_id = [0_u8; 16];
    client_instance_id[..WEB_CLIENT_INSTANCE_PREFIX.len()]
        .copy_from_slice(&WEB_CLIENT_INSTANCE_PREFIX);
    getrandom::fill(&mut client_instance_id[WEB_CLIENT_INSTANCE_PREFIX.len()..])
        .map_err(|_| WebError::RandomUnavailable)?;
    tokio::task::spawn_blocking(move || {
        Client::connect_or_start(&paths, client_instance_id, ConnectPolicy::StartIfMissing)
            .map(|client| Arc::new(client) as Arc<dyn DaemonClient>)
            .map_err(|_| WebError::DaemonUnavailable)
    })
    .await
    .map_err(|_| WebError::TaskFailed)?
}
