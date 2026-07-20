//! Bounded daemon-owned first-slice service and lifecycle workers.
//!
//! The service worker exclusively owns the in-memory generation set. A
//! separate control worker keeps journal status and cancellation responsive
//! while indexing or a query occupies the service lane.

// The daemon-core port deliberately owns PublicError by value. Keeping that
// exact boundary throughout this private adapter avoids repeated boxing and
// unboxing across every dispatch branch.
#![allow(clippy::result_large_err)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rootlight_daemon_core::{
    ControlRequest, ControlResponse, FirstSliceIpcContext, FirstSliceIpcFuture,
    FirstSliceIpcHandler, FirstSliceIpcRequest, FirstSliceIpcResponse, JournalActorHandle,
    ServiceError, operation_record_to_wire,
};
use rootlight_error::{ErrorCode, NextAction, PublicError};
use rootlight_ids::{ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId};
use rootlight_ir::{
    AnalysisTier, CoverageRecord, CoverageStatus, LineRange, OccurrenceRole, RelationEndpoint,
    RelationPredicate, SourceRef, SourceSpan,
};
use rootlight_operations::{
    Cancellation, OperationError, OperationKind, OperationRecord, OperationState,
    OperationSubmission, PlanHash,
};
use rootlight_protocol::generated::{common::v1 as common, daemon::v1 as daemon};
use rootlight_query::{LocateMode, QueryUsage, RelationDirection, RelationFamily};
use rootlight_service::{
    FirstSliceError, FirstSliceGenerationContext, FirstSliceIndexReceipt, FirstSliceService,
};

const FIRST_SLICE_SCHEMA_MAJOR: u32 = 1;
const FIRST_SLICE_SCHEMA_MINOR: u32 = 0;
const DEFAULT_GENERATION_RETENTION: usize = 8;
const DEFAULT_WORK_QUEUE: usize = 16;
const DEFAULT_CONTROL_QUEUE: usize = 32;
const DEFAULT_OPERATION_METADATA: usize = 256;
const RETRY_AFTER_MS: u32 = 100;
const LIFECYCLE_FINALIZATION_GRACE: Duration = Duration::from_secs(2);
const DEFAULT_RELATIONSHIP_RESULTS: u32 = 100;

type Reply = tokio::sync::oneshot::Sender<Result<FirstSliceIpcResponse, PublicError>>;

enum WorkerCommand {
    Execute {
        request: FirstSliceIpcRequest,
        context: FirstSliceIpcContext,
        reply: Reply,
    },
}

struct PublicationBoundaryHook {
    boundary: PublicationBoundary,
    armed: AtomicBool,
    reached: SyncSender<()>,
    release: Receiver<()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationBoundary {
    AfterAdmission,
    AfterActivation,
    BeforeCompletion,
    AfterSuccess,
}

impl PublicationBoundaryHook {
    fn pause(&self, boundary: PublicationBoundary) -> Result<(), PublicError> {
        if self.boundary != boundary || !self.armed.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        self.reached.try_send(()).map_err(|_| internal_error())?;
        self.release
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| internal_error())
    }
}

/// Cloneable bounded first-slice port used by accepted IPC connections.
#[derive(Clone)]
pub(crate) struct FirstSliceDaemon {
    work: SyncSender<WorkerCommand>,
    control: SyncSender<WorkerCommand>,
}

impl FirstSliceDaemon {
    /// Starts the service and lifecycle worker lanes.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceHostError`] when the first-slice service cannot
    /// initialize or either bounded worker thread cannot start.
    pub(crate) fn start(
        journal: JournalActorHandle,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        Self::start_inner(journal, None)
    }

    #[cfg(test)]
    fn start_with_publication_hook(
        journal: JournalActorHandle,
        hook: PublicationBoundaryHook,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        Self::start_inner(journal, Some(hook))
    }

    fn start_inner(
        journal: JournalActorHandle,
        publication_hook: Option<PublicationBoundaryHook>,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        let service = FirstSliceService::new(DEFAULT_GENERATION_RETENTION)
            .map_err(FirstSliceHostError::Service)?;
        let work_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(FirstSliceHostError::AsyncRuntime)?;
        let control_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(FirstSliceHostError::AsyncRuntime)?;
        let metadata = Arc::new(Mutex::new(OperationMetadataSet::new(
            DEFAULT_OPERATION_METADATA,
        )));
        let stopping = Arc::new(AtomicBool::new(false));
        let (work, work_receiver) = mpsc::sync_channel(DEFAULT_WORK_QUEUE);
        let (control, control_receiver) = mpsc::sync_channel(DEFAULT_CONTROL_QUEUE);
        let work_journal = journal.clone();
        let work_metadata = Arc::clone(&metadata);
        let work_stopping = Arc::clone(&stopping);
        let work_thread = thread::Builder::new()
            .name("rootlight-first-slice".to_owned())
            .spawn(move || {
                service_worker(
                    service,
                    work_journal,
                    work_metadata,
                    work_stopping,
                    work_runtime,
                    work_receiver,
                    publication_hook,
                );
            })
            .map_err(FirstSliceHostError::Thread)?;
        let control_journal = journal.clone();
        let control_stopping = Arc::clone(&stopping);
        let control_thread = thread::Builder::new()
            .name("rootlight-first-slice-control".to_owned())
            .spawn(move || {
                lifecycle_worker(
                    control_journal,
                    metadata,
                    control_stopping,
                    control_runtime,
                    control_receiver,
                );
            })
            .map_err(FirstSliceHostError::Thread)?;
        let daemon = Self {
            work: work.clone(),
            control: control.clone(),
        };
        Ok((
            daemon,
            FirstSliceWorkers {
                work: Some(work),
                control: Some(control),
                stopping,
                journal,
                work_thread: Some(work_thread),
                control_thread: Some(control_thread),
            },
        ))
    }

    fn sender(&self, request: &FirstSliceIpcRequest) -> &SyncSender<WorkerCommand> {
        if matches!(request, FirstSliceIpcRequest::RepositoryOperationStatus(_)) {
            &self.control
        } else {
            &self.work
        }
    }
}

impl FirstSliceIpcHandler for FirstSliceDaemon {
    fn dispatch(
        &self,
        request: FirstSliceIpcRequest,
        context: FirstSliceIpcContext,
    ) -> FirstSliceIpcFuture {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        let send = self
            .sender(&request)
            .try_send(WorkerCommand::Execute {
                request,
                context,
                reply,
            })
            .map_err(map_queue_error);
        Box::pin(async move {
            send?;
            receiver.await.unwrap_or_else(|_| Err(internal_error()))
        })
    }
}

/// Join owner for both process-lifetime first-slice workers.
pub(crate) struct FirstSliceWorkers {
    work: Option<SyncSender<WorkerCommand>>,
    control: Option<SyncSender<WorkerCommand>>,
    stopping: Arc<AtomicBool>,
    journal: JournalActorHandle,
    work_thread: Option<JoinHandle<()>>,
    control_thread: Option<JoinHandle<()>>,
}

impl FirstSliceWorkers {
    /// Stops both lanes while accepted connection handlers drain concurrently.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceHostError`] when journal interruption fails, a
    /// worker panics, or cooperative shutdown exceeds the supplied grace.
    pub(crate) async fn stop(
        mut self,
        deadline: tokio::time::Instant,
    ) -> Result<(), FirstSliceHostError> {
        self.stopping.store(true, Ordering::Release);
        // This runs only during global daemon shutdown. Interrupting the full
        // bounded journal batch is intentional: no operation kind may outlive
        // the process-wide worker drain.
        tokio::time::timeout_at(deadline, self.journal.interrupt(DEFAULT_OPERATION_METADATA))
            .await
            .map_err(|_| FirstSliceHostError::ShutdownTimedOut)?
            .map_err(FirstSliceHostError::Journal)?;
        self.work.take();
        self.control.take();
        let work = self.work_thread.take();
        let control = self.control_thread.take();
        let (joined, completion) = tokio::sync::oneshot::channel();
        thread::Builder::new()
            .name("rootlight-first-slice-join".to_owned())
            .spawn(move || {
                let result = [control, work]
                    .into_iter()
                    .flatten()
                    .try_for_each(|thread| {
                        thread
                            .join()
                            .map_err(|_| FirstSliceHostError::ThreadPanicked)
                    });
                let _ = joined.send(result);
            })
            .map_err(FirstSliceHostError::Thread)?;
        tokio::time::timeout_at(deadline, completion)
            .await
            .map_err(|_| FirstSliceHostError::ShutdownTimedOut)?
            .map_err(|_| FirstSliceHostError::ThreadPanicked)??;
        Ok(())
    }
}

impl Drop for FirstSliceWorkers {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        self.work.take();
        self.control.take();
    }
}

