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
type ReconnectFuture<'a> = Pin<Box<dyn Future<Output = Option<Arc<dyn DaemonClient>>> + Send + 'a>>;

trait DaemonReconnect: Send + Sync {
    fn reconnect(&self) -> ReconnectFuture<'_>;
}

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

struct ResilientDaemonClient {
    current: tokio::sync::RwLock<Arc<dyn DaemonClient>>,
    reconnect: Arc<dyn DaemonReconnect>,
    reconnect_gate: tokio::sync::Mutex<()>,
}

impl ResilientDaemonClient {
    fn new(initial: Arc<dyn DaemonClient>, reconnect: Arc<dyn DaemonReconnect>) -> Self {
        Self {
            current: tokio::sync::RwLock::new(initial),
            reconnect,
            reconnect_gate: tokio::sync::Mutex::new(()),
        }
    }

    async fn current(&self) -> Arc<dyn DaemonClient> {
        let current = self.current.read().await;
        Arc::clone(&current)
    }

    async fn reconnect_after(
        &self,
        failed: &Arc<dyn DaemonClient>,
    ) -> Option<Arc<dyn DaemonClient>> {
        let _guard = self.reconnect_gate.lock().await;
        let current = self.current().await;
        if !Arc::ptr_eq(&current, failed) {
            return Some(current);
        }
        let replacement = self.reconnect.reconnect().await?;
        *self.current.write().await = Arc::clone(&replacement);
        Some(replacement)
    }
}

impl DaemonClient for ResilientDaemonClient {
    fn health<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, Health> {
        Box::pin(async move {
            let client = self.current().await;
            match client.health(timeout).await {
                Ok(health) => Ok(health),
                Err(error) if reconnectable_health_error(&error) => {
                    let Some(replacement) = self.reconnect_after(&client).await else {
                        return Err(error);
                    };
                    replacement.health(timeout).await
                }
                Err(error) => Err(error),
            }
        })
    }

    fn repository_catalog_page<'a>(
        &'a self,
        request: &'a RepositoryCatalogPageRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryCatalogPage> {
        Box::pin(async move {
            self.current()
                .await
                .repository_catalog_page(request, timeout)
                .await
        })
    }

    fn repository_status<'a>(
        &'a self,
        request: RepositoryStatusRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryStatus> {
        Box::pin(async move {
            self.current()
                .await
                .repository_status(request, timeout)
                .await
        })
    }

    fn repository_index<'a>(
        &'a self,
        root: &'a str,
        operation: OperationId,
        mode: RepositoryIndexMode,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryIndex> {
        Box::pin(async move {
            self.current()
                .await
                .repository_index(root, operation, mode, timeout)
                .await
        })
    }

    fn repository_operation_status<'a>(
        &'a self,
        operation: OperationId,
        action: RepositoryOperationAction,
        wait_ms: Option<u32>,
        after_revision: Option<u64>,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, RepositoryOperationStatus> {
        Box::pin(async move {
            self.current()
                .await
                .repository_operation_status(operation, action, wait_ms, after_revision, timeout)
                .await
        })
    }

    fn graph_projection_open<'a>(
        &'a self,
        request: &'a GraphProjectionRequest,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, GraphProjectionPage> {
        Box::pin(async move {
            self.current()
                .await
                .graph_projection_open(request, timeout)
                .await
        })
    }

    fn graph_projection_page<'a>(
        &'a self,
        continuation: &'a GraphProjectionContinuation,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, GraphProjectionPage> {
        Box::pin(async move {
            self.current()
                .await
                .graph_projection_page(continuation, timeout)
                .await
        })
    }

    fn graph_projection_release<'a>(
        &'a self,
        projection: GraphProjectionId,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, bool> {
        Box::pin(async move {
            self.current()
                .await
                .graph_projection_release(projection, timeout)
                .await
        })
    }

    fn diagnostics_quick<'a>(
        &'a self,
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, DiagnosticsQuick> {
        Box::pin(async move { self.current().await.diagnostics_quick(timeout).await })
    }

    fn support_bundle<'a>(&'a self, timeout: RequestTimeout) -> ClientFuture<'a, SupportBundle> {
        Box::pin(async move { self.current().await.support_bundle(timeout).await })
    }

    fn symbol_explain<'a>(
        &'a self,
        repository: RepositoryId,
        generation: GenerationSelector,
        symbols: &'a [SymbolId],
        timeout: RequestTimeout,
    ) -> ClientFuture<'a, SymbolExplain> {
        Box::pin(async move {
            self.current()
                .await
                .symbol_explain(repository, generation, symbols, timeout)
                .await
        })
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
        Box::pin(async move {
            self.current()
                .await
                .symbol_relationships(
                    repository,
                    generation,
                    seeds,
                    relations,
                    direction,
                    min_confidence,
                    max_results,
                    page_offset,
                    timeout,
                )
                .await
        })
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
            self.current()
                .await
                .source_read(repository, generation, references, projection, timeout)
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
        Box::pin(async move {
            self.current()
                .await
                .change_impact(
                    repository,
                    generation,
                    changed_symbols,
                    max_depth,
                    min_confidence,
                    include_tests,
                    max_dependents,
                    timeout,
                )
                .await
        })
    }
}

