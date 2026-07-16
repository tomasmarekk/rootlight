//! Rootlight per-user daemon process entry point.
//!
//! The binary owns process-lifetime writer arbitration and accepts bounded local
//! control connections; request semantics stay in `rootlight-daemon-core`.

#![forbid(unsafe_code)]

use std::{env, path::PathBuf, process::ExitCode, sync::Arc};

use rootlight_daemon_core::{
    ControlService, DaemonLifecycle, DaemonLimits, DaemonOrchestrator, DaemonState, JournalActor,
    handle_connection_async,
};
use rootlight_ipc::{AsyncLocalListener, FrameCodec};
use rootlight_operations::{CatalogWriterLock, OperationJournal};
use rootlight_runtime::{DiscoveryRecord, RuntimePaths};

const EXPIRY_BATCH: usize = 64;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rootlight-daemon: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), DaemonError> {
    let mode = validate_arguments()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(DaemonError::AsyncRuntime)?;
    runtime.block_on(run_async(mode))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonMode {
    Normal,
    Coordinated,
    Supervised,
}

fn validate_arguments() -> Result<DaemonMode, DaemonError> {
    let mut arguments = env::args_os().skip(1);
    match (arguments.next(), arguments.next()) {
        (None, None) => Ok(DaemonMode::Normal),
        (Some(argument), None) if argument == "--coordinated-start" => Ok(DaemonMode::Coordinated),
        (Some(argument), None) if argument == "--supervised-stdio" => Ok(DaemonMode::Supervised),
        _ => Err(DaemonError::InvalidArguments),
    }
}

async fn run_async(mode: DaemonMode) -> Result<(), DaemonError> {
    let paths = runtime_paths()?;
    paths.prepare_owner()?;
    let _launch = if mode == DaemonMode::Coordinated {
        None
    } else {
        Some(paths.acquire_launch_lock()?)
    };

    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| DaemonError::RandomUnavailable)?;
    let _writer = CatalogWriterLock::acquire(&paths.writer_lock_path(), nonce)?;
    cleanup_prior_instance(&paths)?;
    let endpoint = paths.endpoint(nonce)?;
    let journal = Arc::new(OperationJournal::open(&paths.operation_journal_path())?);
    let limits = DaemonLimits::default();
    let state = Arc::new(DaemonState::starting());
    let actor = JournalActor::start(
        Arc::clone(&journal),
        limits.control_queue_limit,
        usize::try_from(limits.operation_queue_limit).map_err(|_| DaemonError::InvalidLimits)?,
    )?;
    let actor_handle = actor.handle();
    let mut orchestrator =
        DaemonOrchestrator::new(actor_handle.clone(), Arc::clone(&state), limits)?;
    let service = Arc::new(ControlService::with_state(
        journal,
        nonce,
        Arc::clone(&state),
        limits,
    ));
    let listener = Arc::new(AsyncLocalListener::bind(endpoint.clone())?);
    let discovery = DiscoveryRecord::new(&paths, std::process::id(), &endpoint, nonce)?;
    paths.publish(&discovery)?;
    let discovery = DiscoveryGuard::new(paths, nonce);
    state.set_lifecycle(DaemonLifecycle::Ready);

    let connection_slots = Arc::new(tokio::sync::Semaphore::new(
        usize::try_from(limits.connection_limit).map_err(|_| DaemonError::InvalidLimits)?,
    ));
    let mut connections = tokio::task::JoinSet::new();
    let (submission_tx, mut submission_rx) = tokio::sync::mpsc::channel(
        usize::try_from(limits.operation_queue_limit).map_err(|_| DaemonError::InvalidLimits)?,
    );
    let mut maintenance = tokio::time::interval(limits.maintenance_interval);
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let shutdown = shutdown_signal(mode);
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            _ = maintenance.tick() => {
                if let Err(error) = actor_handle.expire_due(unix_time_ms()?, EXPIRY_BATCH).await {
                    state.set_journal_healthy(false);
                    return Err(error.into());
                }
                if let Err(error) = orchestrator.drain_ready_completions().await {
                    state.set_journal_healthy(false);
                    return Err(error.into());
                }
            }
            completed = orchestrator.complete_next(), if !orchestrator.is_idle() => {
                if let Err(error) = completed {
                    state.set_journal_healthy(false);
                    return Err(error.into());
                }
            }
            admission = submission_rx.recv() => {
                let Some(admission) = admission else { break; };
                if let Err(error) = orchestrator.submit(admission).await
                    && !matches!(error, rootlight_daemon_core::ServiceError::QueueFull | rootlight_daemon_core::ServiceError::NotAccepting)
                {
                    state.set_journal_healthy(false);
                }
            }
            accepted = listener.accept() => {
                let stream = accepted?;
                let permit = match Arc::clone(&connection_slots).try_acquire_owned() {
                    Ok(permit) => permit,
                    Err(_) => continue,
                };
                state.connection_started();
                let service = Arc::clone(&service);
                let actor = actor_handle.clone();
                let submissions = submission_tx.clone();
                let state = Arc::clone(&state);
                connections.spawn(async move {
                    let _permit = permit;
                    let mut stream = stream;
                    let result = handle_connection_async(
                        service,
                        actor,
                        submissions,
                        FrameCodec::default(),
                        &mut stream,
                    )
                    .await;
                    state.connection_finished();
                    result
                });
            }
            joined = connections.join_next(), if !connections.is_empty() => {
                if let Some(Err(error)) = joined {
                    eprintln!("rootlight-daemon: connection task failed: {error}");
                }
            }
        }
    }

    state.set_lifecycle(DaemonLifecycle::Draining);
    drop(discovery);
    drop(listener);
    drop(submission_tx);
    let drain = async {
        let mut admissions_closed = false;
        loop {
            let handlers_done = state.active_connections() == 0 && connections.is_empty();
            if handlers_done && admissions_closed {
                break;
            }
            tokio::select! {
                admission = submission_rx.recv(), if !admissions_closed => {
                    match admission {
                        Some(admission) => {
                            let _ = orchestrator.submit(admission).await;
                        }
                        None => admissions_closed = true,
                    }
                }
                joined = connections.join_next(), if !connections.is_empty() => {
                    if let Some(Err(error)) = joined {
                        eprintln!("rootlight-daemon: connection task failed: {error}");
                    }
                }
                completed = orchestrator.complete_next(), if !orchestrator.is_idle() => {
                    completed?;
                }
            }
        }
        orchestrator.shutdown().await
    };
    tokio::time::timeout(limits.shutdown_grace, drain)
        .await
        .map_err(|_| DaemonError::ShutdownTimedOut)??;
    actor.join()?;
    Ok(())
}