/// Startup failure for the daemon-owned first-slice workers.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FirstSliceHostError {
    /// The bounded service could not initialize.
    #[error("first-slice service failed to initialize")]
    Service(FirstSliceError),
    /// A dedicated bounded worker thread could not start.
    #[error("first-slice worker thread failed to start")]
    Thread(#[source] std::io::Error),
    /// A private current-thread runtime could not initialize.
    #[error("first-slice async runtime failed to initialize")]
    AsyncRuntime(#[source] std::io::Error),
    /// The serialized journal actor rejected shutdown or lifecycle work.
    #[error("first-slice journal request failed")]
    Journal(#[source] ServiceError),
    /// A dedicated first-slice worker panicked.
    #[error("first-slice worker panicked")]
    ThreadPanicked,
    /// Cooperative worker shutdown exceeded the daemon grace period.
    #[error("first-slice worker shutdown timed out")]
    ShutdownTimedOut,
}

#[derive(Debug, Clone, Copy)]
struct OperationMetadata {
    started_unix_ms: u64,
    receipt: Option<FirstSliceIndexReceipt>,
    publication: PublicationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PublicationState {
    None,
    Staged,
    Committed,
    FailedClosed,
}

#[derive(Debug)]
struct OperationMetadataSet {
    maximum: usize,
    records: BTreeMap<OperationId, OperationMetadata>,
}

impl OperationMetadataSet {
    fn new(maximum: usize) -> Self {
        Self {
            maximum,
            records: BTreeMap::new(),
        }
    }

    fn reserve(&mut self, operation: OperationId, started_unix_ms: u64) -> Result<(), PublicError> {
        if self.records.contains_key(&operation) {
            return Ok(());
        }
        if self.records.len() >= self.maximum {
            return Err(resource_exhausted());
        }
        self.records.insert(
            operation,
            OperationMetadata {
                started_unix_ms,
                receipt: None,
                publication: PublicationState::None,
            },
        );
        Ok(())
    }

    fn stage(&mut self, operation: OperationId, receipt: FirstSliceIndexReceipt) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.receipt = Some(receipt);
            metadata.publication = PublicationState::Staged;
        }
    }

    fn commit(&mut self, operation: OperationId) -> Result<(), PublicError> {
        let metadata = self
            .records
            .get_mut(&operation)
            .ok_or_else(internal_error)?;
        if metadata.publication != PublicationState::Staged || metadata.receipt.is_none() {
            return Err(internal_error());
        }
        metadata.publication = PublicationState::Committed;
        Ok(())
    }

    fn discard(&mut self, operation: OperationId) -> Result<(), PublicError> {
        let metadata = self
            .records
            .get_mut(&operation)
            .ok_or_else(internal_error)?;
        if metadata.publication != PublicationState::Staged || metadata.receipt.is_none() {
            return Err(internal_error());
        }
        metadata.receipt = None;
        metadata.publication = PublicationState::None;
        Ok(())
    }

    fn fail_closed(&mut self, operation: OperationId) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.publication = PublicationState::FailedClosed;
        }
    }

    fn remove_unpublished(&mut self, operation: OperationId) {
        if self
            .records
            .get(&operation)
            .is_some_and(|metadata| metadata.publication == PublicationState::None)
        {
            self.records.remove(&operation);
        }
    }
}

fn service_worker(
    mut service: FirstSliceService,
    journal: JournalActorHandle,
    metadata: Arc<Mutex<OperationMetadataSet>>,
    stopping: Arc<AtomicBool>,
    runtime: tokio::runtime::Runtime,
    commands: Receiver<WorkerCommand>,
    publication_hook: Option<PublicationBoundaryHook>,
) {
    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let command = match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match command {
            WorkerCommand::Execute {
                request,
                context,
                reply,
            } => {
                let result = execute_service_request(
                    &mut service,
                    &journal,
                    metadata.as_ref(),
                    &runtime,
                    request,
                    context,
                    publication_hook.as_ref(),
                );
                let _ = reply.send(result);
            }
        }
    }
}

fn lifecycle_worker(
    journal: JournalActorHandle,
    metadata: Arc<Mutex<OperationMetadataSet>>,
    stopping: Arc<AtomicBool>,
    runtime: tokio::runtime::Runtime,
    commands: Receiver<WorkerCommand>,
) {
    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let command = match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match command {
            WorkerCommand::Execute {
                request: FirstSliceIpcRequest::RepositoryOperationStatus(request),
                context,
                reply,
            } => {
                let result = repository_operation_status(
                    &journal,
                    metadata.as_ref(),
                    &runtime,
                    request,
                    &context,
                )
                .map(FirstSliceIpcResponse::RepositoryOperationStatus);
                let _ = reply.send(result);
            }
            WorkerCommand::Execute { reply, .. } => {
                let _ = reply.send(Err(internal_error()));
            }
        }
    }
}

fn execute_service_request(
    service: &mut FirstSliceService,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    runtime: &tokio::runtime::Runtime,
    request: FirstSliceIpcRequest,
    context: FirstSliceIpcContext,
    publication_hook: Option<&PublicationBoundaryHook>,
) -> Result<FirstSliceIpcResponse, PublicError> {
    context
        .cancellation
        .check()
        .map_err(|_| cancelled_error())?;
    match request {
        FirstSliceIpcRequest::RepositoryIndex(request) => repository_index(
            service,
            journal,
            metadata,
            runtime,
            request,
            &context,
            publication_hook,
        )
        .map(FirstSliceIpcResponse::RepositoryIndex),
        FirstSliceIpcRequest::CodeLocate(request) => {
            code_locate(service, request, &context).map(FirstSliceIpcResponse::CodeLocate)
        }
        FirstSliceIpcRequest::SymbolExplain(request) => {
            symbol_explain(service, request, &context).map(FirstSliceIpcResponse::SymbolExplain)
        }
        FirstSliceIpcRequest::SourceRead(request) => {
            source_read(service, request, &context).map(FirstSliceIpcResponse::SourceRead)
        }
        FirstSliceIpcRequest::RepositoryList(request) => {
            repository_list(service, request).map(FirstSliceIpcResponse::RepositoryList)
        }
        FirstSliceIpcRequest::RepositoryStatus(request) => {
            repository_status(service, request).map(FirstSliceIpcResponse::RepositoryStatus)
        }
        FirstSliceIpcRequest::SymbolRelationships(request) => {
            symbol_relationships(service, request, &context)
                .map(FirstSliceIpcResponse::SymbolRelationships)
        }
        FirstSliceIpcRequest::RepositoryOperationStatus(_) => Err(internal_error()),
    }
}

fn repository_index(
    service: &mut FirstSliceService,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    runtime: &tokio::runtime::Runtime,
    request: daemon::RepositoryIndexRequest,
    context: &FirstSliceIpcContext,
    publication_hook: Option<&PublicationBoundaryHook>,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    let operation = parse_operation(request.operation.as_ref())?;
    let lifecycle_deadline = lifecycle_deadline(context.deadline)?;
    let deadline_unix_ms = deadline_unix_ms(context.deadline)?;
    let detached = request.detached;
    let submission = OperationSubmission::new(
        operation,
        OperationKind::RepositoryIndex,
        PlanHash::from_bytes(*blake3::hash(request.root.as_bytes()).as_bytes()),
        context.client_instance_id,
        detached,
        Some(deadline_unix_ms),
        (!detached).then_some(deadline_unix_ms),
    )
    .map_err(|error| operation_error(&error, Some(operation)))?;
    match journal_call(
        runtime,
        context.deadline,
        journal.control(ControlRequest::OperationStatus(operation)),
    ) {
        Ok(ControlResponse::OperationStatus(existing)) => {
            // The request deadline is transport-local and is recomputed for a
            // retry. Reuse the durable first submission's deadline while still
            // checking every caller-controlled immutable field.
            let retry = OperationSubmission {
                deadline_unix_ms: existing.deadline_unix_ms,
                lease_expires_unix_ms: existing.lease_expires_unix_ms,
                ..submission
            };
            let existing = journal_call(runtime, context.deadline, journal.retry_status(retry))?;
            return retry_index_response(metadata, existing);
        }
        Err(error) if error.code() == ErrorCode::NotFound => {}
        Ok(_) => return Err(internal_error()),
        Err(error) => return Err(error),
    }
    let started_unix_ms = unix_time_ms()?;
    lock_metadata(metadata)?.reserve(operation, started_unix_ms)?;
    let submitted =
        match journal_lifecycle_call(runtime, journal.submit_until(submission, context.deadline)) {
            Ok(submitted) => submitted,
            Err(error) => {
                lock_metadata(metadata)?.remove_unpublished(operation);
                return Err(error);
            }
        };
    if !submitted.inserted {
        return retry_index_response(metadata, submitted.operation);
    }
    if let Some(admission) = context.index_admission.as_ref() {
        admission.mark_inserted();
    }
    if let Some(hook) = publication_hook
        && let Err(error) = hook.pause(PublicationBoundary::AfterAdmission)
    {
        return Err(error);
    }
    if propagate_peer_cancellation(runtime, journal, operation, context, lifecycle_deadline)? {
        return Err(cancelled_error());
    }
    let (_, cancellation) = journal_lifecycle_call(
        runtime,
        journal.activate_operation_until(operation, lifecycle_deadline),
    )?;
    // The journal owns cancellation fan-out while the IPC boundary owns the
    // process-local work deadline. Installing it on this exact token keeps
    // later journal cancellation linked to every synchronous service stage.
    if let Err(error) = bind_journal_cancellation_deadline(&cancellation, context.deadline) {
        finish_failed_index(
            runtime,
            lifecycle_deadline,
            journal,
            operation,
            &cancellation,
            &error,
        )?;
        return Err(error);
    }
    if let Some(hook) = publication_hook
        && let Err(error) = hook.pause(PublicationBoundary::AfterActivation)
    {
        finish_failed_index(
            runtime,
            lifecycle_deadline,
            journal,
            operation,
            &cancellation,
            &error,
        )?;
        return Err(error);
    }
    match service.prepare_rust_fixture(&PathBuf::from(request.root), &cancellation) {
        Ok(prepared) => {
            if propagate_peer_cancellation(
                runtime,
                journal,
                operation,
                context,
                lifecycle_deadline,
            )? {
                finish_failed_index(
                    runtime,
                    lifecycle_deadline,
                    journal,
                    operation,
                    &cancellation,
                    &cancelled_error(),
                )?;
                return Err(cancelled_error());
            }
            let staged = match service.stage_prepared(prepared, &cancellation) {
                Ok(staged) => staged,
                Err(error) => {
                    let public = service_error(error);
                    finish_failed_index(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        operation,
                        &cancellation,
                        &public,
                    )?;
                    return Err(public);
                }
            };
            let staged_receipt = staged.receipt();
            lock_metadata(metadata)?.stage(operation, staged_receipt);
            if let Some(hook) = publication_hook
                && let Err(error) = hook.pause(PublicationBoundary::BeforeCompletion)
            {
                service.discard_staged(staged).map_err(service_error)?;
                lock_metadata(metadata)?.fail_closed(operation);
                return Err(error);
            }
            propagate_peer_cancellation(runtime, journal, operation, context, lifecycle_deadline)?;
            let completion = match context.index_admission.clone() {
                Some(admission) => journal_lifecycle_call(
                    runtime,
                    journal.complete_publication_with_admission_until(
                        operation,
                        admission,
                        lifecycle_deadline,
                    ),
                ),
                None => journal_lifecycle_call(
                    runtime,
                    journal.complete_publication_until(operation, lifecycle_deadline),
                ),
            };
            let operation_record = match completion {
                Ok(record) => record,
                Err(error) => {
                    if service.discard_staged(staged).is_err() {
                        lock_metadata(metadata)?.fail_closed(operation);
                        return Err(internal_error());
                    }
                    if error.code() != ErrorCode::Cancelled {
                        lock_metadata(metadata)?.fail_closed(operation);
                    } else {
                        lock_metadata(metadata)?.discard(operation)?;
                    }
                    return Err(error);
                }
            };
            if operation_record.state != OperationState::Succeeded {
                service.discard_staged(staged).map_err(service_error)?;
                return Err(cancelled_error());
            }
            if let Some(hook) = publication_hook
                && let Err(error) = hook.pause(PublicationBoundary::AfterSuccess)
            {
                service.discard_staged(staged).map_err(service_error)?;
                lock_metadata(metadata)?.fail_closed(operation);
                return Err(error);
            }
            let receipt = match service.commit_staged(staged) {
                Ok(receipt) if receipt == staged_receipt => receipt,
                Ok(_) | Err(_) => {
                    lock_metadata(metadata)?.fail_closed(operation);
                    return Err(internal_error());
                }
            };
            lock_metadata(metadata)?.commit(operation)?;
            Ok(index_response(receipt, &operation_record))
        }
        Err(error) => {
            let public = service_error(error);
            finish_failed_index(
                runtime,
                lifecycle_deadline,
                journal,
                operation,
                &cancellation,
                &public,
            )?;
            Err(public)
        }
    }
}