struct RuntimeReconnect {
    paths: RuntimePaths,
    client_instance_id: [u8; 16],
}

impl DaemonReconnect for RuntimeReconnect {
    fn reconnect(&self) -> ReconnectFuture<'_> {
        let paths = self.paths.clone();
        let client_instance_id = self.client_instance_id;
        Box::pin(async move {
            connect_once(paths, client_instance_id)
                .await
                .ok()
                .map(|client| client as Arc<dyn DaemonClient>)
        })
    }
}

fn reconnectable_health_error(error: &ClientError) -> bool {
    matches!(
        error,
        ClientError::Ipc(_) | ClientError::NonceMismatch | ClientError::RequestTimedOut
    )
}

async fn connect_once(
    paths: RuntimePaths,
    client_instance_id: [u8; 16],
) -> Result<Arc<Client>, WebError> {
    tokio::task::spawn_blocking(move || {
        Client::connect_or_start(&paths, client_instance_id, ConnectPolicy::StartIfMissing)
            .map(Arc::new)
            .map_err(|_| WebError::DaemonUnavailable)
    })
    .await
    .map_err(|_| WebError::TaskFailed)?
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
    let initial = connect_once(paths.clone(), client_instance_id).await?;
    let reconnect = Arc::new(RuntimeReconnect {
        paths,
        client_instance_id,
    });
    Ok(Arc::new(ResilientDaemonClient::new(initial, reconnect)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use rootlight_client::{DaemonLifecycle, HealthStatus, ResourcePressure};

    use super::*;

    struct ScriptedDaemon {
        health: Health,
        health_failures: AtomicUsize,
        health_calls: AtomicUsize,
        health_barrier: Option<Arc<tokio::sync::Barrier>>,
        reconnectable: bool,
        index_calls: AtomicUsize,
        index_fails: bool,
    }

    impl ScriptedDaemon {
        fn healthy() -> Self {
            Self {
                health: test_health(),
                health_failures: AtomicUsize::new(0),
                health_calls: AtomicUsize::new(0),
                health_barrier: None,
                reconnectable: true,
                index_calls: AtomicUsize::new(0),
                index_fails: false,
            }
        }

        fn failing_health(
            failures: usize,
            reconnectable: bool,
            barrier: Option<Arc<tokio::sync::Barrier>>,
        ) -> Self {
            Self {
                health_failures: AtomicUsize::new(failures),
                health_barrier: barrier,
                reconnectable,
                ..Self::healthy()
            }
        }

        fn failing_index() -> Self {
            Self {
                index_fails: true,
                ..Self::healthy()
            }
        }
    }

    impl DaemonClient for ScriptedDaemon {
        fn health<'a>(&'a self, _timeout: RequestTimeout) -> ClientFuture<'a, Health> {
            Box::pin(async move {
                self.health_calls.fetch_add(1, Ordering::SeqCst);
                if let Some(barrier) = &self.health_barrier {
                    barrier.wait().await;
                }
                if take_failure(&self.health_failures) {
                    return Err(if self.reconnectable {
                        ClientError::NonceMismatch
                    } else {
                        ClientError::ProtocolMismatch
                    });
                }
                Ok(self.health.clone())
            })
        }

        fn repository_catalog_page<'a>(
            &'a self,
            _request: &'a RepositoryCatalogPageRequest,
            _timeout: RequestTimeout,
        ) -> ClientFuture<'a, RepositoryCatalogPage> {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_status<'a>(
            &'a self,
            _request: RepositoryStatusRequest,
            _timeout: RequestTimeout,
        ) -> ClientFuture<'a, RepositoryStatus> {
            Box::pin(async { Err(ClientError::ProtocolFeatureUnavailable) })
        }

        fn repository_index<'a>(
            &'a self,
            _root: &'a str,
            _operation: OperationId,
            _mode: RepositoryIndexMode,
            _timeout: RequestTimeout,
        ) -> ClientFuture<'a, RepositoryIndex> {
            Box::pin(async move {
                self.index_calls.fetch_add(1, Ordering::SeqCst);
                if self.index_fails {
                    Err(ClientError::NonceMismatch)
                } else {
                    Err(ClientError::ProtocolFeatureUnavailable)
                }
            })
        }
    }

    struct TestReconnect {
        replacement: Arc<dyn DaemonClient>,
        calls: AtomicUsize,
    }

    impl DaemonReconnect for TestReconnect {
        fn reconnect(&self) -> ReconnectFuture<'_> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let replacement = Arc::clone(&self.replacement);
            Box::pin(async move { Some(replacement) })
        }
    }

    #[tokio::test]
    async fn concurrent_health_failures_share_one_reconnect_and_retry() {
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let initial = Arc::new(ScriptedDaemon::failing_health(2, true, Some(barrier)));
        let replacement = Arc::new(ScriptedDaemon::healthy());
        let reconnect = Arc::new(TestReconnect {
            replacement: replacement.clone(),
            calls: AtomicUsize::new(0),
        });
        let client = ResilientDaemonClient::new(initial.clone(), reconnect.clone());
        let timeout =
            RequestTimeout::new(std::time::Duration::from_secs(1)).expect("test timeout is valid");

        let (first, second) = tokio::join!(client.health(timeout), client.health(timeout));

        assert_eq!(first.expect("first health recovers"), test_health());
        assert_eq!(second.expect("second health recovers"), test_health());
        assert_eq!(initial.health_calls.load(Ordering::SeqCst), 2);
        assert_eq!(replacement.health_calls.load(Ordering::SeqCst), 2);
        assert_eq!(reconnect.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn nontransient_health_failure_does_not_replace_the_client() {
        let initial = Arc::new(ScriptedDaemon::failing_health(1, false, None));
        let replacement = Arc::new(ScriptedDaemon::healthy());
        let reconnect = Arc::new(TestReconnect {
            replacement,
            calls: AtomicUsize::new(0),
        });
        let client = ResilientDaemonClient::new(initial, reconnect.clone());
        let timeout =
            RequestTimeout::new(std::time::Duration::from_secs(1)).expect("test timeout is valid");

        assert!(matches!(
            client.health(timeout).await,
            Err(ClientError::ProtocolMismatch)
        ));
        assert_eq!(reconnect.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn mutating_requests_are_never_retried_or_used_to_reconnect() {
        let initial = Arc::new(ScriptedDaemon::failing_index());
        let replacement = Arc::new(ScriptedDaemon::healthy());
        let reconnect = Arc::new(TestReconnect {
            replacement: replacement.clone(),
            calls: AtomicUsize::new(0),
        });
        let client = ResilientDaemonClient::new(initial.clone(), reconnect.clone());
        let timeout =
            RequestTimeout::new(std::time::Duration::from_secs(1)).expect("test timeout is valid");

        assert!(matches!(
            client
                .repository_index(
                    "/opaque-root",
                    OperationId::from_bytes([7; 16]),
                    RepositoryIndexMode::Auto,
                    timeout,
                )
                .await,
            Err(ClientError::NonceMismatch)
        ));
        assert_eq!(initial.index_calls.load(Ordering::SeqCst), 1);
        assert_eq!(replacement.index_calls.load(Ordering::SeqCst), 0);
        assert_eq!(reconnect.calls.load(Ordering::SeqCst), 0);
    }

    fn take_failure(counter: &AtomicUsize) -> bool {
        counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    fn test_health() -> Health {
        Health {
            ready: true,
            active_operations: 0,
            admitted_operations: 0,
            protocol_version: "1.10".to_owned(),
            lifecycle: DaemonLifecycle::Ready,
            accepting_operations: true,
            active_connections: 1,
            connection_limit: 128,
            queued_operations: 0,
            running_operations: 0,
            operation_queue_limit: 256,
            journal_healthy: true,
            catalog_status: HealthStatus::Healthy,
            catalog_schema_version: 2,
            generation_status: HealthStatus::Healthy,
            adapter_status: HealthStatus::Healthy,
            watcher_status: HealthStatus::Healthy,
            resource_pressure: ResourcePressure::Normal,
            endpoint_status: HealthStatus::Healthy,
            endpoint_schema_version: 2,
        }
    }
}