async fn shutdown_signal(mode: DaemonMode) {
    if mode == DaemonMode::Supervised {
        supervised_shutdown().await;
        return;
    }
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    result = tokio::signal::ctrl_c() => { let _ = result; }
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(windows)]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn supervised_shutdown() {
    use tokio::io::AsyncReadExt as _;

    let mut input = tokio::io::stdin();
    let mut byte = [0_u8; 1];
    let mut command = Vec::with_capacity(16);
    loop {
        match input.read(&mut byte).await {
            Ok(0) | Err(_) => return,
            Ok(_) if byte[0] == b'\n' => {
                if command == b"shutdown" {
                    return;
                }
                command.clear();
            }
            Ok(_) if command.len() < 16 => command.push(byte[0]),
            Ok(_) => command.clear(),
        }
    }
}

fn unix_time_ms() -> Result<u64, DaemonError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| DaemonError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| DaemonError::Clock)
}

fn cleanup_prior_instance(paths: &RuntimePaths) -> Result<(), DaemonError> {
    let record = match paths.discover() {
        Ok(record) => record,
        Err(rootlight_runtime::RuntimeError::Io(source))
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    paths.remove_stale_endpoint(record.instance_nonce())?;
    paths.remove_discovery_if_matches(record.instance_nonce())?;
    Ok(())
}

fn runtime_paths() -> Result<RuntimePaths, DaemonError> {
    match (
        env::var_os("ROOTLIGHT_STATE_DIR"),
        env::var_os("ROOTLIGHT_RUNTIME_DIR"),
    ) {
        (None, None) => RuntimePaths::resolve().map_err(DaemonError::Runtime),
        (Some(state), Some(runtime)) if !state.is_empty() && !runtime.is_empty() => {
            RuntimePaths::new(PathBuf::from(state), PathBuf::from(runtime))
                .map_err(DaemonError::Runtime)
        }
        _ => Err(DaemonError::IncompletePathOverride),
    }
}

struct DiscoveryGuard {
    paths: RuntimePaths,
    nonce: [u8; 16],
}

impl DiscoveryGuard {
    const fn new(paths: RuntimePaths, nonce: [u8; 16]) -> Self {
        Self { paths, nonce }
    }
}

impl Drop for DiscoveryGuard {
    fn drop(&mut self) {
        let _ = self.paths.remove_discovery_if_matches(self.nonce);
    }
}

#[derive(Debug, thiserror::Error)]
enum DaemonError {
    #[error("daemon arguments are invalid")]
    InvalidArguments,
    #[error("daemon path overrides must provide both state and runtime directories")]
    IncompletePathOverride,
    #[error("secure random source is unavailable")]
    RandomUnavailable,
    #[error("daemon async runtime setup failed")]
    AsyncRuntime(#[source] std::io::Error),
    #[error("daemon resource limits are invalid")]
    InvalidLimits,
    #[error("daemon shutdown timed out")]
    ShutdownTimedOut,
    #[error("daemon clock is invalid")]
    Clock,
    #[error("daemon orchestration failed")]
    Service(#[from] rootlight_daemon_core::ServiceError),
    #[error("daemon runtime setup failed")]
    Runtime(#[from] rootlight_runtime::RuntimeError),
    #[error("operation journal setup failed")]
    Operations(#[from] rootlight_operations::OperationError),
    #[error("local transport failed")]
    Ipc(#[from] rootlight_ipc::IpcError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_arguments_select_explicit_modes() {
        assert_ne!(DaemonMode::Normal, DaemonMode::Supervised);
        assert_ne!(DaemonMode::Coordinated, DaemonMode::Supervised);
    }
}