fn propagate_peer_cancellation(
    runtime: &tokio::runtime::Runtime,
    journal: &JournalActorHandle,
    operation: OperationId,
    context: &FirstSliceIpcContext,
    lifecycle_deadline: Instant,
) -> Result<bool, PublicError> {
    if context.cancellation.reason()
        != Some(rootlight_operations::CancellationReason::ClientRequest)
    {
        return Ok(false);
    }
    let response = journal_lifecycle_call(
        runtime,
        journal.control_until(
            ControlRequest::OperationCancel(operation),
            lifecycle_deadline,
        ),
    )?;
    match response {
        ControlResponse::OperationCancel { .. } => Ok(true),
        ControlResponse::Error(error) => Err(error),
        _ => Err(internal_error()),
    }
}

fn finish_failed_index(
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
    journal: &JournalActorHandle,
    operation: OperationId,
    cancellation: &Cancellation,
    error: &PublicError,
) -> Result<(), PublicError> {
    if let Some(reason) = cancellation.reason() {
        journal_lifecycle_call(
            runtime,
            journal.finish_operation_until(operation, Some(reason), deadline),
        )?;
    } else {
        journal_lifecycle_call(
            runtime,
            journal.fail_operation_until(operation, error.clone(), deadline),
        )?;
    }
    Ok(())
}

fn bind_journal_cancellation_deadline(
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<(), PublicError> {
    if cancellation.has_deadline() {
        return Err(internal_error());
    }
    cancellation.extend_deadline(deadline).map_err(|_| {
        if cancellation.reason().is_some() {
            cancelled_error()
        } else {
            internal_error()
        }
    })
}

fn retry_index_response(
    metadata: &Mutex<OperationMetadataSet>,
    operation: OperationRecord,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    let metadata = lock_metadata(metadata)?
        .records
        .get(&operation.operation)
        .copied()
        .ok_or_else(unsupported_restart_state)?;
    if metadata.publication == PublicationState::FailedClosed {
        return Err(internal_error());
    }
    match operation.state {
        OperationState::Queued | OperationState::Running | OperationState::Cancelling => {
            return Err(operation_in_progress());
        }
        OperationState::Failed => {
            return Err(operation.error.ok_or_else(internal_error)?);
        }
        OperationState::Cancelled => {
            return Err(terminal_operation_error(
                operation.operation,
                "repository index was cancelled",
            ));
        }
        OperationState::Interrupted => {
            return Err(terminal_operation_error(
                operation.operation,
                "repository index was interrupted",
            ));
        }
        OperationState::Succeeded => {}
    }
    if !matches!(
        metadata.publication,
        PublicationState::Staged | PublicationState::Committed
    ) {
        return Err(internal_error());
    }
    let receipt = metadata.receipt.ok_or_else(internal_error)?;
    Ok(index_response(receipt, &operation))
}

fn index_response(
    receipt: FirstSliceIndexReceipt,
    operation: &OperationRecord,
) -> daemon::RepositoryIndexResponse {
    daemon::RepositoryIndexResponse {
        schema_version: Some(schema_version()),
        repository: Some(repository_to_wire(receipt.repository)),
        operation: Some(operation_to_wire(operation.operation)),
        state: operation_state_to_wire(operation.state) as i32,
        revision: operation.revision,
        parent_generation: receipt.parent.map(generation_to_wire),
        published_generation: Some(generation_to_wire(receipt.generation)),
        discovered_inputs: receipt.discovered_inputs,
        indexed_files: receipt.indexed_files,
        entities: receipt.entities,
        elapsed_micros: receipt.elapsed_micros,
    }
}

fn repository_operation_status(
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    runtime: &tokio::runtime::Runtime,
    request: daemon::RepositoryOperationStatusRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::RepositoryOperationStatusResponse, PublicError> {
    if request.wait_ms.unwrap_or(0) != 0 || request.after_revision.is_some() {
        return Err(unsupported_capability());
    }
    context
        .cancellation
        .check()
        .map_err(|_| cancelled_error())?;
    let operation = parse_operation(request.operation.as_ref())?;
    let action = daemon::RepositoryOperationAction::try_from(request.action)
        .map_err(|_| invalid_argument())?;
    let control = if action == daemon::RepositoryOperationAction::RepositoryOperationCancel {
        ControlRequest::OperationCancel(operation)
    } else {
        ControlRequest::OperationStatus(operation)
    };
    let response = if action == daemon::RepositoryOperationAction::RepositoryOperationCancel {
        journal_lifecycle_call(runtime, journal.control_until(control, context.deadline))?
    } else {
        journal_call(runtime, context.deadline, journal.control(control))?
    };
    let record = match response {
        ControlResponse::OperationStatus(record)
        | ControlResponse::OperationCancel {
            operation: record, ..
        } => record,
        ControlResponse::Error(error) => return Err(error),
        _ => return Err(internal_error()),
    };
    if record.kind != OperationKind::RepositoryIndex {
        return Err(not_found());
    }
    let metadata = lock_metadata(metadata)?
        .records
        .get(&operation)
        .copied()
        .ok_or_else(unsupported_restart_state)?;
    if metadata.publication == PublicationState::FailedClosed {
        return Err(internal_error());
    }
    let published_generation = if record.state == OperationState::Succeeded {
        if !matches!(
            metadata.publication,
            PublicationState::Staged | PublicationState::Committed
        ) {
            return Err(internal_error());
        }
        Some(metadata.receipt.ok_or_else(internal_error)?.generation)
    } else {
        None
    };
    Ok(daemon::RepositoryOperationStatusResponse {
        schema_version: Some(schema_version()),
        operation: Some(operation_record_to_wire(&record)),
        published_generation: published_generation.map(generation_to_wire),
        started_unix_ms: metadata.started_unix_ms,
        peak_rss_bytes: 0,
        written_bytes: 0,
        files_examined: metadata
            .receipt
            .map_or(0, |receipt| receipt.discovered_inputs),
        retry_after_ms: (!record.state.is_terminal()).then_some(RETRY_AFTER_MS),
    })
}

fn code_locate(
    service: &FirstSliceService,
    request: daemon::CodeLocateRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::CodeLocateResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mode = match daemon::FirstSliceLocateMode::try_from(request.mode)
        .map_err(|_| invalid_argument())?
    {
        daemon::FirstSliceLocateMode::FirstSliceLocateExact => LocateMode::Exact,
        daemon::FirstSliceLocateMode::FirstSliceLocatePrefix => LocateMode::Prefix,
        daemon::FirstSliceLocateMode::FirstSliceLocateText => LocateMode::Text,
        daemon::FirstSliceLocateMode::FirstSliceLocateSafeRegex => LocateMode::SafeRegex,
        daemon::FirstSliceLocateMode::FirstSliceLocateGlob => LocateMode::Glob,
        daemon::FirstSliceLocateMode::Unspecified => return Err(invalid_argument()),
    };
    let response = service
        .code_locate(
            generation.generation,
            request.query,
            mode,
            usize::try_from(request.maximum_results).map_err(|_| invalid_argument())?,
            &context.cancellation,
        )
        .map_err(service_error)?;
    let mut hits = Vec::new();
    hits.try_reserve_exact(response.data.hits.len())
        .map_err(|_| resource_exhausted())?;
    for hit in response.data.hits {
        hits.push(daemon::FirstSliceLocateHit {
            symbol: Some(symbol_to_wire(hit.symbol)),
            file: Some(file_to_wire(hit.file)),
            identifier: hit.identifier,
            qualified_name: hit.qualified_name,
            path: hit.path,
            kind: hit.kind,
            language: hit.language,
            tier: tier_label_to_wire(&hit.tier) as i32,
            generated: hit.generated,
            score: score_to_wire(hit.relevance_score),
            source: hit.source.as_ref().map(source_ref_to_wire),
        });
    }
    Ok(daemon::CodeLocateResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(
            generation,
            &response.usage,
            &response.data.coverage,
        )),
        hits,
        matched_candidates: response.data.matched_candidates,
        truncated: response.data.truncated,
    })
}

fn symbol_explain(
    service: &FirstSliceService,
    request: daemon::SymbolExplainRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::SymbolExplainResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mut symbols = Vec::new();
    symbols
        .try_reserve_exact(request.symbols.len())
        .map_err(|_| resource_exhausted())?;
    let mut usage = UsageAccumulator::default();
    let mut coverage = Vec::new();
    for symbol in request.symbols {
        let symbol = parse_symbol(Some(&symbol))?;
        let response = service
            .symbol_explain(generation.generation, symbol, &context.cancellation)
            .map_err(service_error)?;
        usage.add(&response.usage)?;
        coverage.extend(response.data.coverage.iter().cloned());
        let entity = response.data.entity;
        let definition = entity
            .evidence
            .source
            .as_ref()
            .ok_or_else(incomplete_coverage)?;
        let mut relations = RelationCounts::default();
        for relation in &response.data.relations {
            relations.observe(symbol, relation);
        }
        for occurrence in &response.data.occurrences {
            if occurrence.role == OccurrenceRole::Reference {
                relations.references_exact = relations.references_exact.saturating_add(1);
            }
        }
        let provider = response.data.provenance.producer.name().to_owned();
        let evidence = enum_label(response.data.provenance.producer_kind)?;
        symbols.push(daemon::FirstSliceSymbolExplanation {
            symbol: Some(symbol_to_wire(symbol)),
            kind: enum_label(entity.kind)?,
            display_name: entity.display_name,
            signature: None,
            definition: Some(source_ref_to_wire(definition)),
            outbound_exact: relations.outbound_exact,
            outbound_candidates: 0,
            inbound_exact: relations.inbound_exact,
            inbound_candidates: 0,
            references_exact: relations.references_exact,
            provider,
            evidence,
            confidence: if entity.evidence.source.is_some() {
                1_000
            } else {
                0
            },
        });
    }
    Ok(daemon::SymbolExplainResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(generation, &usage.finish(), &coverage)),
        symbols,
        unresolved_symbols: Vec::new(),
        truncated: false,
    })
}

fn symbol_relationships(
    service: &FirstSliceService,
    request: daemon::SymbolRelationshipsRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::SymbolRelationshipsResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mut seeds = BTreeSet::new();
    for seed in &request.seeds {
        seeds.insert(parse_symbol(Some(seed))?);
    }
    let mut families = Vec::new();
    families
        .try_reserve_exact(request.relations.len())
        .map_err(|_| resource_exhausted())?;
    for relation in &request.relations {
        let family = RelationFamily::from_label(relation).ok_or_else(invalid_argument)?;
        if !families.contains(&family) {
            families.push(family);
        }
    }
    let direction = match request.direction.as_deref() {
        Some(label) => Some(RelationDirection::from_label(label).ok_or_else(invalid_argument)?),
        None => None,
    };
    let min_confidence =
        u16::try_from(request.min_confidence.unwrap_or(0)).map_err(|_| invalid_argument())?;
    let max_results = usize::try_from(request.max_results.unwrap_or(DEFAULT_RELATIONSHIP_RESULTS))
        .map_err(|_| invalid_argument())?;
    let response = service
        .symbol_relationships(
            generation.generation,
            seeds,
            families,
            direction,
            min_confidence,
            max_results,
            &context.cancellation,
        )
        .map_err(service_error)?;
    let mut groups = Vec::new();
    groups
        .try_reserve_exact(response.data.groups.len())
        .map_err(|_| resource_exhausted())?;
    for group in response.data.groups {
        let items = group
            .items
            .iter()
            .map(|item| daemon::FirstSliceRelationshipTarget {
                symbol: Some(symbol_to_wire(item.symbol)),
                confidence: u32::from(item.confidence),
                source_refs: item.source_refs.iter().map(source_ref_to_wire).collect(),
            })
            .collect();
        groups.push(daemon::FirstSliceRelationshipGroup {
            seed: Some(symbol_to_wire(group.seed)),
            relation: group.family.as_str().to_owned(),
            direction: group.direction.as_str().to_owned(),
            items,
            total_count: u64::from(group.total_count),
        });
    }
    Ok(daemon::SymbolRelationshipsResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(generation, &response.usage, &[])),
        groups,
        returned_edges: u64::from(response.data.returned_edges),
        total_edges: u64::from(response.data.total_edges),
        exact: response.data.exact,
        truncated: response.data.truncated,
    })
}

fn source_read(
    service: &FirstSliceService,
    request: daemon::SourceReadRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::SourceReadResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mut references = Vec::new();
    references
        .try_reserve_exact(request.references.len())
        .map_err(|_| resource_exhausted())?;
    for reference in &request.references {
        let reference = source_ref_from_wire(reference)?;
        if reference.repository() != repository || reference.generation() != generation.generation {
            return Err(stale_generation());
        }
        references.push(reference);
    }
    let response = service
        .source_read(generation.generation, references, &context.cancellation)
        .map_err(service_error)?;
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(response.data.chunks.len())
        .map_err(|_| resource_exhausted())?;
    for chunk in response.data.chunks {
        chunks.push(daemon::FirstSliceSourceChunk {
            source: Some(source_ref_to_wire(&chunk.reference)),
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line: chunk.start_line,
            end_line: chunk.end_line,
            content: chunk.text,
            content_hash: Some(content_hash_to_wire(chunk.content_hash)),
            language: chunk.language,
            generated: chunk.generated,
        });
    }
    Ok(daemon::SourceReadResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(generation, &response.usage, &[])),
        chunks,
        total_source_bytes: response.usage.source_bytes,
        truncated: false,
    })
}

fn repository_list(
    service: &FirstSliceService,
    request: daemon::RepositoryListRequest,
) -> Result<daemon::RepositoryListResponse, PublicError> {
    let mut repositories = Vec::new();
    for entry in service.list_repositories() {
        repositories.push(daemon::RepositoryListEntry {
            repository: Some(repository_to_wire(entry.repository)),
            active_generation: Some(generation_to_wire(entry.active_generation)),
            languages: entry.languages,
            structural_freshness: entry.structural_freshness,
            semantic_freshness: entry.semantic_freshness,
            state: entry.state,
        });
    }
    // The service enumerates every known repository; honor the optional bound.
    // The optional query is validated at the protocol boundary but not applied
    // because repositories are opaque process-local identities with no text
    // field to match.
    if let Some(max_results) = request.max_results {
        repositories.truncate(usize::try_from(max_results).map_err(|_| invalid_argument())?);
    }
    Ok(daemon::RepositoryListResponse { repositories })
}

fn repository_status(
    service: &FirstSliceService,
    request: daemon::RepositoryStatusRequest,
) -> Result<daemon::RepositoryStatusResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    // The status reports the repository's active generation. The generation
    // selector is validated at the protocol boundary; the active generation is
    // returned regardless of the selector.
    let status = service
        .repository_status(repository)
        .map_err(service_error)?;
    let coverage = status
        .coverage
        .into_iter()
        .map(|entry| daemon::RepositoryCoverageEntry {
            language: entry.language,
            tier: entry.tier,
            status: entry.status,
            discovered_files: entry.discovered_files,
            indexed_files: entry.indexed_files,
        })
        .collect();
    Ok(daemon::RepositoryStatusResponse {
        repository: Some(repository_to_wire(status.repository)),
        active_generation: Some(generation_to_wire(status.active_generation)),
        parent_generation: status.parent_generation.map(generation_to_wire),
        structural_freshness: status.structural_freshness,
        semantic_freshness: status.semantic_freshness,
        state: status.state,
        coverage,
    })
}

#[derive(Debug, Default)]
struct RelationCounts {
    outbound_exact: u64,
    inbound_exact: u64,
    references_exact: u64,
}

impl RelationCounts {
    fn observe(&mut self, symbol: SymbolId, relation: &rootlight_ir::RelationRecord) {
        if relation.predicate == RelationPredicate::Calls {
            if relation.subject == RelationEndpoint::Entity(symbol) {
                self.outbound_exact = self.outbound_exact.saturating_add(1);
            }
            if relation.object == RelationEndpoint::Entity(symbol) {
                self.inbound_exact = self.inbound_exact.saturating_add(1);
            }
        }
        if relation.predicate == RelationPredicate::RefersTo {
            self.references_exact = self.references_exact.saturating_add(1);
        }
    }
}

#[derive(Debug, Default)]
struct UsageAccumulator {
    rows: u64,
    edges: u64,
    results: u64,
    source_bytes: u64,
    json_bytes: u64,
    estimated_tokens: u64,
    memory_bytes: u64,
    elapsed_micros: u64,
}

impl UsageAccumulator {
    fn add(&mut self, usage: &QueryUsage) -> Result<(), PublicError> {
        self.rows = checked_add(self.rows, usage.rows)?;
        self.edges = checked_add(self.edges, usage.edges)?;
        self.results = checked_add(self.results, usage.results)?;
        self.source_bytes = checked_add(self.source_bytes, usage.source_bytes)?;
        self.json_bytes = checked_add(self.json_bytes, usage.json_bytes)?;
        self.estimated_tokens = checked_add(self.estimated_tokens, usage.estimated_tokens)?;
        self.memory_bytes = checked_add(self.memory_bytes, usage.memory_bytes)?;
        self.elapsed_micros = checked_add(self.elapsed_micros, usage.elapsed_micros)?;
        Ok(())
    }

    fn finish(&self) -> QueryUsage {
        QueryUsage {
            rows: self.rows,
            edges: self.edges,
            results: self.results,
            source_bytes: self.source_bytes,
            json_bytes: self.json_bytes,
            estimated_tokens: self.estimated_tokens,
            token_accounting: rootlight_query::TokenAccountingProfile::Utf8ByteUpperBoundV1,
            memory_bytes: self.memory_bytes,
            elapsed_micros: self.elapsed_micros,
        }
    }
}

fn query_context(
    generation: FirstSliceGenerationContext,
    usage: &QueryUsage,
    coverage: &[CoverageRecord],
) -> daemon::FirstSliceQueryContext {
    let (tier, status, skipped) = aggregate_coverage(coverage, generation.receipt);
    daemon::FirstSliceQueryContext {
        repository: Some(repository_to_wire(generation.repository)),
        generation: Some(generation_to_wire(generation.generation)),
        parent_generation: generation.parent.map(generation_to_wire),
        active_generation: generation.active,
        tier: analysis_tier_to_wire(tier) as i32,
        coverage_status: coverage_status_to_wire(status) as i32,
        skipped_inputs: skipped,
        usage: Some(daemon::FirstSliceQueryUsage {
            rows: usage.rows,
            edges: usage.edges,
            results: usage.results,
            source_bytes: usage.source_bytes,
            json_bytes: usage.json_bytes,
            estimated_tokens: usage.estimated_tokens,
            elapsed_micros: usage.elapsed_micros,
        }),
    }
}

fn aggregate_coverage(
    coverage: &[CoverageRecord],
    receipt: FirstSliceIndexReceipt,
) -> (AnalysisTier, CoverageStatus, u64) {
    if coverage.is_empty() {
        return (
            AnalysisTier::TierD,
            if receipt.discovered_inputs == receipt.indexed_files {
                CoverageStatus::Complete
            } else {
                CoverageStatus::Bounded
            },
            receipt
                .discovered_inputs
                .saturating_sub(receipt.indexed_files),
        );
    }
    let mut tier = AnalysisTier::TierA;
    let mut status = CoverageStatus::Complete;
    let mut skipped = 0_u64;
    for record in coverage {
        tier = weaker_tier(tier, record.tier);
        status = weaker_coverage(status, record.status);
        skipped = skipped.saturating_add(record.skipped);
    }
    (tier, status, skipped)
}

const fn weaker_tier(left: AnalysisTier, right: AnalysisTier) -> AnalysisTier {
    use AnalysisTier::{TierA, TierB, TierC, TierD};
    match (left, right) {
        (TierD, _) | (_, TierD) => TierD,
        (TierC, _) | (_, TierC) => TierC,
        (TierB, _) | (_, TierB) => TierB,
        _ => TierA,
    }
}

const fn weaker_coverage(left: CoverageStatus, right: CoverageStatus) -> CoverageStatus {
    use CoverageStatus::{Bounded, Complete, Sampled, Unknown};
    match (left, right) {
        (Unknown, _) | (_, Unknown) => Unknown,
        (Sampled, _) | (_, Sampled) => Sampled,
        (Bounded, _) | (_, Bounded) => Bounded,
        _ => Complete,
    }
}

fn source_ref_from_wire(reference: &daemon::FirstSliceSourceRef) -> Result<SourceRef, PublicError> {
    let repository = parse_repository(reference.repository.as_ref())?;
    let generation = parse_generation(reference.generation.as_ref())?;
    let file = parse_file(reference.file.as_ref())?;
    let span = SourceSpan::new(file, reference.start_byte, reference.end_byte)
        .map_err(|_| invalid_argument())?;
    let content_hash = parse_content_hash(reference.content_hash.as_ref())?;
    let line_hint = match (reference.start_line, reference.end_line) {
        (None, None) => None,
        (Some(start), Some(end)) => {
            Some(LineRange::new(start, end).map_err(|_| invalid_argument())?)
        }
        _ => return Err(invalid_argument()),
    };
    Ok(SourceRef::new(
        repository,
        generation,
        span,
        content_hash,
        line_hint,
    ))
}

fn source_ref_to_wire(reference: &SourceRef) -> daemon::FirstSliceSourceRef {
    let span = reference.span();
    daemon::FirstSliceSourceRef {
        repository: Some(repository_to_wire(reference.repository())),
        generation: Some(generation_to_wire(reference.generation())),
        file: Some(file_to_wire(span.file())),
        start_byte: span.start_byte(),
        end_byte: span.end_byte(),
        content_hash: Some(content_hash_to_wire(reference.content_hash())),
        start_line: reference.line_hint().map(LineRange::start_line),
        end_line: reference.line_hint().map(LineRange::end_line),
    }
}

fn parse_generation_selector(
    selector: Option<&daemon::GenerationSelector>,
) -> Result<Option<GenerationId>, PublicError> {
    match selector.and_then(|selector| selector.selector.as_ref()) {
        Some(daemon::generation_selector::Selector::Active(true)) => Ok(None),
        Some(daemon::generation_selector::Selector::Generation(generation)) => {
            parse_generation(Some(generation)).map(Some)
        }
        _ => Err(invalid_argument()),
    }
}

fn parse_repository(value: Option<&common::RepositoryId>) -> Result<RepositoryId, PublicError> {
    Ok(RepositoryId::from_bytes(parse_array(
        value.map(|value| value.value.as_slice()),
    )?))
}

fn parse_generation(value: Option<&common::GenerationId>) -> Result<GenerationId, PublicError> {
    Ok(GenerationId::from_bytes(parse_array(
        value.map(|value| value.value.as_slice()),
    )?))
}

fn parse_symbol(value: Option<&common::SymbolId>) -> Result<SymbolId, PublicError> {
    Ok(SymbolId::from_bytes(parse_array(
        value.map(|value| value.value.as_slice()),
    )?))
}

fn parse_file(value: Option<&common::FileId>) -> Result<FileId, PublicError> {
    Ok(FileId::from_bytes(parse_array(
        value.map(|value| value.value.as_slice()),
    )?))
}

fn parse_content_hash(value: Option<&common::ContentHash>) -> Result<ContentHash, PublicError> {
    Ok(ContentHash::from_bytes(parse_array(
        value.map(|value| value.value.as_slice()),
    )?))
}

fn parse_operation(value: Option<&common::OperationId>) -> Result<OperationId, PublicError> {
    Ok(OperationId::from_bytes(parse_array(
        value.map(|value| value.value.as_slice()),
    )?))
}

fn parse_array<const N: usize>(value: Option<&[u8]>) -> Result<[u8; N], PublicError> {
    value
        .and_then(|value| value.try_into().ok())
        .ok_or_else(invalid_argument)
}

fn repository_to_wire(value: RepositoryId) -> common::RepositoryId {
    common::RepositoryId {
        value: value.as_bytes().to_vec(),
    }
}

fn generation_to_wire(value: GenerationId) -> common::GenerationId {
    common::GenerationId {
        value: value.as_bytes().to_vec(),
    }
}

fn symbol_to_wire(value: SymbolId) -> common::SymbolId {
    common::SymbolId {
        value: value.as_bytes().to_vec(),
    }
}

fn file_to_wire(value: FileId) -> common::FileId {
    common::FileId {
        value: value.as_bytes().to_vec(),
    }
}

fn content_hash_to_wire(value: ContentHash) -> common::ContentHash {
    common::ContentHash {
        value: value.as_bytes().to_vec(),
    }
}

fn operation_to_wire(value: OperationId) -> common::OperationId {
    common::OperationId {
        value: value.as_bytes().to_vec(),
    }
}

const fn schema_version() -> common::ContractVersion {
    common::ContractVersion {
        major: FIRST_SLICE_SCHEMA_MAJOR,
        minor: FIRST_SLICE_SCHEMA_MINOR,
    }
}

fn analysis_tier_to_wire(tier: AnalysisTier) -> daemon::FirstSliceAnalysisTier {
    match tier {
        AnalysisTier::TierA => daemon::FirstSliceAnalysisTier::FirstSliceTierA,
        AnalysisTier::TierB => daemon::FirstSliceAnalysisTier::FirstSliceTierB,
        AnalysisTier::TierC => daemon::FirstSliceAnalysisTier::FirstSliceTierC,
        AnalysisTier::TierD => daemon::FirstSliceAnalysisTier::FirstSliceTierD,
        _ => daemon::FirstSliceAnalysisTier::Unspecified,
    }
}

fn tier_label_to_wire(tier: &str) -> daemon::FirstSliceAnalysisTier {
    match tier {
        "tier_a" => daemon::FirstSliceAnalysisTier::FirstSliceTierA,
        "tier_b" => daemon::FirstSliceAnalysisTier::FirstSliceTierB,
        "tier_c" => daemon::FirstSliceAnalysisTier::FirstSliceTierC,
        _ => daemon::FirstSliceAnalysisTier::FirstSliceTierD,
    }
}

fn coverage_status_to_wire(status: CoverageStatus) -> daemon::FirstSliceCoverageStatus {
    match status {
        CoverageStatus::Complete => daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete,
        CoverageStatus::Bounded => daemon::FirstSliceCoverageStatus::FirstSliceCoverageBounded,
        CoverageStatus::Sampled => daemon::FirstSliceCoverageStatus::FirstSliceCoverageSampled,
        CoverageStatus::Unknown => daemon::FirstSliceCoverageStatus::FirstSliceCoverageUnknown,
        _ => daemon::FirstSliceCoverageStatus::Unspecified,
    }
}

fn operation_state_to_wire(state: OperationState) -> daemon::OperationState {
    match state {
        OperationState::Queued => daemon::OperationState::Queued,
        OperationState::Running => daemon::OperationState::Running,
        OperationState::Cancelling => daemon::OperationState::Cancelling,
        OperationState::Succeeded => daemon::OperationState::Succeeded,
        OperationState::Failed => daemon::OperationState::Failed,
        OperationState::Interrupted => daemon::OperationState::Interrupted,
        OperationState::Cancelled => daemon::OperationState::Cancelled,
    }
}

// The finite [0, 1] clamp proves the final conversion is non-negative and
// bounded by the wire scale.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn score_to_wire(score: f32) -> u32 {
    if !score.is_finite() || score <= 0.0 {
        0
    } else if score >= 1.0 {
        1_000
    } else {
        (score * 1_000.0).round() as u32
    }
}

fn enum_label(value: impl serde::Serialize) -> Result<String, PublicError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(internal_error)
}

fn checked_add(left: u64, right: u64) -> Result<u64, PublicError> {
    left.checked_add(right).ok_or_else(resource_exhausted)
}

fn journal_call<T>(
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
    request: impl Future<Output = Result<T, ServiceError>>,
) -> Result<T, PublicError> {
    runtime.block_on(async {
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), request)
            .await
            .map_err(|_| cancelled_error())?
            .map_err(service_boundary_error)
    })
}

fn journal_lifecycle_call<T>(
    runtime: &tokio::runtime::Runtime,
    request: impl Future<Output = Result<T, ServiceError>>,
) -> Result<T, PublicError> {
    // Claimed `_until` commands own their absolute timeout. An outer timeout
    // would drop a command after the actor won `Executing`, violating the
    // invariant that an already-claimed durable mutation is awaited to reply.
    runtime.block_on(request).map_err(service_boundary_error)
}

fn lifecycle_deadline(client_deadline: Instant) -> Result<Instant, PublicError> {
    client_deadline
        .checked_add(LIFECYCLE_FINALIZATION_GRACE)
        .ok_or_else(internal_error)
}

fn service_boundary_error(error: ServiceError) -> PublicError {
    match error {
        ServiceError::Operations(error) => operation_error(&error, None),
        ServiceError::Public(error) => *error,
        ServiceError::QueueFull
        | ServiceError::ClientOperationLimit { .. }
        | ServiceError::ClientConnectionLimit { .. } => resource_exhausted(),
        ServiceError::RequestTimedOut => lifecycle_timed_out(),
        _ => internal_error(),
    }
}

fn deadline_unix_ms(deadline: Instant) -> Result<u64, PublicError> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(cancelled_error)?;
    let deadline = SystemTime::now()
        .checked_add(remaining)
        .ok_or_else(invalid_argument)?;
    system_time_ms(deadline)
}

fn unix_time_ms() -> Result<u64, PublicError> {
    system_time_ms(SystemTime::now())
}

fn system_time_ms(time: SystemTime) -> Result<u64, PublicError> {
    let elapsed = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| internal_error())?;
    u64::try_from(elapsed.as_millis()).map_err(|_| internal_error())
}

fn lock_metadata(
    metadata: &Mutex<OperationMetadataSet>,
) -> Result<std::sync::MutexGuard<'_, OperationMetadataSet>, PublicError> {
    metadata.lock().map_err(|_| internal_error())
}

fn map_queue_error(error: TrySendError<WorkerCommand>) -> PublicError {
    match error {
        TrySendError::Full(_) => resource_exhausted(),
        TrySendError::Disconnected(_) => internal_error(),
    }
}

fn service_error(error: FirstSliceError) -> PublicError {
    let (code, message, retryable) = match error {
        FirstSliceError::Cancelled(_) => (ErrorCode::Cancelled, "operation was cancelled", false),
        FirstSliceError::RepositoryNotFound => {
            (ErrorCode::NotFound, "repository was not found", false)
        }
        FirstSliceError::GenerationNotFound => (
            ErrorCode::StaleGeneration,
            "generation is not retained",
            false,
        ),
        FirstSliceError::GenerationMismatch => (
            ErrorCode::Conflict,
            "generation does not belong to repository",
            false,
        ),
        FirstSliceError::FixtureShape => (
            ErrorCode::UnsupportedCapability,
            "repository shape is unsupported",
            false,
        ),
        FirstSliceError::Retention | FirstSliceError::Limits => (
            ErrorCode::ResourceExhausted,
            "first-slice resource limit was reached",
            true,
        ),
        FirstSliceError::Query => (ErrorCode::NotFound, "query target was not found", false),
        FirstSliceError::Adapter => (
            ErrorCode::AdapterFailed,
            "repository analysis failed",
            false,
        ),
        _ => (ErrorCode::Internal, "first-slice operation failed", false),
    };
    let mut builder = PublicError::builder(code, message);
    if retryable {
        builder = builder.retryable().next_action(NextAction::Retry);
    }
    builder
        .build()
        .unwrap_or_else(|_| unreachable!("closed first-slice errors are statically bounded"))
}

fn operation_error(error: &OperationError, operation: Option<OperationId>) -> PublicError {
    let (code, message, retryable) = match error {
        OperationError::NotFound => (ErrorCode::NotFound, "operation was not found", false),
        OperationError::Busy | OperationError::WriterBusy | OperationError::ConcurrentUpdate => {
            (ErrorCode::Busy, "operation state is busy", true)
        }
        OperationError::InvalidSubmission
        | OperationError::InvalidClientInstanceId
        | OperationError::InvalidProgress
        | OperationError::InvalidStage => (
            ErrorCode::InvalidArgument,
            "operation request is invalid",
            false,
        ),
        OperationError::AlreadyExists
        | OperationError::SubmissionConflict
        | OperationError::IllegalTransition { .. }
        | OperationError::InvalidTerminalError
        | OperationError::LeaseOwnerMismatch
        | OperationError::InvalidLease => (
            ErrorCode::Conflict,
            "operation state conflicts with request",
            false,
        ),
        OperationError::CancellationWon => (ErrorCode::Cancelled, "operation was cancelled", false),
        _ => (ErrorCode::Internal, "operation journal failed", false),
    };
    let mut builder = PublicError::builder(code, message);
    if let Some(operation) = operation {
        builder = builder.operation(operation);
    }
    if retryable {
        builder = builder.retryable().next_action(NextAction::Retry);
    }
    builder
        .build()
        .unwrap_or_else(|_| unreachable!("closed operation errors are statically bounded"))
}

fn invalid_argument() -> PublicError {
    PublicError::builder(ErrorCode::InvalidArgument, "first-slice request is invalid")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn not_found() -> PublicError {
    PublicError::builder(ErrorCode::NotFound, "operation was not found")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn stale_generation() -> PublicError {
    PublicError::builder(ErrorCode::StaleGeneration, "source generation is stale")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn incomplete_coverage() -> PublicError {
    PublicError::builder(
        ErrorCode::IncompleteCoverage,
        "symbol definition evidence is unavailable",
    )
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn operation_in_progress() -> PublicError {
    PublicError::builder(ErrorCode::Busy, "repository index is still running")
        .retryable()
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn terminal_operation_error(operation: OperationId, message: &'static str) -> PublicError {
    PublicError::builder(ErrorCode::Cancelled, message)
        .operation(operation)
        .next_action(NextAction::InspectOperation)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn unsupported_capability() -> PublicError {
    PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "first-slice request mode is unsupported",
    )
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn unsupported_restart_state() -> PublicError {
    PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "first-slice state is not available after restart",
    )
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn resource_exhausted() -> PublicError {
    PublicError::builder(
        ErrorCode::ResourceExhausted,
        "first-slice capacity is exhausted",
    )
    .retryable()
    .next_action(NextAction::Retry)
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn cancelled_error() -> PublicError {
    PublicError::builder(ErrorCode::Cancelled, "first-slice request was cancelled")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn lifecycle_timed_out() -> PublicError {
    PublicError::builder(ErrorCode::Busy, "operation lifecycle timed out")
        .retryable()
        .next_action(NextAction::InspectOperation)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn internal_error() -> PublicError {
    PublicError::builder(ErrorCode::Internal, "first-slice operation failed")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_daemon_core::JournalActor;
    use rootlight_operations::{ClientInstanceId, OperationJournal, OperationStage, RecoveryClass};
    use std::{fs, time::Duration};
    use tempfile::TempDir;

    fn repository_submission(operation: OperationId, seed: u8) -> OperationSubmission {
        OperationSubmission::new(
            operation,
            OperationKind::RepositoryIndex,
            PlanHash::from_bytes([seed; 32]),
            ClientInstanceId::new([seed; 16]).expect("client identity is valid"),
            true,
            None,
            None,
        )
        .expect("submission is valid")
    }

    fn index_across_client_disconnect(
        detached: bool,
        protocol_error: bool,
        boundary: PublicationBoundary,
        prove_lane_reusable: bool,
        operation_byte: u8,
    ) -> (
        Result<FirstSliceIpcResponse, PublicError>,
        OperationRecord,
        bool,
    ) {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary,
            armed: AtomicBool::new(true),
            reached: reached_sender,
            release: release_receiver,
        };
        let (daemon, workers) = FirstSliceDaemon::start_with_publication_hook(actor.handle(), hook)
            .expect("host starts");
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([operation_byte; 16]);
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let connection_cancellation = cancellation.clone();
        let admission = rootlight_daemon_core::FirstSliceAdmission::default();
        let connection_admission = admission.clone();
        let index_daemon = daemon.clone();
        let root = fixture.path().to_string_lossy().into_owned();
        let follow_up_root = root.clone();
        let (response_sender, response_receiver) = mpsc::sync_channel(1);
        let index = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let context = FirstSliceIpcContext {
                client_instance_id: ClientInstanceId::from_bytes([7; 16]),
                selected_protocol_minor: 5,
                cancellation,
                deadline,
                index_admission: Some(admission),
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("runtime builds");
            let response = runtime.block_on(index_daemon.dispatch(
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root,
                    operation: Some(operation_to_wire(operation)),
                    detached,
                }),
                context,
            ));
            response_sender
                .send(response)
                .expect("index response is observed");
        });
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches selected lifecycle boundary");
        if !detached || protocol_error {
            connection_admission.cancel_publication();
            assert!(
                connection_cancellation
                    .cancel(rootlight_operations::CancellationReason::ClientRequest)
            );
        }
        if prove_lane_reusable {
            let cancellation = execute(
                &daemon,
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(operation)),
                        action: daemon::RepositoryOperationAction::RepositoryOperationCancel as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                ),
            );
            assert!(matches!(
                cancellation,
                FirstSliceIpcResponse::RepositoryOperationStatus(_)
            ));
        }
        release_sender.send(()).expect("index resumes");
        let response = response_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("cancelled index releases the work lane");
        index.join().expect("index thread joins");
        let status = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryOperationStatus(
                daemon::RepositoryOperationStatusRequest {
                    schema_version: Some(schema_version()),
                    operation: Some(operation_to_wire(operation)),
                    action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                    wait_ms: None,
                    after_revision: None,
                },
            ),
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
            panic!("operation status response expected");
        };
        let published = status.published_generation.is_some();
        let terminal = journal.status(operation).expect("terminal status persists");
        if prove_lane_reusable {
            let follow_up = execute_with_timeout(
                &daemon,
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root: follow_up_root,
                    operation: Some(operation_to_wire(OperationId::from_bytes(
                        [operation_byte.wrapping_add(64); 16],
                    ))),
                    detached: true,
                }),
            );
            let FirstSliceIpcResponse::RepositoryIndex(follow_up) =
                follow_up.expect("fresh index completes on the released work lane")
            else {
                panic!("fresh repository index response expected");
            };
            assert!(
                follow_up.parent_generation.is_none(),
                "cancelled work must not publish a parent generation"
            );
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
        (response, terminal, published)
    }

    #[test]
    fn attached_disconnect_cancels_before_publication() {
        let (response, terminal, published) = index_across_client_disconnect(
            false,
            false,
            PublicationBoundary::BeforeCompletion,
            false,
            61,
        );

        assert_eq!(
            response.expect_err("attached request is cancelled").code(),
            ErrorCode::Cancelled
        );
        assert_eq!(terminal.state, OperationState::Cancelled);
        assert_eq!(terminal.stage, OperationStage::Cleanup);
        assert!(!published);
    }

    #[test]
    fn detached_disconnect_does_not_cancel_publication() {
        let (response, terminal, published) = index_across_client_disconnect(
            true,
            false,
            PublicationBoundary::BeforeCompletion,
            false,
            62,
        );

        assert!(matches!(
            response.expect("detached request completes"),
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
        assert_eq!(terminal.state, OperationState::Succeeded);
        assert!(published);
    }

    #[test]
    fn detached_protocol_error_cancels_before_publication() {
        let (response, terminal, published) = index_across_client_disconnect(
            true,
            true,
            PublicationBoundary::BeforeCompletion,
            false,
            63,
        );

        assert_eq!(
            response
                .expect_err("detached protocol error is cancelled")
                .code(),
            ErrorCode::Cancelled
        );
        assert_eq!(terminal.state, OperationState::Cancelled);
        assert_eq!(terminal.stage, OperationStage::Cleanup);
        assert!(!published);
    }

    #[test]
    fn peer_cancellation_leaves_work_lane_reusable_before_publication() {
        for (boundary, operation_byte) in [
            (PublicationBoundary::AfterAdmission, 64),
            (PublicationBoundary::AfterActivation, 65),
            (PublicationBoundary::BeforeCompletion, 66),
        ] {
            let (response, terminal, published) =
                index_across_client_disconnect(false, false, boundary, true, operation_byte);

            assert_eq!(
                response.expect_err("attached request is cancelled").code(),
                ErrorCode::Cancelled
            );
            assert_eq!(terminal.state, OperationState::Cancelled);
            assert_eq!(
                terminal.stage,
                if boundary == PublicationBoundary::AfterAdmission {
                    OperationStage::Accepted
                } else {
                    OperationStage::Cleanup
                }
            );
            assert!(!published);
        }
    }

    #[test]
    fn daemon_worker_indexes_and_serves_prior_generation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(journal, 16, 16).expect("journal actor starts");
        let (daemon, workers) = FirstSliceDaemon::start(actor.handle()).expect("host starts");
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        let source = fixture.path().join("src/lib.rs");
        fs::write(&source, "pub fn answer() -> u32 { 42 }\n").expect("source writes");
        let first = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(OperationId::from_bytes([1; 16]))),
                detached: true,
            }),
        );
        let FirstSliceIpcResponse::RepositoryIndex(first) = first else {
            panic!("index response expected");
        };
        let retry = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(OperationId::from_bytes([1; 16]))),
                detached: true,
            }),
        );
        let FirstSliceIpcResponse::RepositoryIndex(retry) = retry else {
            panic!("retry index response expected");
        };
        assert_eq!(retry, first);
        let repository = first.repository.clone().expect("repository is returned");
        let generation = first
            .published_generation
            .clone()
            .expect("generation is published");

        fs::write(&source, "pub fn answer() -> u32 { 43 }\n").expect("source updates");
        let second = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(OperationId::from_bytes([2; 16]))),
                detached: true,
            }),
        );
        assert!(matches!(second, FirstSliceIpcResponse::RepositoryIndex(_)));
        let locate = execute(
            &daemon,
            FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest {
                schema_version: Some(schema_version()),
                repository: Some(repository),
                generation: Some(daemon::GenerationSelector {
                    selector: Some(daemon::generation_selector::Selector::Generation(
                        generation,
                    )),
                }),
                query: "answer".to_owned(),
                mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
                maximum_results: 8,
            }),
        );
        let FirstSliceIpcResponse::CodeLocate(locate) = locate else {
            panic!("locate response expected");
        };
        assert_eq!(locate.hits.len(), 1);
        assert!(!locate.context.expect("context exists").active_generation);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn succeeded_status_observes_staged_receipt_at_commit_boundary() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::AfterSuccess,
            armed: AtomicBool::new(true),
            reached: reached_sender,
            release: release_receiver,
        };
        let (daemon, workers) = FirstSliceDaemon::start_with_publication_hook(actor.handle(), hook)
            .expect("host starts");
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([41; 16]);
        let index_daemon = daemon.clone();
        let root = fixture.path().to_string_lossy().into_owned();
        let index = thread::spawn(move || {
            execute(
                &index_daemon,
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root,
                    operation: Some(operation_to_wire(operation)),
                    detached: true,
                }),
            )
        });
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches success/commit boundary");

        let status = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryOperationStatus(
                daemon::RepositoryOperationStatusRequest {
                    schema_version: Some(schema_version()),
                    operation: Some(operation_to_wire(operation)),
                    action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                    wait_ms: None,
                    after_revision: None,
                },
            ),
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
            panic!("operation status response expected");
        };
        assert_eq!(
            status
                .operation
                .as_ref()
                .expect("operation status exists")
                .state,
            daemon::OperationState::Succeeded as i32
        );
        assert!(status.published_generation.is_some());
        assert_eq!(status.files_examined, 1);

        release_sender.send(()).expect("publication resumes");
        let response = index.join().expect("index thread joins");
        assert!(matches!(
            response,
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn shutdown_interrupts_in_flight_index_and_wakes_live_sender_clones() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::BeforeCompletion,
            armed: AtomicBool::new(true),
            reached: reached_sender,
            release: release_receiver,
        };
        let (daemon, workers) = FirstSliceDaemon::start_with_publication_hook(actor.handle(), hook)
            .expect("host starts");
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([43; 16]);
        let index_daemon = daemon.clone();
        let root = fixture.path().to_string_lossy().into_owned();
        let index = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(30);
            let context = FirstSliceIpcContext {
                client_instance_id: rootlight_operations::ClientInstanceId::from_bytes([7; 16]),
                selected_protocol_minor: 5,
                cancellation: rootlight_operations::Cancellation::with_deadline(deadline),
                deadline,
                index_admission: None,
            };
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("runtime builds");
            runtime.block_on(index_daemon.dispatch(
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root,
                    operation: Some(operation_to_wire(operation)),
                    detached: true,
                }),
                context,
            ))
        });
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches pre-completion boundary");
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            release_sender.send(()).expect("index worker resumes");
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let started = Instant::now();
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(2)))
            .expect("workers stop within the global cap");
        assert!(started.elapsed() < Duration::from_secs(2));
        release.join().expect("release thread joins");
        assert!(index.join().expect("index thread joins").is_err());
        let terminal = journal
            .status(operation)
            .expect("operation status persists");
        assert_eq!(terminal.state, OperationState::Interrupted);
        assert_eq!(
            terminal.stage,
            rootlight_operations::OperationStage::Executing
        );
        drop(daemon);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn lifecycle_channel_close_is_fail_closed() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let closed =
            journal_lifecycle_call::<()>(&runtime, async { Err(ServiceError::ChannelClosed) })
                .expect_err("closed actor channel fails");
        assert_eq!(closed.code(), ErrorCode::Internal);

        let mut metadata = OperationMetadataSet::new(1);
        let operation = OperationId::from_bytes([42; 16]);
        metadata.reserve(operation, 1).expect("metadata reserves");
        metadata.fail_closed(operation);
        assert_eq!(
            metadata
                .records
                .get(&operation)
                .expect("metadata remains inspectable")
                .publication,
            PublicationState::FailedClosed
        );
    }

    #[test]
    fn journal_cancellation_deadline_binding_is_single_use_and_cancellation_aware() {
        let deadline = Instant::now() + Duration::from_secs(1);
        let cancellation = Cancellation::new();
        bind_journal_cancellation_deadline(&cancellation, deadline)
            .expect("journal token accepts the IPC deadline");
        assert!(cancellation.has_deadline());
        assert_eq!(
            bind_journal_cancellation_deadline(&cancellation, deadline)
                .expect_err("a pre-bound journal token fails closed")
                .code(),
            ErrorCode::Internal
        );

        let cancelled = Cancellation::new();
        assert!(cancelled.cancel(rootlight_operations::CancellationReason::ClientRequest));
        assert_eq!(
            bind_journal_cancellation_deadline(&cancelled, deadline)
                .expect_err("an existing cancellation reason wins")
                .code(),
            ErrorCode::Cancelled
        );
    }

    #[test]
    fn retry_replays_terminal_outcomes_instead_of_reporting_busy() {
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let mut metadata = OperationMetadataSet::new(8);

        let failed = OperationId::from_bytes([51; 16]);
        journal
            .submit(repository_submission(failed, 51))
            .expect("failure operation submits");
        journal
            .start_execution(failed)
            .expect("failure operation starts");
        journal
            .update_stage(failed, OperationStage::Cleanup)
            .expect("failure operation enters cleanup");
        let stored_error = PublicError::builder(ErrorCode::InvalidArgument, "checked failure")
            .operation(failed)
            .build()
            .expect("error is valid");
        let failed_record = journal
            .transition(failed, OperationState::Failed, Some(&stored_error))
            .expect("failure persists");
        metadata.reserve(failed, 1).expect("metadata reserves");
        assert_eq!(
            retry_index_response(&Mutex::new(metadata), failed_record)
                .expect_err("failed retry replays its error"),
            stored_error
        );

        let cancelled = OperationId::from_bytes([52; 16]);
        journal
            .submit(repository_submission(cancelled, 52))
            .expect("cancelled operation submits");
        let cancelled_record = journal
            .request_cancellation(
                cancelled,
                rootlight_operations::CancellationReason::ClientRequest,
            )
            .expect("cancellation persists")
            .operation;
        let mut cancelled_metadata = OperationMetadataSet::new(1);
        cancelled_metadata
            .reserve(cancelled, 1)
            .expect("metadata reserves");
        let cancelled_error =
            retry_index_response(&Mutex::new(cancelled_metadata), cancelled_record)
                .expect_err("cancelled retry is terminal");
        assert_eq!(cancelled_error.code(), ErrorCode::Cancelled);

        let interrupted = OperationId::from_bytes([53; 16]);
        journal
            .submit(repository_submission(interrupted, 53))
            .expect("interrupted operation submits");
        let interrupted_record = journal
            .interrupt_deadline(interrupted)
            .expect("interruption persists");
        let mut interrupted_metadata = OperationMetadataSet::new(1);
        interrupted_metadata
            .reserve(interrupted, 1)
            .expect("metadata reserves");
        let interrupted_error =
            retry_index_response(&Mutex::new(interrupted_metadata), interrupted_record)
                .expect_err("interrupted retry is terminal");
        assert_eq!(interrupted_error.code(), ErrorCode::Cancelled);
    }

    #[test]
    fn succeeded_status_without_staged_receipt_fails_closed() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([54; 16]);
        journal
            .submit(repository_submission(operation, 54))
            .expect("operation submits");
        journal
            .start_execution(operation)
            .expect("operation starts");
        journal
            .complete_repository_publication(operation)
            .expect("publication completes");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let mut metadata = OperationMetadataSet::new(1);
        metadata.reserve(operation, 1).expect("metadata reserves");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(1);
        let error = repository_operation_status(
            &actor.handle(),
            &Mutex::new(metadata),
            &runtime,
            daemon::RepositoryOperationStatusRequest {
                schema_version: Some(schema_version()),
                operation: Some(operation_to_wire(operation)),
                action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                after_revision: None,
                wait_ms: None,
            },
            &FirstSliceIpcContext {
                client_instance_id: ClientInstanceId::from_bytes([54; 16]),
                selected_protocol_minor: 5,
                cancellation: Cancellation::with_deadline(deadline),
                deadline,
                index_admission: None,
            },
        )
        .expect_err("missing receipt fails closed");
        assert_eq!(error.code(), ErrorCode::Internal);
        actor.join().expect("actor joins");
    }

    #[test]
    fn elapsed_deadline_during_index_failure_persists_interruption() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([55; 16]);
        journal
            .submit(repository_submission(operation, 55))
            .expect("operation submits");
        journal
            .start_execution(operation)
            .expect("operation starts");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let elapsed = Cancellation::with_deadline(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("elapsed deadline derives"),
        );
        let adapter_error =
            PublicError::builder(ErrorCode::AdapterFailed, "repository analysis failed")
                .operation(operation)
                .build()
                .expect("error is valid");

        finish_failed_index(
            &runtime,
            Instant::now() + Duration::from_secs(1),
            &actor.handle(),
            operation,
            &elapsed,
            &adapter_error,
        )
        .expect("deadline finalization persists");
        let terminal = journal.status(operation).expect("terminal state loads");
        assert_eq!(terminal.state, OperationState::Interrupted);
        assert_eq!(terminal.recovery_class, RecoveryClass::DeadlineElapsed);
        actor.join().expect("actor joins");
    }

    fn execute(daemon: &FirstSliceDaemon, request: FirstSliceIpcRequest) -> FirstSliceIpcResponse {
        let deadline = Instant::now() + Duration::from_secs(30);
        let context = FirstSliceIpcContext {
            client_instance_id: rootlight_operations::ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: 5,
            cancellation: rootlight_operations::Cancellation::with_deadline(deadline),
            deadline,
            index_admission: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        runtime
            .block_on(daemon.dispatch(request, context))
            .expect("request succeeds")
    }

    fn execute_with_timeout(
        daemon: &FirstSliceDaemon,
        request: FirstSliceIpcRequest,
    ) -> Result<FirstSliceIpcResponse, PublicError> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: rootlight_operations::ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: 5,
            cancellation: rootlight_operations::Cancellation::with_deadline(deadline),
            deadline,
            index_admission: None,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        runtime
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(5), daemon.dispatch(request, context))
                    .await
            })
            .expect("work-lane request completes within its deadline")
    }
}
