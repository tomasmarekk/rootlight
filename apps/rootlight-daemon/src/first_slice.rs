//! Bounded daemon-owned first-slice service and lifecycle workers.
//!
//! Query and semantic-refinement workers share the immutable generation view
//! under read locks. Publication remains short and serialized, while a separate
//! control worker keeps journal status and cancellation responsive.

// The daemon-core port deliberately owns PublicError by value. Keeping that
// exact boundary throughout this private adapter avoids repeated boxing and
// unboxing across every dispatch branch.
#![allow(clippy::result_large_err)]

use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rootlight_adapter_host::{
    AdapterHostError, PROJECT_ADAPTER_HARD_LIMITS, execute_isolated_project_adapter,
    negotiate_project_adapter_session_with_cancellation,
    project_adapter_identity_with_cancellation,
};
use rootlight_daemon_core::{
    ControlRequest, ControlResponse, DaemonState, FirstSliceEffectiveBudget, FirstSliceIpcContext,
    FirstSliceIpcFuture, FirstSliceIpcHandler, FirstSliceIpcRequest, FirstSliceIpcResponse,
    HealthStatus, IndexSupportInventory, JournalActorHandle, RepositoryIndexProvider,
    ResourcePressure, ServiceError, operation_record_to_wire,
};
use rootlight_error::{DetailKey, ErrorCode, NextAction, PublicError, PublicValue, SafeLabel};
use rootlight_ids::{
    ContentHash, FileId, GenerationId, OperationId, RepositoryId, SymbolId, content_hash,
};
use rootlight_ir::{
    AnalysisTier, CoverageRecord, CoverageStatus, LineRange, NormalizedIrDocument, OccurrenceRole,
    RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
};
use rootlight_observability::{
    SupportAdapterInventory, SupportChecksumStatus, SupportGenerationInventory,
    SupportRepositoryInventory,
};
#[cfg(test)]
use rootlight_operations::RecoveryClass;
use rootlight_operations::{
    Cancellation, CancellationAuthority, CancellationReason, ClientInstanceId,
    InternalCancellationAuthority, OperationError, OperationKind, OperationRecord, OperationStage,
    OperationState, OperationSubmission, PlanHash, Progress, RepositoryOperationContext,
    RepositoryOperationMode, RepositoryOperationSubmission,
};
use rootlight_protocol::{
    MAX_CODE_LOCATE_LANGUAGE_BYTES, MAX_CODE_LOCATE_LANGUAGES,
    adapter_contract::{
        ADAPTER_NONCE_BYTES, MAX_ADAPTER_FRAME_BYTES, NegotiatedSession,
        project_analysis_frame_bytes, project_analysis_input_field_bytes,
        project_analysis_request_payload_bytes,
    },
    generated::{adapter::v1 as adapter, common::v1 as common, daemon::v1 as daemon},
};
use rootlight_query::{
    ArchitectureOverviewView, CodeDeadEntryPointPolicy, ExecutionCompletenessState, LocateMode,
    QueryResource, QueryUsage, RelationDirection, RelationFamily, TestsSelectKind,
};
use rootlight_runtime::{CoordinatedStartupSignal, STARTUP_ACTIVE_GENERATION_RESTORE_TIMEOUT};
use rootlight_service::{
    ADVANCED_DEFAULT_MAX_DEPTH, ADVANCED_DEFAULT_MAX_RESULTS, ADVANCED_MAX_TRAVERSAL,
    AdvancedAstNode, FirstSliceBudget, FirstSliceDeferredRestore, FirstSliceDurableOperation,
    FirstSliceError, FirstSliceGenerationContext, FirstSliceIndexAdmission, FirstSliceIndexMode,
    FirstSliceIndexProgress, FirstSliceIndexProvider, FirstSliceIndexReceipt,
    FirstSliceObservedFreshness, FirstSliceOperationContext, FirstSliceProjectAnalysis,
    FirstSliceProjectAnalysisError, FirstSliceProjectAnalysisProgress,
    FirstSliceProjectAnalysisRequest, FirstSliceProjectAnalyzer, FirstSliceService,
    FirstSliceSupportInventory, HistoryChangeKind, PlanChangeObjective,
    SourceEncoding as ServiceSourceEncoding, SourceReadOptions,
    catalog::{
        CATALOG_SORT_VERSION, CatalogError, CatalogInstant, CatalogListFilter, CatalogPageRequest,
        CatalogPageSize, CatalogRepositoryRecord, CatalogRepositoryState, CatalogSnapshotId,
        CatalogSortKey,
    },
};
use sysinfo::{Pid, ProcessesToUpdate, System};

const FIRST_SLICE_SCHEMA_MAJOR: u32 = 1;
const FIRST_SLICE_SCHEMA_MINOR: u32 = 0;
const DEFAULT_GENERATION_RETENTION: usize = 8;
const DEFAULT_WORK_QUEUE: usize = 16;
const DEFAULT_READ_QUEUE: usize = 32;
const DEFAULT_CONTROL_QUEUE: usize = 32;
const DEFAULT_OPERATION_METADATA: usize = 256;
const RETRY_AFTER_MS: u32 = 100;
const MAX_OPERATION_STATUS_WAIT_MS: u32 = 30_000;
const OPERATION_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const PUBLICATION_LOCK_POLL_INTERVAL: Duration = Duration::from_millis(2);
// The public client timeout is also 30 seconds. Leave enough time after a
// maximum long poll for serialization, IPC scheduling, and client decoding.
const OPERATION_STATUS_RESPONSE_GRACE: Duration = Duration::from_secs(2);
const LIFECYCLE_FINALIZATION_GRACE: Duration = Duration::from_secs(2);
const REFINEMENT_ADMISSION_WAIT: Duration = Duration::from_secs(2);
const DETACHED_INDEX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
const DEFAULT_RELATIONSHIP_RESULTS: u32 = 100;
const DEFAULT_FLOW_DEPTH: u32 = 3;
const DEFAULT_FLOW_PATHS: u32 = 10;
const DEFAULT_CYCLE_MIN_SIZE: u32 = 2;
const DEFAULT_CYCLE_MAX_CYCLES: u32 = 50;
const DEFAULT_CODE_DEAD_MAX_CANDIDATES: u32 = 50;
const DEFAULT_ARCHITECTURE_OVERVIEW_MAX_COMPONENTS: u32 = 50;
const DEFAULT_TESTS_SELECT_MAX_TESTS: u32 = 20;
const DEFAULT_CHANGE_IMPACT_MAX_DEPTH: u32 = 3;
const DEFAULT_CHANGE_IMPACT_MAX_DEPENDENTS: u32 = 100;
const DEFAULT_PLAN_CHANGE_MAX_STEPS: u32 = 6;
const DEFAULT_HISTORY_COMPARE_MAX_RESULTS: u32 = 100;
// Keep native adapter limits below the 30-second request envelope so process
// cleanup, IR validation, resolution, and atomic publication retain headroom.
const PROJECT_ADAPTER_WALL_TIME_MS: u64 = 20_000;
const PROJECT_ADAPTER_CPU_TIME_MS: u64 = 15_000;
const PROJECT_ADAPTER_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
const PROJECT_ADAPTER_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const PROJECT_ADAPTER_OUTPUT_BYTES: u64 = 16 * 1024 * 1024;
// Source expands into normalized facts, so partitions need output and CPU
// headroom even when the encoded request remains below the hard input limit.
const PROJECT_ADAPTER_PARTITION_SOURCE_BYTES: u64 = 1024 * 1024;
const PROJECT_ADAPTER_PARTITION_FILES: usize = 512;
const PROJECT_ADAPTER_HANDLES: u32 = 64;

type Reply = tokio::sync::oneshot::Sender<Result<FirstSliceIpcResponse, PublicError>>;

struct InstalledProjectAnalyzer {
    executable: PathBuf,
    provider_identity: ContentHash,
}

impl InstalledProjectAnalyzer {
    fn discover(
        cancellation: &Cancellation,
    ) -> Result<Option<Arc<dyn FirstSliceProjectAnalyzer>>, FirstSliceError> {
        let mut executable = std::env::current_exe().map_err(|_| FirstSliceError::Adapter)?;
        executable.set_file_name(format!(
            "rootlight-adapter-host{}",
            std::env::consts::EXE_SUFFIX
        ));
        if !executable
            .try_exists()
            .map_err(|_| FirstSliceError::Adapter)?
        {
            return Ok(None);
        }
        if !executable
            .metadata()
            .map_err(|_| FirstSliceError::Adapter)?
            .is_file()
        {
            return Err(FirstSliceError::Adapter);
        }
        let identity = project_adapter_identity_with_cancellation(&executable, cancellation)
            .map_err(|error| match error {
                AdapterHostError::Cancelled(cancelled) => {
                    FirstSliceError::Cancelled(cancelled.reason())
                }
                _ => FirstSliceError::Adapter,
            })?;
        let provider_identity =
            adapter_identity_digest(&identity).map_err(|_| FirstSliceError::Adapter)?;
        Ok(Some(Arc::new(Self {
            executable,
            provider_identity,
        })))
    }
}

impl FirstSliceProjectAnalyzer for InstalledProjectAnalyzer {
    fn provider_identity(&self) -> ContentHash {
        self.provider_identity
    }

    fn analyze(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
        self.analyze_with_progress(request, cancellation, &mut |_| {})
    }

    fn analyze_with_progress(
        &self,
        request: FirstSliceProjectAnalysisRequest<'_>,
        cancellation: &Cancellation,
        observe_progress: &mut dyn FnMut(FirstSliceProjectAnalysisProgress),
    ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
        let mut ordered_inputs: Vec<&rootlight_service::FirstSliceProjectInput<'_>> = Vec::new();
        ordered_inputs
            .try_reserve_exact(request.inputs().len())
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        ordered_inputs.extend(request.inputs().iter());
        ordered_inputs.sort_unstable_by(|left, right| left.path().cmp(right.path()));
        if ordered_inputs.is_empty() {
            return Err(FirstSliceProjectAnalysisError::Analysis);
        }
        let total_files = u64::try_from(ordered_inputs.len())
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let total_bytes = ordered_inputs.iter().try_fold(0_u64, |total, input| {
            total
                .checked_add(
                    u64::try_from(input.source().len())
                        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
                )
                .ok_or(FirstSliceProjectAnalysisError::Analysis)
        })?;
        let files = u32::try_from(ordered_inputs.len())
            .unwrap_or(u32::MAX)
            .min(PROJECT_ADAPTER_HARD_LIMITS.files);
        let limits = adapter::ResourceLimits {
            wall_time_ms: PROJECT_ADAPTER_WALL_TIME_MS,
            cpu_time_ms: PROJECT_ADAPTER_CPU_TIME_MS,
            memory_bytes: PROJECT_ADAPTER_MEMORY_BYTES,
            input_bytes: PROJECT_ADAPTER_INPUT_BYTES,
            output_bytes: PROJECT_ADAPTER_OUTPUT_BYTES,
            files,
            processes: 1,
            handles: PROJECT_ADAPTER_HANDLES,
            retries: 0,
        };
        let mut session_id = [0_u8; ADAPTER_NONCE_BYTES];
        getrandom::fill(&mut session_id).map_err(|_| FirstSliceProjectAnalysisError::Identity)?;
        if session_id.iter().all(|byte| *byte == 0) {
            return Err(FirstSliceProjectAnalysisError::Identity);
        }
        let (observed_identity, session) = negotiate_project_adapter_session_with_cancellation(
            &self.executable,
            session_id,
            limits,
            cancellation,
        )
        .map_err(map_project_adapter_error)?;
        if adapter_identity_digest(&observed_identity)? != self.provider_identity {
            return Err(FirstSliceProjectAnalysisError::Identity);
        }

        let sizing_request = build_project_analysis_request(
            &request,
            session.session_id(),
            &[1; ADAPTER_NONCE_BYTES],
            request.build_context(),
            Vec::new(),
        );
        let base_request_payload_bytes = project_analysis_request_payload_bytes(&sizing_request);
        let mut partition = ProjectPartitionBuffer::new(
            base_request_payload_bytes,
            request.context_manifest().len(),
            usize::try_from(files).map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
        )?;
        let mut documents = Vec::new();
        let maximum_partitions = ordered_inputs.len();
        documents
            .try_reserve(maximum_partitions.min(16))
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let mut isolation_permits_deep_adapter = true;
        let mut completed_files = 0_u64;
        let mut completed_bytes = 0_u64;
        for input in ordered_inputs {
            let wire_input = project_input_to_wire(&request, input)?;
            if let Some(rejected) = partition.try_push(wire_input)? {
                let batch = partition.take();
                if batch.is_empty() {
                    return Err(FirstSliceProjectAnalysisError::Analysis);
                }
                let (partition_files, partition_bytes) = project_partition_progress(&batch)?;
                let (document, isolated) =
                    self.execute_partition(&request, &session, batch, cancellation)?;
                documents.push(document);
                isolation_permits_deep_adapter &= isolated;
                completed_files = completed_files
                    .checked_add(partition_files)
                    .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
                completed_bytes = completed_bytes
                    .checked_add(partition_bytes)
                    .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
                observe_progress(FirstSliceProjectAnalysisProgress {
                    completed_files,
                    total_files,
                    completed_bytes,
                    total_bytes,
                });
                if partition.try_push(rejected)?.is_some() {
                    return Err(FirstSliceProjectAnalysisError::Analysis);
                }
            }
        }
        let batch = partition.take();
        if batch.is_empty() {
            return Err(FirstSliceProjectAnalysisError::Analysis);
        }
        let (partition_files, partition_bytes) = project_partition_progress(&batch)?;
        let (document, isolated) =
            self.execute_partition(&request, &session, batch, cancellation)?;
        documents.push(document);
        isolation_permits_deep_adapter &= isolated;
        completed_files = completed_files
            .checked_add(partition_files)
            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
        completed_bytes = completed_bytes
            .checked_add(partition_bytes)
            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
        observe_progress(FirstSliceProjectAnalysisProgress {
            completed_files,
            total_files,
            completed_bytes,
            total_bytes,
        });
        Ok(FirstSliceProjectAnalysis::new_partitioned(
            documents,
            isolation_permits_deep_adapter,
        ))
    }
}

fn project_partition_progress(
    inputs: &[adapter::ProjectInput],
) -> Result<(u64, u64), FirstSliceProjectAnalysisError> {
    let files =
        u64::try_from(inputs.len()).map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
    let bytes = inputs.iter().try_fold(0_u64, |total, input| {
        total
            .checked_add(
                u64::try_from(input.source.len())
                    .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?,
            )
            .ok_or(FirstSliceProjectAnalysisError::Analysis)
    })?;
    Ok((files, bytes))
}

impl InstalledProjectAnalyzer {
    fn execute_partition(
        &self,
        request: &FirstSliceProjectAnalysisRequest<'_>,
        session: &NegotiatedSession,
        inputs: Vec<adapter::ProjectInput>,
        cancellation: &Cancellation,
    ) -> Result<(NormalizedIrDocument, bool), FirstSliceProjectAnalysisError> {
        let mut request_id = [0_u8; ADAPTER_NONCE_BYTES];
        getrandom::fill(&mut request_id).map_err(|_| FirstSliceProjectAnalysisError::Identity)?;
        if request_id.iter().all(|byte| *byte == 0) {
            return Err(FirstSliceProjectAnalysisError::Identity);
        }
        let project_request = build_project_analysis_request(
            request,
            session.session_id(),
            &request_id,
            request.build_context(),
            inputs,
        );
        let payload_bytes = project_analysis_request_payload_bytes(&project_request);
        if project_analysis_frame_bytes(payload_bytes)
            .is_none_or(|bytes| bytes > MAX_ADAPTER_FRAME_BYTES)
        {
            return Err(FirstSliceProjectAnalysisError::Analysis);
        }
        let output = execute_isolated_project_adapter(
            &self.executable,
            session,
            &project_request,
            &rootlight_ir::ExtensionSupport::default(),
            cancellation,
        )
        .map_err(map_project_adapter_error)?;
        Ok((
            output.document().clone(),
            output.isolation().permits_deep_adapter(),
        ))
    }
}

struct ProjectPartitionBuffer {
    inputs: Vec<adapter::ProjectInput>,
    base_request_payload_bytes: usize,
    request_payload_bytes: usize,
    source_bytes: usize,
    context_bytes: usize,
    max_files: usize,
}

impl ProjectPartitionBuffer {
    fn new(
        base_request_payload_bytes: usize,
        context_bytes: usize,
        max_files: usize,
    ) -> Result<Self, FirstSliceProjectAnalysisError> {
        if context_bytes
            > usize::try_from(PROJECT_ADAPTER_INPUT_BYTES)
                .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?
            || max_files == 0
        {
            return Err(FirstSliceProjectAnalysisError::Analysis);
        }
        Ok(Self {
            inputs: Vec::new(),
            base_request_payload_bytes,
            request_payload_bytes: base_request_payload_bytes,
            source_bytes: context_bytes,
            context_bytes,
            max_files: max_files.min(PROJECT_ADAPTER_PARTITION_FILES),
        })
    }

    fn try_push(
        &mut self,
        input: adapter::ProjectInput,
    ) -> Result<Option<adapter::ProjectInput>, FirstSliceProjectAnalysisError> {
        let input_field_bytes = project_analysis_input_field_bytes(&input)
            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
        let request_payload_bytes = self
            .request_payload_bytes
            .checked_add(input_field_bytes)
            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
        let source_bytes = self
            .source_bytes
            .checked_add(input.source.len())
            .ok_or(FirstSliceProjectAnalysisError::Analysis)?;
        let input_limit = usize::try_from(PROJECT_ADAPTER_INPUT_BYTES)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let partition_source_limit = usize::try_from(PROJECT_ADAPTER_PARTITION_SOURCE_BYTES)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        let fits = self.inputs.len() < self.max_files
            && source_bytes <= input_limit
            && (self.inputs.is_empty() || source_bytes <= partition_source_limit)
            && project_analysis_frame_bytes(request_payload_bytes)
                .is_some_and(|bytes| bytes <= MAX_ADAPTER_FRAME_BYTES);
        if !fits {
            return Ok(Some(input));
        }
        self.inputs
            .try_reserve(1)
            .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
        self.inputs.push(input);
        self.request_payload_bytes = request_payload_bytes;
        self.source_bytes = source_bytes;
        Ok(None)
    }

    fn take(&mut self) -> Vec<adapter::ProjectInput> {
        self.request_payload_bytes = self.base_request_payload_bytes;
        self.source_bytes = self.context_bytes;
        std::mem::take(&mut self.inputs)
    }
}

fn project_input_to_wire(
    request: &FirstSliceProjectAnalysisRequest<'_>,
    input: &rootlight_service::FirstSliceProjectInput<'_>,
) -> Result<adapter::ProjectInput, FirstSliceProjectAnalysisError> {
    let mut source = Vec::new();
    source
        .try_reserve_exact(input.source().len())
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
    source.extend_from_slice(input.source());
    let mut origins = Vec::new();
    origins
        .try_reserve_exact(input.origins().len())
        .map_err(|_| FirstSliceProjectAnalysisError::Analysis)?;
    for mapping in input.origins() {
        origins.push(adapter::GeneratedOrigin {
            generated_start_byte: mapping.generated().start_byte(),
            generated_end_byte: mapping.generated().end_byte(),
            origin_path: mapping.origin_path().as_str().to_owned(),
            origin_start_byte: mapping.origin().start_byte(),
            origin_end_byte: mapping.origin().end_byte(),
            transformation: mapping.transformation().as_str().to_owned(),
            generator_digest: mapping
                .generator_digest()
                .map(|digest| common::ContentHash {
                    value: digest.as_bytes().to_vec(),
                }),
        });
    }
    Ok(adapter::ProjectInput {
        file: Some(common::FileId {
            value: input.file().as_bytes().to_vec(),
        }),
        path: input.path().to_owned(),
        language: request.language().to_owned(),
        source_digest: Some(common::ContentHash {
            value: input.content_hash().as_bytes().to_vec(),
        }),
        source,
        generated: input.generated(),
        origins,
    })
}

fn build_project_analysis_request(
    request: &FirstSliceProjectAnalysisRequest<'_>,
    session_id: &[u8],
    request_id: &[u8],
    build_context: ContentHash,
    inputs: Vec<adapter::ProjectInput>,
) -> adapter::ProjectAnalysisRequest {
    let context_manifest = request.context_manifest().to_vec();
    adapter::ProjectAnalysisRequest {
        session_id: session_id.to_vec(),
        request_id: request_id.to_vec(),
        repository: Some(common::RepositoryId {
            value: request.repository().as_bytes().to_vec(),
        }),
        generation: Some(common::GenerationId {
            value: request.generation().as_bytes().to_vec(),
        }),
        analysis_unit: format!("first-slice.{}.partition", request.language()),
        target: format!("//rootlight:{}/partition", request.language()),
        build_context: Some(common::ContentHash {
            value: build_context.as_bytes().to_vec(),
        }),
        config_digest: Some(common::ContentHash {
            value: content_hash(&context_manifest).as_bytes().to_vec(),
        }),
        inputs,
        context_manifest,
        requested_tier: adapter::RequestedAnalysisTier::TierB as i32,
    }
}

fn adapter_identity_digest(
    identity: &adapter::AdapterIdentity,
) -> Result<ContentHash, FirstSliceProjectAnalysisError> {
    let digest = identity
        .source_digest
        .as_slice()
        .try_into()
        .map_err(|_| FirstSliceProjectAnalysisError::Identity)?;
    Ok(ContentHash::from_bytes(digest))
}

fn map_project_adapter_error(error: AdapterHostError) -> FirstSliceProjectAnalysisError {
    match error {
        AdapterHostError::Cancelled(cancelled) => {
            FirstSliceProjectAnalysisError::Cancelled(cancelled.reason())
        }
        AdapterHostError::BinaryIdentity
        | AdapterHostError::DigestMismatch
        | AdapterHostError::ProvenanceMismatch => FirstSliceProjectAnalysisError::Identity,
        AdapterHostError::Process
        | AdapterHostError::ProcessIo
        | AdapterHostError::IsolationEvidence => FirstSliceProjectAnalysisError::Isolation,
        AdapterHostError::ProcessTimeout => FirstSliceProjectAnalysisError::WallTimeLimit,
        AdapterHostError::ProcessFailed => FirstSliceProjectAnalysisError::ProcessFailure,
        AdapterHostError::ProjectAnalysis => FirstSliceProjectAnalysisError::Analysis,
        _ => FirstSliceProjectAnalysisError::Protocol,
    }
}

enum WorkerCommand {
    Execute {
        request: FirstSliceIpcRequest,
        context: FirstSliceIpcContext,
        reply: Reply,
    },
}

struct SemanticRefinementCommand {
    request: daemon::RepositoryIndexRequest,
    context: FirstSliceIpcContext,
    operation: OperationId,
    repository: RepositoryId,
    structural_generation: GenerationId,
    admitted: SyncSender<Result<(), PublicError>>,
}

struct PendingSemanticRefinement {
    repository: RepositoryId,
    cancellation: Cancellation,
}

type SharedFirstSliceService = Arc<RwLock<FirstSliceService>>;
type IndexSerialization = Arc<Mutex<()>>;
type SemanticRefinements = Arc<Mutex<BTreeMap<OperationId, PendingSemanticRefinement>>>;

#[derive(Clone)]
struct FirstSliceServiceLanes {
    service: SharedFirstSliceService,
    index_serialization: IndexSerialization,
    semantic_refinements: SemanticRefinements,
    refinement: SyncSender<SemanticRefinementCommand>,
    recovery_ready: Arc<AtomicBool>,
    support_state: Option<Arc<DaemonState>>,
}

struct DeferredRecoveryWork {
    restore: FirstSliceDeferredRestore,
    installed_generations: BTreeSet<GenerationId>,
    restore_active: bool,
}

struct PublicationBoundaryHook {
    boundary: PublicationBoundary,
    fail_commit: AtomicBool,
    armed: AtomicBool,
    reached: SyncSender<()>,
    release: Receiver<()>,
}

#[derive(Clone, Copy)]
struct ServiceRequestResources<'a> {
    journal: &'a JournalActorHandle,
    metadata: &'a Mutex<OperationMetadataSet>,
    runtime: &'a tokio::runtime::Runtime,
    catalog_epoch: Instant,
    publication_hook: Option<&'a PublicationBoundaryHook>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PublicationBoundary {
    AfterAdmission,
    AfterActivation,
    BeforeCompletion,
    AfterSuccess,
    AfterCommit,
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

    fn fail_commit(&self) -> bool {
        self.fail_commit.swap(false, Ordering::AcqRel)
    }
}

/// Cloneable bounded first-slice port used by accepted IPC connections.
#[derive(Clone)]
pub(crate) struct FirstSliceDaemon {
    work: SyncSender<WorkerCommand>,
    read: SyncSender<WorkerCommand>,
    control: SyncSender<WorkerCommand>,
}

impl FirstSliceDaemon {
    /// Starts the service, semantic-refinement, and lifecycle worker lanes.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceHostError`] when the first-slice service cannot
    /// initialize or a bounded worker thread cannot start.
    #[cfg(test)]
    pub(crate) fn start(
        journal: JournalActorHandle,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        Self::start_inner(journal, None)
    }

    /// Starts worker lanes backed by the account-private durable state root.
    ///
    /// # Errors
    ///
    /// Returns [`FirstSliceHostError`] when durable recovery, service
    /// initialization, or bounded worker startup fails.
    pub(crate) async fn start_durable(
        journal: JournalActorHandle,
        state_root: &Path,
        support_state: Arc<DaemonState>,
        startup_signal: Option<fn(CoordinatedStartupSignal) -> io::Result<()>>,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(STARTUP_ACTIVE_GENERATION_RESTORE_TIMEOUT)
                .ok_or(FirstSliceHostError::Service(FirstSliceError::Limits))?,
        );
        let project_analyzer = InstalledProjectAnalyzer::discover(&cancellation)
            .map_err(FirstSliceHostError::Service)?;
        let (service, deferred_restore) = match project_analyzer {
            Some(project_analyzer) => {
                FirstSliceService::open_durable_deferred_with_project_analyzer(
                    DEFAULT_GENERATION_RETENTION,
                    state_root,
                    project_analyzer,
                )
            }
            None => {
                FirstSliceService::open_durable_deferred(DEFAULT_GENERATION_RETENTION, state_root)
            }
        }
        .map_err(FirstSliceHostError::Service)?;
        let restore_active = deferred_restore
            .has_active_restore_work()
            .map_err(FirstSliceHostError::Service)?;
        if let Some(publish) = startup_signal {
            let signal = if restore_active {
                CoordinatedStartupSignal::ActiveGenerationRestore
            } else {
                CoordinatedStartupSignal::NoRecovery
            };
            publish(signal).map_err(FirstSliceHostError::StartupSignal)?;
        }
        let durable_contexts = Self::load_startup_repository_contexts(&journal).await?;
        let inventory = index_support_inventory(&service).map_err(FirstSliceHostError::Service)?;
        support_state
            .replace_index_support_inventory(inventory)
            .map_err(FirstSliceHostError::Journal)?;
        if restore_active {
            support_state.set_generation_status(HealthStatus::Unavailable);
        }
        Self::start_workers(
            journal,
            service,
            None,
            Vec::new(),
            durable_contexts,
            Some(support_state),
            restore_active.then_some(DeferredRecoveryWork {
                restore: deferred_restore,
                installed_generations: BTreeSet::new(),
                restore_active: true,
            }),
        )
    }

    #[cfg(test)]
    fn start_with_publication_hook(
        journal: JournalActorHandle,
        hook: PublicationBoundaryHook,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        Self::start_inner(journal, Some(hook))
    }

    #[cfg(test)]
    fn start_durable_with_publication_hook(
        journal: JournalActorHandle,
        state_root: &Path,
        hook: PublicationBoundaryHook,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(STARTUP_ACTIVE_GENERATION_RESTORE_TIMEOUT)
                .ok_or(FirstSliceHostError::Service(FirstSliceError::Limits))?,
        );
        let service =
            FirstSliceService::new_durable(DEFAULT_GENERATION_RETENTION, state_root, &cancellation)
                .map_err(FirstSliceHostError::Service)?;
        Self::start_workers(
            journal,
            service,
            Some(hook),
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
    }

    #[cfg(test)]
    fn start_inner(
        journal: JournalActorHandle,
        publication_hook: Option<PublicationBoundaryHook>,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        let service = FirstSliceService::new(DEFAULT_GENERATION_RETENTION)
            .map_err(FirstSliceHostError::Service)?;
        Self::start_workers(
            journal,
            service,
            publication_hook,
            Vec::new(),
            Vec::new(),
            None,
            None,
        )
    }

    async fn load_startup_repository_contexts(
        journal: &JournalActorHandle,
    ) -> Result<Vec<(RepositoryOperationContext, OperationRecord)>, FirstSliceHostError> {
        let contexts = journal
            .repository_operation_contexts()
            .await
            .map_err(FirstSliceHostError::Journal)?;
        let mut restored = Vec::new();
        restored
            .try_reserve_exact(contexts.len())
            .map_err(|_| FirstSliceHostError::Service(FirstSliceError::Retention))?;
        for context in contexts {
            let response = journal
                .control(ControlRequest::OperationStatus(context.operation))
                .await
                .map_err(FirstSliceHostError::Journal)?;
            let ControlResponse::OperationStatus(record) = response else {
                return Err(FirstSliceHostError::Service(FirstSliceError::Retention));
            };
            restored.push((context, record));
        }
        Ok(restored)
    }

    fn start_workers(
        journal: JournalActorHandle,
        service: FirstSliceService,
        publication_hook: Option<PublicationBoundaryHook>,
        durable_publications: Vec<(FirstSliceDurableOperation, OperationRecord)>,
        durable_contexts: Vec<(RepositoryOperationContext, OperationRecord)>,
        support_state: Option<Arc<DaemonState>>,
        deferred_restore: Option<DeferredRecoveryWork>,
    ) -> Result<(Self, FirstSliceWorkers), FirstSliceHostError> {
        for (context, _) in &durable_contexts {
            if let Some(root_identity) = context.root_identity {
                service
                    .restore_repository_registration(
                        ContentHash::from_bytes(root_identity),
                        context.repository,
                    )
                    .map_err(FirstSliceHostError::Service)?;
            }
        }
        let mut operation_metadata = OperationMetadataSet::new(DEFAULT_OPERATION_METADATA);
        for (context, record) in durable_contexts {
            if let Some(state) = support_state.as_deref() {
                state.record_repository_index_context(
                    context.operation,
                    context.repository,
                    repository_operation_support_provider(context.mode),
                );
            }
            operation_metadata
                .restore_context(context, &record)
                .map_err(FirstSliceHostError::Service)?;
        }
        for (publication, record) in durable_publications {
            if let Some(state) = support_state.as_deref() {
                state.record_repository_index_context(
                    publication.operation,
                    publication.receipt.repository,
                    repository_index_support_provider(publication.provider),
                );
            }
            operation_metadata
                .restore_committed(publication, &record)
                .map_err(FirstSliceHostError::Service)?;
        }
        // Complete fallible durable restoration before constructing owned
        // runtimes. Async daemon bootstrap cannot drop a Tokio runtime when a
        // later restoration error unwinds this function.
        let work_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(FirstSliceHostError::AsyncRuntime)?;
        let read_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(FirstSliceHostError::AsyncRuntime)?;
        let control_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(FirstSliceHostError::AsyncRuntime)?;
        let refinement_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(FirstSliceHostError::AsyncRuntime)?;
        let recovery_runtime = deferred_restore
            .as_ref()
            .map(|_| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .map_err(FirstSliceHostError::AsyncRuntime)
            })
            .transpose()?;
        let metadata = Arc::new(Mutex::new(operation_metadata));
        let stopping = Arc::new(AtomicBool::new(false));
        let service = Arc::new(RwLock::new(service));
        let index_serialization = Arc::new(Mutex::new(()));
        let semantic_refinements = Arc::new(Mutex::new(BTreeMap::new()));
        let recovery_ready = Arc::new(AtomicBool::new(
            deferred_restore
                .as_ref()
                .is_none_or(|recovery| !recovery.restore_active),
        ));
        let (work, work_receiver) = mpsc::sync_channel(DEFAULT_WORK_QUEUE);
        let (read, read_receiver) = mpsc::sync_channel(DEFAULT_READ_QUEUE);
        let (control, control_receiver) = mpsc::sync_channel(DEFAULT_CONTROL_QUEUE);
        let (refinement, refinement_receiver) = mpsc::sync_channel(DEFAULT_OPERATION_METADATA);
        let lanes = FirstSliceServiceLanes {
            service,
            index_serialization,
            semantic_refinements: Arc::clone(&semantic_refinements),
            refinement: refinement.clone(),
            recovery_ready: Arc::clone(&recovery_ready),
            support_state,
        };
        let work_journal = journal.clone();
        let work_metadata = Arc::clone(&metadata);
        let work_stopping = Arc::clone(&stopping);
        let work_lanes = lanes.clone();
        let work_thread = thread::Builder::new()
            .name("rootlight-first-slice".to_owned())
            .spawn(move || {
                service_worker(
                    work_lanes,
                    work_journal,
                    work_metadata,
                    work_stopping,
                    work_runtime,
                    work_receiver,
                    publication_hook,
                );
            })
            .map_err(|error| {
                stopping.store(true, Ordering::Release);
                FirstSliceHostError::Thread(error)
            })?;
        let read_journal = journal.clone();
        let read_metadata = Arc::clone(&metadata);
        let read_stopping = Arc::clone(&stopping);
        let read_lanes = lanes.clone();
        let read_thread = thread::Builder::new()
            .name("rootlight-first-slice-read".to_owned())
            .spawn(move || {
                service_worker(
                    read_lanes,
                    read_journal,
                    read_metadata,
                    read_stopping,
                    read_runtime,
                    read_receiver,
                    None,
                );
            })
            .map_err(|error| {
                stopping.store(true, Ordering::Release);
                FirstSliceHostError::Thread(error)
            })?;
        let refinement_journal = journal.clone();
        let refinement_metadata = Arc::clone(&metadata);
        let refinement_stopping = Arc::clone(&stopping);
        let refinement_lanes = lanes.clone();
        let refinement_thread = thread::Builder::new()
            .name("rootlight-semantic-refinement".to_owned())
            .spawn(move || {
                semantic_refinement_worker(
                    refinement_lanes,
                    refinement_journal,
                    refinement_metadata,
                    refinement_stopping,
                    refinement_runtime,
                    refinement_receiver,
                );
            })
            .map_err(|error| {
                stopping.store(true, Ordering::Release);
                FirstSliceHostError::Thread(error)
            })?;
        let control_journal = journal.clone();
        let control_stopping = Arc::clone(&stopping);
        let control_metadata = Arc::clone(&metadata);
        let control_thread = thread::Builder::new()
            .name("rootlight-first-slice-control".to_owned())
            .spawn(move || {
                lifecycle_worker(
                    control_journal,
                    control_metadata,
                    control_stopping,
                    control_runtime,
                    control_receiver,
                );
            })
            .map_err(|error| {
                stopping.store(true, Ordering::Release);
                FirstSliceHostError::Thread(error)
            })?;
        let (recovery_cancellation, recovery_thread) = match (deferred_restore, recovery_runtime) {
            (Some(deferred_restore), Some(recovery_runtime)) => {
                let deadline = Instant::now()
                    .checked_add(STARTUP_ACTIVE_GENERATION_RESTORE_TIMEOUT)
                    .ok_or(FirstSliceHostError::Service(FirstSliceError::Limits))?;
                let cancellation = Cancellation::with_deadline(deadline);
                let worker_cancellation = cancellation.clone();
                let recovery_lanes = lanes.clone();
                let recovery_state = recovery_lanes.support_state.clone();
                let recovery_journal = journal.clone();
                let recovery_metadata = Arc::clone(&metadata);
                let recovery_stopping = Arc::clone(&stopping);
                let thread = thread::Builder::new()
                    .name("rootlight-durable-recovery".to_owned())
                    .spawn(move || {
                        let result = durable_recovery_worker(
                            deferred_restore,
                            recovery_lanes,
                            recovery_journal,
                            recovery_metadata,
                            recovery_stopping,
                            recovery_runtime,
                            worker_cancellation,
                        );
                        if result.is_err()
                            && let Some(state) = recovery_state.as_deref()
                        {
                            state.set_generation_status(HealthStatus::Failed);
                        }
                        result
                    })
                    .map_err(|error| {
                        stopping.store(true, Ordering::Release);
                        FirstSliceHostError::Thread(error)
                    })?;
                (Some(cancellation), Some(thread))
            }
            (None, None) => (None, None),
            _ => return Err(FirstSliceHostError::Service(FirstSliceError::Retention)),
        };
        let daemon = Self {
            work: work.clone(),
            read: read.clone(),
            control: control.clone(),
        };
        Ok((
            daemon,
            FirstSliceWorkers {
                work: Some(work),
                read: Some(read),
                control: Some(control),
                refinement: Some(refinement),
                semantic_refinements,
                stopping,
                journal,
                recovery_cancellation,
                work_thread: Some(work_thread),
                read_thread: Some(read_thread),
                control_thread: Some(control_thread),
                refinement_thread: Some(refinement_thread),
                recovery_thread,
            },
        ))
    }

    fn sender(&self, request: &FirstSliceIpcRequest) -> &SyncSender<WorkerCommand> {
        match request {
            FirstSliceIpcRequest::RepositoryIndex(_) => &self.work,
            FirstSliceIpcRequest::RepositoryOperationStatus(_) => &self.control,
            _ => &self.read,
        }
    }

    fn dispatch_once(
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

    async fn dispatch_operation_status(
        &self,
        mut request: daemon::RepositoryOperationStatusRequest,
        context: FirstSliceIpcContext,
    ) -> Result<FirstSliceIpcResponse, PublicError> {
        let wait_ms = request.wait_ms.unwrap_or(0);
        if wait_ms > MAX_OPERATION_STATUS_WAIT_MS {
            return Err(invalid_argument());
        }
        let action = daemon::RepositoryOperationAction::try_from(request.action)
            .map_err(|_| invalid_argument())?;
        let after_revision = request.after_revision;
        // Waiting is owned by the asynchronous connection task. Each journal
        // observation remains short, so one long poll cannot monopolize the
        // control worker needed by cancellation and health traffic.
        request.wait_ms = None;
        request.after_revision = None;
        let response_deadline = context
            .deadline
            .checked_sub(OPERATION_STATUS_RESPONSE_GRACE)
            .unwrap_or(context.deadline);
        let wait_deadline = Instant::now()
            .checked_add(Duration::from_millis(u64::from(wait_ms)))
            .ok_or_else(internal_error)?
            .min(response_deadline);
        let mut observed_revision = None;
        loop {
            context
                .cancellation
                .check()
                .map_err(|cancelled| cancellation_error(cancelled.reason()))?;
            let response = self
                .dispatch_once(
                    FirstSliceIpcRequest::RepositoryOperationStatus(request.clone()),
                    context.clone(),
                )
                .await?;
            let (revision, terminal) = operation_status_observation(&response)?;
            let revision_gate = after_revision.or(observed_revision);
            if action == daemon::RepositoryOperationAction::RepositoryOperationCancel
                || wait_ms == 0
                || terminal
                || revision_gate.is_some_and(|gate| revision > gate)
            {
                return Ok(response);
            }
            observed_revision = Some(revision);
            let now = Instant::now();
            if now >= wait_deadline {
                return Ok(response);
            }
            let wake = now
                .checked_add(OPERATION_STATUS_POLL_INTERVAL)
                .ok_or_else(internal_error)?
                .min(wait_deadline);
            tokio::time::sleep_until(tokio::time::Instant::from_std(wake)).await;
        }
    }
}

impl FirstSliceIpcHandler for FirstSliceDaemon {
    fn dispatch(
        &self,
        request: FirstSliceIpcRequest,
        context: FirstSliceIpcContext,
    ) -> FirstSliceIpcFuture {
        if let FirstSliceIpcRequest::RepositoryOperationStatus(request) = request {
            let daemon = self.clone();
            return Box::pin(
                async move { daemon.dispatch_operation_status(request, context).await },
            );
        }
        self.dispatch_once(request, context)
    }
}

fn operation_status_observation(
    response: &FirstSliceIpcResponse,
) -> Result<(u64, bool), PublicError> {
    let FirstSliceIpcResponse::RepositoryOperationStatus(response) = response else {
        return Err(internal_error());
    };
    let operation = response.operation.as_ref().ok_or_else(internal_error)?;
    let state = daemon::OperationState::try_from(operation.state).map_err(|_| internal_error())?;
    Ok((
        operation.revision,
        matches!(
            state,
            daemon::OperationState::Succeeded
                | daemon::OperationState::Failed
                | daemon::OperationState::Interrupted
                | daemon::OperationState::Cancelled
        ),
    ))
}

/// Join owner for the process-lifetime first-slice workers.
pub(crate) struct FirstSliceWorkers {
    work: Option<SyncSender<WorkerCommand>>,
    read: Option<SyncSender<WorkerCommand>>,
    control: Option<SyncSender<WorkerCommand>>,
    refinement: Option<SyncSender<SemanticRefinementCommand>>,
    semantic_refinements: SemanticRefinements,
    stopping: Arc<AtomicBool>,
    journal: JournalActorHandle,
    recovery_cancellation: Option<Cancellation>,
    work_thread: Option<JoinHandle<()>>,
    read_thread: Option<JoinHandle<()>>,
    control_thread: Option<JoinHandle<()>>,
    refinement_thread: Option<JoinHandle<()>>,
    recovery_thread: Option<JoinHandle<Result<(), FirstSliceHostError>>>,
}

impl FirstSliceWorkers {
    /// Stops all lanes while accepted connection handlers drain concurrently.
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
        if let Some(cancellation) = self.recovery_cancellation.take() {
            let _ = cancellation.cancel(CancellationReason::Shutdown);
        }
        // Cancel the local registry before scanning the journal. A refinement
        // may have left the queue but not activated its journal record yet.
        cancel_all_semantic_refinements(&self.semantic_refinements, CancellationReason::Shutdown)?;
        // This runs only during global daemon shutdown. Interrupting the full
        // bounded journal batch is intentional: no operation kind may outlive
        // the process-wide worker drain.
        tokio::time::timeout_at(deadline, self.journal.interrupt(DEFAULT_OPERATION_METADATA))
            .await
            .map_err(|_| FirstSliceHostError::ShutdownTimedOut)?
            .map_err(FirstSliceHostError::Journal)?;
        self.work.take();
        self.read.take();
        self.control.take();
        self.refinement.take();
        let work = self.work_thread.take();
        let read = self.read_thread.take();
        let control = self.control_thread.take();
        let refinement = self.refinement_thread.take();
        let recovery = self.recovery_thread.take();
        let (joined, completion) = tokio::sync::oneshot::channel();
        thread::Builder::new()
            .name("rootlight-first-slice-join".to_owned())
            .spawn(move || {
                let mut result = [control, read, work, refinement]
                    .into_iter()
                    .flatten()
                    .try_for_each(|thread| {
                        thread
                            .join()
                            .map_err(|_| FirstSliceHostError::ThreadPanicked)
                    });
                if result.is_ok()
                    && let Some(recovery) = recovery
                {
                    result = recovery
                        .join()
                        .map_err(|_| FirstSliceHostError::ThreadPanicked)
                        .and_then(std::convert::identity);
                }
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
        if let Some(cancellation) = self.recovery_cancellation.take() {
            let _ = cancellation.cancel(CancellationReason::Shutdown);
        }
        if let Ok(refinements) = self.semantic_refinements.try_lock() {
            for pending in refinements.values() {
                let _ = pending.cancellation.cancel(CancellationReason::Shutdown);
            }
        }
        self.work.take();
        self.read.take();
        self.control.take();
        self.refinement.take();
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
    /// The exact-child startup signal could not reach its coordinator.
    #[error("first-slice startup signal failed")]
    StartupSignal(#[source] std::io::Error),
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

#[derive(Debug, Clone)]
struct OperationMetadata {
    started_unix_ms: u64,
    repository: Option<RepositoryId>,
    parent_generation: Option<GenerationId>,
    estimated_disk_bytes: u64,
    receipt: Option<FirstSliceIndexReceipt>,
    published_generation: Option<GenerationId>,
    peak_rss_bytes: Arc<AtomicU64>,
    written_bytes: u64,
    files_examined: u64,
    bytes_examined: u64,
    publication: PublicationState,
    terminal: bool,
    terminal_snapshot: Option<OperationStatusSnapshot>,
}

impl OperationMetadata {
    fn from_durable_context(context: RepositoryOperationContext, record: &OperationRecord) -> Self {
        Self {
            started_unix_ms: context.started_unix_ms,
            repository: Some(context.repository),
            parent_generation: context.parent_generation,
            estimated_disk_bytes: context.estimated_disk_bytes,
            receipt: None,
            published_generation: context.published_generation,
            peak_rss_bytes: Arc::new(AtomicU64::new(record.peak_rss_bytes)),
            written_bytes: record.written_bytes,
            files_examined: context.files_examined,
            bytes_examined: context.bytes_examined,
            publication: if context.published_generation.is_some()
                && record.state == OperationState::Succeeded
            {
                PublicationState::Committed
            } else {
                PublicationState::None
            },
            terminal: record.state.is_terminal(),
            terminal_snapshot: record
                .state
                .is_terminal()
                .then(|| OperationStatusSnapshot::from_record(record)),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OperationStatusSnapshot {
    kind: OperationKind,
    state: OperationState,
    completed_units: u32,
    total_units: u32,
    owner: ClientInstanceId,
}

impl OperationStatusSnapshot {
    fn from_record(record: &OperationRecord) -> Self {
        Self {
            kind: record.kind,
            state: record.state,
            completed_units: record.progress.completed,
            total_units: record.progress.total,
            owner: record.owner,
        }
    }
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

    fn restore_committed(
        &mut self,
        publication: rootlight_service::FirstSliceDurableOperation,
        record: &OperationRecord,
    ) -> Result<(), FirstSliceError> {
        if record.operation != publication.operation || record.state != OperationState::Succeeded {
            return Err(FirstSliceError::Retention);
        }
        if let Some(metadata) = self.records.get_mut(&publication.operation) {
            // A no-change publication is admitted against the current active
            // generation, while its reused receipt keeps that generation's
            // original lineage parent. The durable projection binds the
            // operation to the exact reused generation in that case.
            let reused_generation_matches = metadata.parent_generation
                == Some(publication.receipt.generation)
                && metadata.published_generation == Some(publication.receipt.generation);
            if metadata.repository != Some(publication.receipt.repository)
                || metadata.started_unix_ms != publication.started_unix_ms
                || (metadata.parent_generation != publication.receipt.parent
                    && !reused_generation_matches)
            {
                return Err(FirstSliceError::Retention);
            }
            metadata.estimated_disk_bytes = publication.receipt.estimated_disk_bytes;
            metadata.published_generation = Some(publication.receipt.generation);
            metadata.receipt = Some(publication.receipt);
            metadata.peak_rss_bytes = Arc::new(AtomicU64::new(record.peak_rss_bytes));
            metadata.written_bytes = record.written_bytes;
            metadata.publication = PublicationState::Committed;
            metadata.terminal = true;
            metadata.terminal_snapshot = Some(OperationStatusSnapshot::from_record(record));
            return Ok(());
        }
        if self.records.len() >= self.maximum {
            return Ok(());
        }
        self.records.insert(
            publication.operation,
            OperationMetadata {
                started_unix_ms: publication.started_unix_ms,
                repository: Some(publication.receipt.repository),
                parent_generation: publication.receipt.parent,
                estimated_disk_bytes: publication.receipt.estimated_disk_bytes,
                files_examined: publication.receipt.discovered_inputs,
                bytes_examined: 0,
                published_generation: Some(publication.receipt.generation),
                receipt: Some(publication.receipt),
                peak_rss_bytes: Arc::new(AtomicU64::new(record.peak_rss_bytes)),
                written_bytes: record.written_bytes,
                publication: PublicationState::Committed,
                terminal: true,
                terminal_snapshot: Some(OperationStatusSnapshot::from_record(record)),
            },
        );
        Ok(())
    }

    fn restore_context(
        &mut self,
        context: RepositoryOperationContext,
        record: &OperationRecord,
    ) -> Result<(), FirstSliceError> {
        if self.records.contains_key(&context.operation)
            || record.operation != context.operation
            || record.kind != OperationKind::RepositoryIndex
        {
            return Err(FirstSliceError::Retention);
        }
        if self.records.len() >= self.maximum {
            if record.state.is_terminal() {
                return Ok(());
            }
            let oldest_terminal = self
                .records
                .iter()
                .filter(|(_, metadata)| metadata.terminal)
                .min_by_key(|(operation, metadata)| (metadata.started_unix_ms, **operation))
                .map(|(operation, _)| *operation)
                .ok_or(FirstSliceError::Retention)?;
            self.records.remove(&oldest_terminal);
        }
        self.records.insert(
            context.operation,
            OperationMetadata::from_durable_context(context, record),
        );
        Ok(())
    }

    fn reserve(
        &mut self,
        operation: OperationId,
        started_unix_ms: u64,
        repository: Option<RepositoryId>,
    ) -> Result<(), PublicError> {
        if self.records.contains_key(&operation) {
            return Ok(());
        }
        if self.records.len() >= self.maximum {
            let oldest_terminal = self
                .records
                .iter()
                .filter(|(_, metadata)| metadata.terminal)
                .min_by(|(left_id, left), (right_id, right)| {
                    left.started_unix_ms
                        .cmp(&right.started_unix_ms)
                        .then_with(|| left_id.cmp(right_id))
                })
                .map(|(operation, _)| *operation);
            let Some(oldest_terminal) = oldest_terminal else {
                return Err(resource_exhausted());
            };
            self.records.remove(&oldest_terminal);
        }
        self.records.insert(
            operation,
            OperationMetadata {
                started_unix_ms,
                repository,
                parent_generation: None,
                estimated_disk_bytes: 0,
                published_generation: None,
                receipt: None,
                peak_rss_bytes: Arc::new(AtomicU64::new(0)),
                written_bytes: 0,
                files_examined: 0,
                bytes_examined: 0,
                publication: PublicationState::None,
                terminal: false,
                terminal_snapshot: None,
            },
        );
        Ok(())
    }

    fn stage(&mut self, operation: OperationId, receipt: FirstSliceIndexReceipt) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.repository = Some(receipt.repository);
            metadata.parent_generation = receipt.parent;
            metadata.published_generation = Some(receipt.generation);
            metadata.receipt = Some(receipt);
            metadata.publication = PublicationState::Staged;
        }
    }

    fn resource_meter(&self, operation: OperationId) -> Result<Arc<AtomicU64>, PublicError> {
        self.records
            .get(&operation)
            .map(|metadata| Arc::clone(&metadata.peak_rss_bytes))
            .ok_or_else(internal_error)
    }

    fn observe_written_bytes(&mut self, operation: OperationId, written_bytes: u64) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.written_bytes = metadata.written_bytes.max(written_bytes);
        }
    }

    fn observe_inputs(&mut self, operation: OperationId, files_examined: u64, bytes_examined: u64) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.files_examined = metadata.files_examined.max(files_examined);
            metadata.bytes_examined = metadata.bytes_examined.max(bytes_examined);
        }
    }

    fn resources(&self, operation: OperationId) -> Result<(u64, u64), PublicError> {
        let metadata = self.records.get(&operation).ok_or_else(internal_error)?;
        Ok((
            metadata.peak_rss_bytes.load(Ordering::Relaxed),
            metadata.written_bytes,
        ))
    }

    fn admit(&mut self, operation: OperationId, admission: FirstSliceIndexAdmission) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.repository = Some(admission.repository);
            metadata.parent_generation = admission.parent;
            metadata.estimated_disk_bytes = admission.estimated_disk_bytes;
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
        metadata.terminal = true;
        Ok(())
    }

    fn observe_terminal(&mut self, record: &OperationRecord) {
        if !record.state.is_terminal() {
            return;
        }
        if let Some(metadata) = self.records.get_mut(&record.operation) {
            metadata.terminal = true;
            metadata.terminal_snapshot = Some(OperationStatusSnapshot::from_record(record));
        }
    }

    fn cache_snapshot(&mut self, operation: OperationId, snapshot: OperationStatusSnapshot) {
        // Only terminal journal state is immutable. Caching a running snapshot
        // makes repository status remain "indexing" after later publication.
        if snapshot.state.is_terminal()
            && let Some(metadata) = self.records.get_mut(&operation)
        {
            metadata.terminal_snapshot = Some(snapshot);
        }
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
        metadata.published_generation = None;
        metadata.publication = PublicationState::None;
        Ok(())
    }

    #[cfg(test)]
    fn fail_closed(&mut self, operation: OperationId) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.publication = PublicationState::FailedClosed;
            metadata.terminal = true;
        }
    }

    fn mark_terminal(&mut self, operation: OperationId) {
        if let Some(metadata) = self.records.get_mut(&operation) {
            metadata.terminal = true;
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

    fn repository_operations(
        &self,
        repository: RepositoryId,
    ) -> Vec<(OperationId, OperationMetadata)> {
        let mut operations: Vec<_> = self
            .records
            .iter()
            .filter_map(|(operation, metadata)| {
                (metadata.repository == Some(repository)).then_some((*operation, metadata.clone()))
            })
            .collect();
        operations.sort_by(|(left_id, left), (right_id, right)| {
            right
                .started_unix_ms
                .cmp(&left.started_unix_ms)
                .then_with(|| right_id.cmp(left_id))
        });
        operations.truncate(100);
        operations
    }
}

struct ProcessRssSampler {
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessRssSampler {
    fn start(peak_rss_bytes: Arc<AtomicU64>, state: Option<Arc<DaemonState>>) -> Self {
        sample_current_process_rss(&peak_rss_bytes, state.as_deref());
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = thread::Builder::new()
            .name("rootlight-rss-sampler".to_owned())
            .spawn(move || {
                let pid = Pid::from_u32(std::process::id());
                let mut system = System::new();
                while !worker_stopping.load(Ordering::Acquire) {
                    refresh_process_rss(&mut system, pid, &peak_rss_bytes, state.as_deref());
                    thread::park_timeout(Duration::from_millis(25));
                }
                refresh_process_rss(&mut system, pid, &peak_rss_bytes, state.as_deref());
            })
            .ok();
        Self { stopping, worker }
    }
}

impl Drop for ProcessRssSampler {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

fn sample_current_process_rss(peak_rss_bytes: &AtomicU64, state: Option<&DaemonState>) {
    let mut system = System::new();
    refresh_process_rss(
        &mut system,
        Pid::from_u32(std::process::id()),
        peak_rss_bytes,
        state,
    );
}

fn refresh_process_rss(
    system: &mut System,
    pid: Pid,
    peak_rss_bytes: &AtomicU64,
    state: Option<&DaemonState>,
) {
    system.refresh_memory();
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    if let Some(process) = system.process(pid) {
        let rss_bytes = process.memory();
        peak_rss_bytes.fetch_max(rss_bytes, Ordering::Relaxed);
        if let Some(state) = state {
            state.set_resource_pressure(classify_resource_pressure(
                system.total_memory(),
                system.available_memory(),
                rss_bytes,
            ));
        }
    }
}

fn classify_resource_pressure(
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    process_rss_bytes: u64,
) -> ResourcePressure {
    if total_memory_bytes == 0 {
        return ResourcePressure::Unknown;
    }
    let percent = |value: u64| {
        u64::try_from(
            u128::from(value)
                .saturating_mul(100)
                .checked_div(u128::from(total_memory_bytes))
                .unwrap_or_default(),
        )
        .unwrap_or(u64::MAX)
    };
    let available_percent = percent(available_memory_bytes);
    let process_percent = percent(process_rss_bytes);
    if available_percent <= 5 || process_percent >= 80 {
        ResourcePressure::Critical
    } else if available_percent <= 10 || process_percent >= 65 {
        ResourcePressure::High
    } else if available_percent <= 20 || process_percent >= 50 {
        ResourcePressure::Elevated
    } else {
        ResourcePressure::Normal
    }
}

fn durable_recovery_worker(
    mut deferred: DeferredRecoveryWork,
    lanes: FirstSliceServiceLanes,
    journal: JournalActorHandle,
    metadata: Arc<Mutex<OperationMetadataSet>>,
    stopping: Arc<AtomicBool>,
    runtime: tokio::runtime::Runtime,
    cancellation: Cancellation,
) -> Result<(), FirstSliceHostError> {
    if deferred.restore_active {
        let active = match deferred.restore.restore_active(&cancellation) {
            Ok(restored) => restored,
            Err(FirstSliceError::Cancelled(CancellationReason::Shutdown))
                if stopping.load(Ordering::Acquire) =>
            {
                return Ok(());
            }
            Err(error) => return Err(FirstSliceHostError::Service(error)),
        };
        if stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        deferred.installed_generations = active.generation_ids();
        lanes
            .service
            .write()
            .map_err(|_| FirstSliceHostError::ThreadPanicked)?
            .install_deferred_restore(active, &cancellation)
            .map_err(FirstSliceHostError::Service)?;
        reconcile_restored_publications(
            &lanes,
            &journal,
            metadata.as_ref(),
            &stopping,
            &runtime,
            &deferred.installed_generations,
        )?;
        refresh_recovery_support_inventory(&lanes)?;
        lanes.recovery_ready.store(true, Ordering::Release);
    }
    let remaining = match deferred
        .restore
        .restore_excluding(&deferred.installed_generations, &cancellation)
    {
        Ok(restored) => restored,
        Err(FirstSliceError::Cancelled(CancellationReason::Shutdown))
            if stopping.load(Ordering::Acquire) =>
        {
            return Ok(());
        }
        Err(error) => return Err(FirstSliceHostError::Service(error)),
    };
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let remaining_generations = remaining.generation_ids();
    if remaining_generations.is_empty() {
        return Ok(());
    }
    lanes
        .service
        .write()
        .map_err(|_| FirstSliceHostError::ThreadPanicked)?
        .install_additional_deferred_restore(remaining, &cancellation)
        .map_err(FirstSliceHostError::Service)?;
    reconcile_restored_publications(
        &lanes,
        &journal,
        metadata.as_ref(),
        &stopping,
        &runtime,
        &remaining_generations,
    )?;
    refresh_recovery_support_inventory(&lanes)
}

fn reconcile_restored_publications(
    lanes: &FirstSliceServiceLanes,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    stopping: &AtomicBool,
    runtime: &tokio::runtime::Runtime,
    generations: &BTreeSet<GenerationId>,
) -> Result<(), FirstSliceHostError> {
    let publications = lanes
        .service
        .read()
        .map_err(|_| FirstSliceHostError::ThreadPanicked)?
        .durable_operation_publications()
        .filter(|publication| generations.contains(&publication.receipt.generation))
        .collect::<Vec<_>>();
    for publication in publications {
        if stopping.load(Ordering::Acquire) {
            return Ok(());
        }
        match runtime.block_on(journal.reconcile_committed_publication(publication.operation)) {
            Ok(record) => {
                let deadline = Instant::now()
                    .checked_add(LIFECYCLE_FINALIZATION_GRACE)
                    .ok_or(FirstSliceHostError::Service(FirstSliceError::Limits))?;
                match runtime.block_on(journal.record_repository_publication_until(
                    publication.operation,
                    publication.receipt.generation,
                    deadline,
                )) {
                    Ok(_) | Err(ServiceError::Operations(OperationError::NotFound)) => {}
                    Err(error) => return Err(FirstSliceHostError::Journal(error)),
                }
                metadata
                    .lock()
                    .map_err(|_| FirstSliceHostError::ThreadPanicked)?
                    .restore_committed(publication, &record)
                    .map_err(FirstSliceHostError::Service)?;
            }
            // Durable generation history may legitimately outlive the bounded
            // operation journal, so no status is reconstructed without its
            // original journal identity.
            Err(ServiceError::Operations(OperationError::NotFound)) => {}
            Err(error) => return Err(FirstSliceHostError::Journal(error)),
        }
    }
    Ok(())
}

fn refresh_recovery_support_inventory(
    lanes: &FirstSliceServiceLanes,
) -> Result<(), FirstSliceHostError> {
    if let Some(state) = lanes.support_state.as_deref() {
        let inventory = lanes
            .service
            .read()
            .map_err(|_| FirstSliceHostError::ThreadPanicked)
            .and_then(|service| {
                index_support_inventory(&service).map_err(FirstSliceHostError::Service)
            })?;
        state
            .replace_index_support_inventory(inventory)
            .map_err(FirstSliceHostError::Journal)?;
    }
    Ok(())
}

fn service_worker(
    lanes: FirstSliceServiceLanes,
    journal: JournalActorHandle,
    metadata: Arc<Mutex<OperationMetadataSet>>,
    stopping: Arc<AtomicBool>,
    runtime: tokio::runtime::Runtime,
    commands: Receiver<WorkerCommand>,
    publication_hook: Option<PublicationBoundaryHook>,
) {
    // Snapshot expiry is process-relative so wall-clock corrections cannot
    // invalidate live cursor sessions or make the service observe time regress.
    let catalog_epoch = Instant::now();
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
                let mut reply = Some(reply);
                let resources = ServiceRequestResources {
                    journal: &journal,
                    metadata: metadata.as_ref(),
                    runtime: &runtime,
                    catalog_epoch,
                    publication_hook: publication_hook.as_ref(),
                };
                let result =
                    execute_service_request(&lanes, resources, request, context, &mut reply);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
            }
        }
    }
}

fn semantic_refinement_worker(
    lanes: FirstSliceServiceLanes,
    journal: JournalActorHandle,
    metadata: Arc<Mutex<OperationMetadataSet>>,
    stopping: Arc<AtomicBool>,
    runtime: tokio::runtime::Runtime,
    commands: Receiver<SemanticRefinementCommand>,
) {
    let catalog_epoch = Instant::now();
    loop {
        if stopping.load(Ordering::Acquire) {
            return;
        }
        let command = match commands.recv_timeout(Duration::from_millis(10)) {
            Ok(command) => command,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let cancelled = command.context.cancellation.check().is_err();
        let superseded = read_service(&lanes.service).map_or(true, |service| {
            service.active_generation_for(command.repository) != Some(command.structural_generation)
        });
        if cancelled || superseded {
            if remove_semantic_refinement(&lanes.semantic_refinements, command.operation).is_err() {
                return;
            }
            continue;
        }
        let resources = ServiceRequestResources {
            journal: &journal,
            metadata: metadata.as_ref(),
            runtime: &runtime,
            catalog_epoch,
            publication_hook: None,
        };
        let mut reply = None;
        let result = repository_index_with_intent(
            &lanes,
            resources,
            command.request,
            &command.context,
            &mut reply,
            RepositoryIndexIntent::SemanticRefinement {
                structural_generation: command.structural_generation,
            },
            Some(&command.admitted),
        );
        if let Err(error) = result {
            let _ = command.admitted.try_send(Err(error));
        }
        if remove_semantic_refinement(&lanes.semantic_refinements, command.operation).is_err() {
            return;
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
    lanes: &FirstSliceServiceLanes,
    resources: ServiceRequestResources<'_>,
    request: FirstSliceIpcRequest,
    context: FirstSliceIpcContext,
    reply: &mut Option<Reply>,
) -> Result<FirstSliceIpcResponse, PublicError> {
    context
        .cancellation
        .check()
        .map_err(|cancelled| cancellation_error(cancelled.reason()))?;
    if !lanes.recovery_ready.load(Ordering::Acquire)
        && !matches!(
            &request,
            FirstSliceIpcRequest::RepositoryList(_)
                | FirstSliceIpcRequest::RepositoryCatalogPage(_)
        )
    {
        return Err(recovery_in_progress());
    }
    let request = match request {
        FirstSliceIpcRequest::RepositoryIndex(request) => {
            return repository_index(lanes, resources, request, &context, reply)
                .map(FirstSliceIpcResponse::RepositoryIndex);
        }
        request => request,
    };
    let service = read_service(&lanes.service)?;
    match request {
        FirstSliceIpcRequest::RepositoryIndex(_) => Err(internal_error()),
        FirstSliceIpcRequest::CodeLocate(request) => {
            code_locate(&service, request, &context).map(FirstSliceIpcResponse::CodeLocate)
        }
        FirstSliceIpcRequest::SymbolExplain(request) => {
            symbol_explain(&service, request, &context).map(FirstSliceIpcResponse::SymbolExplain)
        }
        FirstSliceIpcRequest::SourceRead(request) => {
            source_read(&service, request, &context).map(FirstSliceIpcResponse::SourceRead)
        }
        FirstSliceIpcRequest::RepositoryList(request) => {
            repository_list(&service, request).map(FirstSliceIpcResponse::RepositoryList)
        }
        FirstSliceIpcRequest::RepositoryCatalogPage(request) => {
            repository_catalog_page(&service, request, resources.catalog_epoch)
                .map(FirstSliceIpcResponse::RepositoryCatalogPage)
        }
        FirstSliceIpcRequest::RepositoryStatus(request) => repository_status(
            &service,
            resources.journal,
            resources.metadata,
            resources.runtime,
            request,
            &context,
        )
        .map(FirstSliceIpcResponse::RepositoryStatus),
        FirstSliceIpcRequest::SymbolRelationships(request) => {
            symbol_relationships(&service, request, &context)
                .map(FirstSliceIpcResponse::SymbolRelationships)
        }
        FirstSliceIpcRequest::FlowTrace(request) => {
            flow_trace(&service, request, &context).map(FirstSliceIpcResponse::FlowTrace)
        }
        FirstSliceIpcRequest::ArchitectureCycles(request) => {
            architecture_cycles(&service, request, &context)
                .map(FirstSliceIpcResponse::ArchitectureCycles)
        }
        FirstSliceIpcRequest::CodeDead(request) => {
            code_dead(&service, request, &context).map(FirstSliceIpcResponse::CodeDead)
        }
        FirstSliceIpcRequest::ArchitectureOverview(request) => {
            architecture_overview(&service, request, &context)
                .map(FirstSliceIpcResponse::ArchitectureOverview)
        }
        FirstSliceIpcRequest::TestsSelect(request) => {
            tests_select(&service, request, &context).map(FirstSliceIpcResponse::TestsSelect)
        }
        FirstSliceIpcRequest::ChangeImpact(request) => {
            change_impact(&service, request, &context).map(FirstSliceIpcResponse::ChangeImpact)
        }
        FirstSliceIpcRequest::PlanChange(request) => {
            plan_change(&service, request, &context).map(FirstSliceIpcResponse::PlanChange)
        }
        FirstSliceIpcRequest::HistoryCompare(request) => {
            history_compare(&service, request, &context).map(FirstSliceIpcResponse::HistoryCompare)
        }
        FirstSliceIpcRequest::QueryAdvanced(request) => {
            advanced_query(&service, request, &context).map(FirstSliceIpcResponse::QueryAdvanced)
        }
        FirstSliceIpcRequest::RepositoryOperationStatus(_) => Err(internal_error()),
    }
}

fn read_service(
    service: &RwLock<FirstSliceService>,
) -> Result<RwLockReadGuard<'_, FirstSliceService>, PublicError> {
    service.read().map_err(|_| internal_error())
}

fn write_service(
    service: &RwLock<FirstSliceService>,
) -> Result<RwLockWriteGuard<'_, FirstSliceService>, PublicError> {
    service.write().map_err(|_| internal_error())
}

fn lock_publication_until<'a>(
    serialization: &'a Mutex<()>,
    service: &'a RwLock<FirstSliceService>,
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<(MutexGuard<'a, ()>, RwLockWriteGuard<'a, FirstSliceService>), PublicError> {
    let serialization = loop {
        cancellation
            .check()
            .map_err(|cancelled| cancellation_error(cancelled.reason()))?;
        match serialization.try_lock() {
            Ok(guard) => break guard,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::park_timeout(PUBLICATION_LOCK_POLL_INTERVAL);
            }
            Err(TryLockError::WouldBlock) => return Err(operation_in_progress()),
            Err(TryLockError::Poisoned(_)) => return Err(internal_error()),
        }
    };
    let service = loop {
        cancellation
            .check()
            .map_err(|cancelled| cancellation_error(cancelled.reason()))?;
        match service.try_write() {
            Ok(guard) => break guard,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                // Preparation readers never acquire the publication mutex until
                // after releasing their read guard, so bounded polling here
                // preserves the prepared candidate without creating a lock cycle.
                thread::park_timeout(PUBLICATION_LOCK_POLL_INTERVAL);
            }
            Err(TryLockError::WouldBlock) => return Err(operation_in_progress()),
            Err(TryLockError::Poisoned(_)) => return Err(internal_error()),
        }
    };
    Ok((serialization, service))
}

fn retry_after() -> Duration {
    Duration::from_millis(u64::from(RETRY_AFTER_MS))
}

fn operation_in_progress() -> PublicError {
    PublicError::builder(ErrorCode::Busy, "repository index is still running")
        .retry_after(retry_after())
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn index_admission_in_progress(operation: OperationId) -> PublicError {
    PublicError::builder(ErrorCode::Busy, "repository index is still running")
        .retry_after(retry_after())
        .operation(operation)
        .next_action(NextAction::InspectOperation)
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn recovery_in_progress() -> PublicError {
    PublicError::builder(
        ErrorCode::Busy,
        "durable repository recovery is still running",
    )
    .retry_after(retry_after())
    .next_action(NextAction::Retry)
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn publication_error(operation: OperationId, error: PublicError) -> PublicError {
    if error.code() == ErrorCode::Busy {
        index_admission_in_progress(operation)
    } else {
        error
    }
}

#[derive(Clone, Copy)]
struct ServiceBudgetReduction {
    rows: u64,
    edges: u64,
    results: u64,
    source_bytes: u64,
    json_bytes: u64,
    estimated_tokens: u64,
    memory_bytes: u64,
    duration: Duration,
}

impl From<FirstSliceEffectiveBudget> for ServiceBudgetReduction {
    fn from(budget: FirstSliceEffectiveBudget) -> Self {
        Self {
            rows: budget.rows(),
            edges: budget.edges(),
            results: budget.results(),
            source_bytes: budget.source_bytes(),
            json_bytes: budget.json_bytes(),
            estimated_tokens: budget.estimated_tokens(),
            memory_bytes: budget.memory_bytes(),
            duration: budget.duration(),
        }
    }
}

fn service_budget(context: &FirstSliceIpcContext) -> FirstSliceBudget {
    reduced_service_budget(context.effective_budget.map(ServiceBudgetReduction::from))
}

fn reduced_service_budget(reduction: Option<ServiceBudgetReduction>) -> FirstSliceBudget {
    reduction.map_or_else(FirstSliceBudget::new, |reduction| {
        FirstSliceBudget::new()
            .reduce_max_rows(reduction.rows)
            .reduce_max_edges(reduction.edges)
            .reduce_max_results(reduction.results)
            .reduce_max_source_bytes(reduction.source_bytes)
            .reduce_max_json_bytes(reduction.json_bytes)
            .reduce_max_tokens(reduction.estimated_tokens)
            .reduce_max_memory_bytes(reduction.memory_bytes)
            .reduce_max_duration(reduction.duration)
    })
}

enum BudgetExhaustion {
    Resource(QueryResource),
    Duration,
}

fn remaining_service_budget(
    context: &FirstSliceIpcContext,
    usage: &UsageAccumulator,
) -> Result<FirstSliceBudget, BudgetExhaustion> {
    let Some(budget) = context.effective_budget else {
        return Ok(FirstSliceBudget::new());
    };
    let remaining = |maximum: u64, used: u64, resource| {
        maximum
            .checked_sub(used)
            .filter(|remaining| *remaining > 0)
            .ok_or(resource)
    };
    let rows = match remaining(budget.rows(), usage.rows, QueryResource::Rows) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let edges = match remaining(budget.edges(), usage.edges, QueryResource::Edges) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let results = match remaining(budget.results(), usage.results, QueryResource::Results) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let source_bytes = match remaining(
        budget.source_bytes(),
        usage.source_bytes,
        QueryResource::SourceBytes,
    ) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let json_bytes = match remaining(
        budget.json_bytes(),
        usage.json_bytes,
        QueryResource::JsonBytes,
    ) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let estimated_tokens = match remaining(
        budget.estimated_tokens(),
        usage.estimated_tokens,
        QueryResource::Tokens,
    ) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let memory_bytes = match remaining(
        budget.memory_bytes(),
        usage.memory_bytes,
        QueryResource::MemoryBytes,
    ) {
        Ok(remaining) => remaining,
        Err(resource) => return Err(BudgetExhaustion::Resource(resource)),
    };
    let Some(duration) = budget
        .duration()
        .checked_sub(Duration::from_micros(usage.elapsed_micros))
        .filter(|duration| !duration.is_zero())
    else {
        return Err(BudgetExhaustion::Duration);
    };
    Ok(reduced_service_budget(Some(ServiceBudgetReduction {
        rows,
        edges,
        results,
        source_bytes,
        json_bytes,
        estimated_tokens,
        memory_bytes,
        duration,
    })))
}

fn reduce_optional_u8(current: u8, maximum: Option<u64>) -> Result<u8, PublicError> {
    maximum.map_or(Ok(current), |maximum| {
        u8::try_from(maximum)
            .map(|maximum| current.min(maximum))
            .map_err(|_| internal_error())
    })
}

fn reduce_optional_usize(current: usize, maximum: Option<u64>) -> Result<usize, PublicError> {
    maximum.map_or(Ok(current), |maximum| {
        usize::try_from(maximum)
            .map(|maximum| current.min(maximum))
            .map_err(|_| internal_error())
    })
}

fn repository_index(
    lanes: &FirstSliceServiceLanes,
    resources: ServiceRequestResources<'_>,
    request: daemon::RepositoryIndexRequest,
    context: &FirstSliceIpcContext,
    reply: &mut Option<Reply>,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    repository_index_with_intent(
        lanes,
        resources,
        request,
        context,
        reply,
        RepositoryIndexIntent::Requested,
        None,
    )
}

#[derive(Clone, Copy)]
enum RepositoryIndexIntent {
    Requested,
    SemanticRefinement { structural_generation: GenerationId },
}

fn repository_index_with_intent(
    lanes: &FirstSliceServiceLanes,
    resources: ServiceRequestResources<'_>,
    request: daemon::RepositoryIndexRequest,
    context: &FirstSliceIpcContext,
    reply: &mut Option<Reply>,
    intent: RepositoryIndexIntent,
    admission_ack: Option<&SyncSender<Result<(), PublicError>>>,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    let service = &lanes.service;
    let index_serialization = &lanes.index_serialization;
    let semantic_refinements = &lanes.semantic_refinements;
    let refinement = &lanes.refinement;
    let ServiceRequestResources {
        journal,
        metadata,
        runtime,
        publication_hook,
        ..
    } = resources;
    let operation = parse_operation(request.operation.as_ref())?;
    let requested_mode =
        daemon::RepositoryIndexMode::try_from(request.mode).map_err(|_| invalid_argument())?;
    let auto_refinement = matches!(intent, RepositoryIndexIntent::Requested)
        && requested_mode == daemon::RepositoryIndexMode::RepositoryIndexAuto
        && read_service(service)?.deep_analysis_available();
    let mode = match intent {
        RepositoryIndexIntent::SemanticRefinement { .. } => FirstSliceIndexMode::Deep,
        RepositoryIndexIntent::Requested => match requested_mode {
            daemon::RepositoryIndexMode::Unspecified
            | daemon::RepositoryIndexMode::RepositoryIndexStructural => {
                FirstSliceIndexMode::Structural
            }
            daemon::RepositoryIndexMode::RepositoryIndexDeep => FirstSliceIndexMode::Deep,
            // Auto publishes the interactive structural stage first. A separate
            // journaled operation below owns the later semantic publication.
            daemon::RepositoryIndexMode::RepositoryIndexAuto => FirstSliceIndexMode::Structural,
        },
    };
    if matches!(intent, RepositoryIndexIntent::SemanticRefinement { .. })
        && requested_mode != daemon::RepositoryIndexMode::RepositoryIndexDeep
    {
        return Err(invalid_argument());
    }
    let root = PathBuf::from(&request.root);
    let requested_repository = read_service(service)?
        .registered_repository_for_root(&root, &context.cancellation)
        .map_err(service_error)?;
    let detached = request.detached;
    let work_deadline = if detached {
        Instant::now()
            .checked_add(DETACHED_INDEX_TIMEOUT)
            .ok_or_else(internal_error)?
    } else {
        context.deadline
    };
    let lifecycle_deadline = lifecycle_deadline(work_deadline)?;
    let deadline_unix_ms = deadline_unix_ms(work_deadline)?;
    let mut plan_hasher = blake3::Hasher::new();
    match intent {
        RepositoryIndexIntent::Requested => {
            // Preserve the original caller-plan identity across the two-stage
            // activation change so a durable Auto submission remains retryable.
            let plan_mode = if requested_mode == daemon::RepositoryIndexMode::RepositoryIndexAuto
                && read_service(service)?.deep_analysis_available()
            {
                FirstSliceIndexMode::Deep
            } else {
                mode
            };
            plan_hasher.update(b"rootlight.repository-index-plan/1\0");
            plan_hasher.update(request.root.as_bytes());
            plan_hasher.update(&[repository_index_mode_tag(plan_mode)]);
        }
        RepositoryIndexIntent::SemanticRefinement {
            structural_generation,
        } => {
            plan_hasher.update(b"rootlight.repository-index-plan/2\0");
            plan_hasher.update(request.root.as_bytes());
            plan_hasher.update(&[repository_index_mode_tag(mode)]);
            plan_hasher.update(b"\0semantic-refinement\0");
            plan_hasher.update(structural_generation.as_bytes());
        }
    }
    let mut submission = OperationSubmission::new(
        operation,
        OperationKind::RepositoryIndex,
        PlanHash::from_bytes(*plan_hasher.finalize().as_bytes()),
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
            let repository_context = match journal_call(
                runtime,
                context.deadline,
                journal.repository_operation_context(operation),
            ) {
                Ok(context) => {
                    let submission = RepositoryOperationSubmission::new(
                        context.repository,
                        context.parent_generation,
                        context.started_unix_ms,
                        context.estimated_disk_bytes,
                        context.mode,
                    )
                    .map_err(|error| operation_error(&error, Some(operation)))?;
                    Some(match context.root_identity {
                        Some(root_identity) => submission.with_root_identity(root_identity),
                        None => submission,
                    })
                }
                Err(error) if error.code() == ErrorCode::NotFound => None,
                Err(error) => return Err(error),
            };
            let retry = OperationSubmission {
                deadline_unix_ms: existing.deadline_unix_ms,
                lease_expires_unix_ms: existing.lease_expires_unix_ms,
                repository_context,
                ..submission
            };
            let existing = journal_call(runtime, context.deadline, journal.retry_status(retry))?;
            if let Some(admitted) = admission_ack {
                let _ = admitted.try_send(Ok(()));
            }
            let response = retry_index_response(metadata, existing, mode)?;
            if auto_refinement {
                return complete_auto_structural_index(
                    &request.root,
                    operation,
                    context,
                    reply,
                    response,
                    semantic_refinements,
                    refinement,
                );
            }
            return Ok(response);
        }
        Err(error) if error.code() == ErrorCode::NotFound => {}
        Ok(_) => return Err(internal_error()),
        Err(error) => return Err(error),
    }
    if matches!(intent, RepositoryIndexIntent::Requested)
        && let Some(repository) = requested_repository
    {
        cancel_semantic_refinements(semantic_refinements, repository)?;
    }
    let started_unix_ms = unix_time_ms()?;
    let service_guard = read_service(service)?;
    let admission = service_guard
        .admit_rust_fixture(&root, &context.cancellation)
        .map_err(service_error)?;
    drop(service_guard);
    let metadata_admission = match lock_metadata(metadata) {
        Ok(mut operation_metadata) => operation_metadata
            .reserve(operation, started_unix_ms, Some(admission.repository))
            .map(|()| operation_metadata.admit(operation, admission)),
        Err(error) => Err(error),
    };
    if let Err(error) = metadata_admission {
        read_service(service)?.release_index_admission(admission);
        return Err(error);
    }
    submission = submission
        .with_repository_context(
            RepositoryOperationSubmission::new(
                admission.repository,
                admission.parent,
                started_unix_ms,
                admission.estimated_disk_bytes,
                if auto_refinement {
                    RepositoryOperationMode::Auto
                } else {
                    repository_operation_mode(mode)
                },
            )
            .unwrap_or_else(|_| unreachable!("wall-clock starts are nonzero"))
            .with_root_identity(*admission.root_identity.as_bytes()),
        )
        .unwrap_or_else(|_| unreachable!("repository context matches the operation kind"));
    let submitted =
        match journal_lifecycle_call(runtime, journal.submit_until(submission, context.deadline)) {
            Ok(submitted) => submitted,
            Err(error) => {
                lock_metadata(metadata)?.remove_unpublished(operation);
                read_service(service)?.release_index_admission(admission);
                return Err(error);
            }
        };
    if !submitted.inserted {
        if let Some(admitted) = admission_ack {
            let _ = admitted.try_send(Ok(()));
        }
        return retry_index_response(metadata, submitted.operation, mode);
    }
    if let Some(admission) = context.index_admission.as_ref() {
        admission.mark_inserted();
    }
    if let Some(admitted) = admission_ack {
        let _ = admitted.try_send(Ok(()));
    }
    let peak_rss_bytes = lock_metadata(metadata)?.resource_meter(operation)?;
    let _rss_sampler =
        ProcessRssSampler::start(peak_rss_bytes, lanes.support_state.as_ref().map(Arc::clone));
    if let Some(state) = lanes.support_state.as_deref() {
        state.record_repository_index_context(
            operation,
            admission.repository,
            repository_index_support_provider(first_slice_index_provider(mode)),
        );
    }
    if detached && let Some(reply) = reply.take() {
        let response = admitted_index_response(admission, &submitted.operation, mode);
        let _ = reply.send(Ok(FirstSliceIpcResponse::RepositoryIndex(response)));
    }
    let result = (|| {
        if let Some(hook) = publication_hook
            && let Err(error) = hook.pause(PublicationBoundary::AfterAdmission)
        {
            finish_failed_index(
                runtime,
                lifecycle_deadline,
                journal,
                metadata,
                operation,
                &context.cancellation,
                &error,
            )?;
            return Err(error);
        }
        if propagate_peer_cancellation(runtime, journal, operation, context, lifecycle_deadline)? {
            lock_metadata(metadata)?.mark_terminal(operation);
            return Err(cancelled_error());
        }
        let (_, cancellation) = journal_lifecycle_call(
            runtime,
            journal.activate_operation_until(operation, lifecycle_deadline),
        )?;
        // The journal owns cancellation fan-out while the IPC boundary owns the
        // process-local work deadline. Installing it on this exact token keeps
        // later journal cancellation linked to every synchronous service stage.
        if let Err(error) = bind_journal_cancellation_deadline(&cancellation, work_deadline) {
            finish_failed_index(
                runtime,
                lifecycle_deadline,
                journal,
                metadata,
                operation,
                &cancellation,
                &error,
            )?;
            return Err(error);
        }
        if matches!(intent, RepositoryIndexIntent::SemanticRefinement { .. }) {
            activate_semantic_refinement(semantic_refinements, operation, cancellation.clone())?;
        }
        if let Some(hook) = publication_hook
            && let Err(error) = hook.pause(PublicationBoundary::AfterActivation)
        {
            finish_failed_index(
                runtime,
                lifecycle_deadline,
                journal,
                metadata,
                operation,
                &cancellation,
                &error,
            )?;
            return Err(error);
        }
        let mut progress_failure = None;
        let preparation = {
            let mut observe_progress = |observed: FirstSliceIndexProgress| {
                if progress_failure.is_some() {
                    return;
                }
                let progress = match operation_progress(observed) {
                    Ok(progress) => progress,
                    Err(error) => {
                        progress_failure = Some(error);
                        return;
                    }
                };
                if let Err(error) = journal_lifecycle_call(
                    runtime,
                    journal.update_progress_until(operation, progress, lifecycle_deadline),
                ) {
                    progress_failure = Some(error);
                    return;
                }
                match lock_metadata(metadata) {
                    Ok(mut metadata) => metadata.observe_inputs(
                        operation,
                        observed.files_examined,
                        observed.bytes_examined,
                    ),
                    Err(error) => {
                        progress_failure = Some(error);
                        return;
                    }
                }
                if let Err(error) = journal_lifecycle_call(
                    runtime,
                    journal.update_repository_observation_until(
                        operation,
                        observed.files_examined,
                        observed.bytes_examined,
                        lifecycle_deadline,
                    ),
                ) {
                    progress_failure = Some(error);
                    return;
                }
                if observed.written_bytes > 0
                    && let Err(error) = persist_operation_resources(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        metadata,
                        operation,
                        observed.written_bytes,
                    )
                {
                    progress_failure = Some(error);
                }
            };
            let service = read_service(service)?;
            match intent {
                RepositoryIndexIntent::Requested => service
                    .prepare_repository_with_mode_and_progress(
                        &root,
                        mode,
                        &cancellation,
                        &mut observe_progress,
                    ),
                RepositoryIndexIntent::SemanticRefinement {
                    structural_generation,
                } => service.prepare_semantic_refinement_with_progress(
                    &root,
                    structural_generation,
                    &cancellation,
                    &mut observe_progress,
                ),
            }
        };
        if let Some(error) = progress_failure {
            finish_failed_index(
                runtime,
                lifecycle_deadline,
                journal,
                metadata,
                operation,
                &cancellation,
                &error,
            )?;
            return Err(error);
        }
        match preparation {
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
                        metadata,
                        operation,
                        &cancellation,
                        &cancelled_error(),
                    )?;
                    return Err(cancelled_error());
                }
                // Independent repositories prepare concurrently. Only the
                // generation switch is serialized, and the same prepared value
                // remains owned while transient readers leave the boundary.
                let (_publication, mut service_guard) = match lock_publication_until(
                    index_serialization,
                    service,
                    &cancellation,
                    work_deadline,
                ) {
                    Ok(guards) => guards,
                    Err(error) => {
                        let public = publication_error(operation, error);
                        finish_failed_index(
                            runtime,
                            lifecycle_deadline,
                            journal,
                            metadata,
                            operation,
                            &cancellation,
                            &public,
                        )?;
                        return Err(public);
                    }
                };
                if let RepositoryIndexIntent::SemanticRefinement {
                    structural_generation,
                } = intent
                    && service_guard.active_generation_for(admission.repository)
                        != Some(structural_generation)
                {
                    let error = stale_generation();
                    finish_failed_index(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        metadata,
                        operation,
                        &cancellation,
                        &error,
                    )?;
                    return Err(error);
                }
                let staged = match service_guard
                    .stage_prepared(prepared, &cancellation)
                    .map_err(service_error)
                {
                    Ok(staged) => staged,
                    Err(error) => {
                        let public = publication_error(operation, error);
                        finish_failed_index(
                            runtime,
                            lifecycle_deadline,
                            journal,
                            metadata,
                            operation,
                            &cancellation,
                            &public,
                        )?;
                        return Err(public);
                    }
                };
                drop(service_guard);
                let staged_written_bytes = staged.written_bytes();
                let staged_receipt = staged.receipt();
                {
                    let mut metadata = lock_metadata(metadata)?;
                    metadata.stage(operation, staged_receipt.clone());
                    metadata.observe_written_bytes(operation, staged_written_bytes);
                }
                if let Err(error) = persist_operation_resources(
                    runtime,
                    lifecycle_deadline,
                    journal,
                    metadata,
                    operation,
                    staged_written_bytes,
                ) {
                    write_service(service)?
                        .discard_staged(staged)
                        .map_err(service_error)?;
                    finish_failed_index(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        metadata,
                        operation,
                        &cancellation,
                        &error,
                    )?;
                    return Err(error);
                }
                if let Some(hook) = publication_hook
                    && let Err(error) = hook.pause(PublicationBoundary::BeforeCompletion)
                {
                    let discard = write_service(service)?
                        .discard_staged(staged)
                        .map_err(service_error);
                    let terminal = finish_failed_index(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        metadata,
                        operation,
                        &cancellation,
                        &error,
                    );
                    discard?;
                    terminal?;
                    return Err(error);
                }
                propagate_peer_cancellation(
                    runtime,
                    journal,
                    operation,
                    context,
                    lifecycle_deadline,
                )?;
                let durable_publication = read_service(service)?.uses_durable_publication();
                let completion = if durable_publication {
                    journal_lifecycle_call(
                        runtime,
                        journal.authorize_publication_until(
                            operation,
                            context.index_admission.clone(),
                            lifecycle_deadline,
                        ),
                    )
                } else {
                    match context.index_admission.clone() {
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
                    }
                };
                let publication_record = match completion {
                    Ok(record) => record,
                    Err(error) => {
                        write_service(service)?
                            .discard_staged(staged)
                            .map_err(service_error)?;
                        finish_failed_index(
                            runtime,
                            lifecycle_deadline,
                            journal,
                            metadata,
                            operation,
                            &cancellation,
                            &error,
                        )?;
                        return Err(error);
                    }
                };
                let expected_publication_state = if durable_publication {
                    publication_record.state == OperationState::Running
                        && publication_record.stage == OperationStage::Cleanup
                } else {
                    publication_record.state == OperationState::Succeeded
                };
                if !expected_publication_state {
                    write_service(service)?
                        .discard_staged(staged)
                        .map_err(service_error)?;
                    let mut metadata = lock_metadata(metadata)?;
                    metadata.discard(operation)?;
                    metadata.mark_terminal(operation);
                    return Err(cancelled_error());
                }
                if let Some(hook) = publication_hook
                    && let Err(error) = hook.pause(PublicationBoundary::AfterSuccess)
                {
                    write_service(service)?
                        .discard_staged(staged)
                        .map_err(service_error)?;
                    finish_failed_index(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        metadata,
                        operation,
                        &cancellation,
                        &error,
                    )?;
                    return Err(error);
                }
                let commit = if publication_hook.is_some_and(PublicationBoundaryHook::fail_commit) {
                    match write_service(service)?.discard_staged(staged) {
                        Ok(()) => Err(FirstSliceError::Retention),
                        Err(error) => Err(error),
                    }
                } else {
                    write_service(service)?.commit_staged_for_operation_with_metrics(
                        staged,
                        FirstSliceOperationContext {
                            operation,
                            started_unix_ms,
                            provider: first_slice_index_provider(mode),
                        },
                    )
                };
                let (receipt, written_bytes) = match commit {
                    Ok(commit) => {
                        let (receipt, written_bytes) = commit.into_parts();
                        if receipt != staged_receipt {
                            lock_metadata(metadata)?.stage(operation, receipt.clone());
                        }
                        (receipt, written_bytes)
                    }
                    Err(_error) => {
                        let public = failed_closed_publication(operation);
                        finish_failed_index(
                            runtime,
                            lifecycle_deadline,
                            journal,
                            metadata,
                            operation,
                            &cancellation,
                            &public,
                        )?;
                        return Err(public);
                    }
                };
                refresh_index_support_inventory(service, lanes.support_state.as_deref());
                {
                    lock_metadata(metadata)?.observe_written_bytes(operation, written_bytes);
                }
                let resource_update = persist_operation_resources(
                    runtime,
                    lifecycle_deadline,
                    journal,
                    metadata,
                    operation,
                    written_bytes,
                );
                let operation_record = if durable_publication {
                    let finalization = match publication_hook {
                        Some(hook) => hook.pause(PublicationBoundary::AfterCommit).and_then(|()| {
                            journal_lifecycle_call(
                                runtime,
                                journal.finish_authorized_publication(operation),
                            )
                        }),
                        None => journal_lifecycle_call(
                            runtime,
                            journal.finish_authorized_publication(operation),
                        ),
                    };
                    match finalization {
                        Ok(record) if record.state == OperationState::Succeeded => record,
                        Ok(_) | Err(_) => journal_lifecycle_call(
                            runtime,
                            journal.reconcile_committed_publication(operation),
                        )
                        .and_then(|record| {
                            (record.state == OperationState::Succeeded)
                                .then_some(record)
                                .ok_or_else(internal_error)
                        })?,
                    }
                } else {
                    publication_record
                };
                let operation_record = match resource_update {
                    Ok(_) => operation_record,
                    Err(_) => persist_operation_resources(
                        runtime,
                        lifecycle_deadline,
                        journal,
                        metadata,
                        operation,
                        written_bytes,
                    )
                    .unwrap_or(operation_record),
                };
                let publication_deadline = fresh_lifecycle_deadline(lifecycle_deadline)?;
                let publication_projection = journal_lifecycle_call(
                    runtime,
                    journal.record_repository_publication_until(
                        operation,
                        receipt.generation,
                        publication_deadline,
                    ),
                );
                if publication_projection.is_err() {
                    let retry_deadline = fresh_lifecycle_deadline(lifecycle_deadline)?;
                    let _ = journal_lifecycle_call(
                        runtime,
                        journal.record_repository_publication_until(
                            operation,
                            receipt.generation,
                            retry_deadline,
                        ),
                    );
                }
                let mut metadata = lock_metadata(metadata)?;
                metadata.observe_terminal(&operation_record);
                metadata.commit(operation)?;
                Ok(index_response(receipt, &operation_record, mode))
            }
            Err(error) => {
                let public = repository_index_error(
                    error,
                    RepositoryIndexErrorContext {
                        operation,
                        repository: admission.repository,
                        provider: repository_index_provider(mode),
                    },
                );
                finish_failed_index(
                    runtime,
                    lifecycle_deadline,
                    journal,
                    metadata,
                    operation,
                    &cancellation,
                    &public,
                )?;
                Err(public)
            }
        }
    })();
    if result.as_ref().is_err_and(|error| {
        matches!(
            error.code(),
            ErrorCode::AdapterFailed | ErrorCode::ResourceExhausted
        )
    }) && matches!(intent, RepositoryIndexIntent::SemanticRefinement { .. })
        && let Some(state) = lanes.support_state.as_deref()
    {
        state.set_adapter_status(HealthStatus::Degraded);
    }
    if result.is_err() {
        read_service(service)?
            .restore_repository_registration(admission.root_identity, admission.repository)
            .map_err(service_error)?;
    }
    match result {
        Ok(response) if auto_refinement => complete_auto_structural_index(
            &request.root,
            operation,
            context,
            reply,
            response,
            semantic_refinements,
            refinement,
        ),
        result => result,
    }
}

fn complete_auto_structural_index(
    root: &str,
    operation: OperationId,
    context: &FirstSliceIpcContext,
    reply: &mut Option<Reply>,
    mut response: daemon::RepositoryIndexResponse,
    semantic_refinements: &SemanticRefinements,
    refinement: &SyncSender<SemanticRefinementCommand>,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    let Some(published_generation) = response.published_generation.as_ref() else {
        return Ok(response);
    };
    let structural_generation = parse_generation(Some(published_generation))?;
    let repository = parse_repository(response.repository.as_ref())?;
    let semantic_operation = semantic_refinement_operation(operation);

    let refinement_deadline = Instant::now()
        .checked_add(DETACHED_INDEX_TIMEOUT)
        .ok_or_else(internal_error)?;
    let cancellation = Cancellation::with_deadline(refinement_deadline);
    let refinement_context = FirstSliceIpcContext {
        client_instance_id: context.client_instance_id,
        selected_protocol_minor: context.selected_protocol_minor,
        cancellation: cancellation.clone(),
        deadline: refinement_deadline,
        effective_budget: context.effective_budget,
        index_admission: None,
    };
    let (admitted, admission) = mpsc::sync_channel(1);
    let command = SemanticRefinementCommand {
        request: daemon::RepositoryIndexRequest {
            schema_version: Some(schema_version()),
            root: root.to_owned(),
            operation: Some(operation_to_wire(semantic_operation)),
            detached: true,
            mode: daemon::RepositoryIndexMode::RepositoryIndexDeep as i32,
        },
        context: refinement_context,
        operation: semantic_operation,
        repository,
        structural_generation,
        admitted,
    };
    let mut scheduled = false;
    if register_semantic_refinement(
        semantic_refinements,
        semantic_operation,
        repository,
        cancellation,
    )? {
        match refinement.try_send(command) {
            Ok(()) => {
                scheduled = true;
            }
            Err(TrySendError::Full(_)) => {
                remove_semantic_refinement(semantic_refinements, semantic_operation)?;
            }
            Err(TrySendError::Disconnected(_)) => {
                remove_semantic_refinement(semantic_refinements, semantic_operation)?;
            }
        }
    }
    if scheduled
        && matches!(
            admission.recv_timeout(REFINEMENT_ADMISSION_WAIT),
            Ok(Ok(()))
        )
    {
        response.semantic_operation = Some(operation_to_wire(semantic_operation));
    }

    // The bounded refinement lane now owns the expensive follow-up. Queries
    // share the immutable service view while it prepares the next generation.
    if let Some(reply) = reply.take() {
        let _ = reply.send(Ok(FirstSliceIpcResponse::RepositoryIndex(response.clone())));
    }
    Ok(response)
}

fn register_semantic_refinement(
    refinements: &Mutex<BTreeMap<OperationId, PendingSemanticRefinement>>,
    operation: OperationId,
    repository: RepositoryId,
    cancellation: Cancellation,
) -> Result<bool, PublicError> {
    let mut refinements = refinements.lock().map_err(|_| internal_error())?;
    match refinements.entry(operation) {
        std::collections::btree_map::Entry::Occupied(_) => Ok(false),
        std::collections::btree_map::Entry::Vacant(entry) => {
            entry.insert(PendingSemanticRefinement {
                repository,
                cancellation,
            });
            Ok(true)
        }
    }
}

fn activate_semantic_refinement(
    refinements: &Mutex<BTreeMap<OperationId, PendingSemanticRefinement>>,
    operation: OperationId,
    cancellation: Cancellation,
) -> Result<(), PublicError> {
    let mut refinements = refinements.lock().map_err(|_| internal_error())?;
    let pending = refinements.get_mut(&operation).ok_or_else(internal_error)?;
    if let Some(reason) = pending.cancellation.reason() {
        let _ = cancellation.cancel(reason);
    }
    pending.cancellation = cancellation;
    Ok(())
}

fn cancel_semantic_refinements(
    refinements: &Mutex<BTreeMap<OperationId, PendingSemanticRefinement>>,
    repository: RepositoryId,
) -> Result<(), PublicError> {
    let refinements = refinements.lock().map_err(|_| internal_error())?;
    for pending in refinements.values() {
        if pending.repository == repository {
            let _ = pending
                .cancellation
                .cancel(CancellationReason::ParentCancelled);
        }
    }
    Ok(())
}

fn cancel_all_semantic_refinements(
    refinements: &Mutex<BTreeMap<OperationId, PendingSemanticRefinement>>,
    reason: CancellationReason,
) -> Result<(), FirstSliceHostError> {
    let refinements = refinements
        .lock()
        .map_err(|_| FirstSliceHostError::ThreadPanicked)?;
    for pending in refinements.values() {
        let _ = pending.cancellation.cancel(reason);
    }
    Ok(())
}

fn remove_semantic_refinement(
    refinements: &Mutex<BTreeMap<OperationId, PendingSemanticRefinement>>,
    operation: OperationId,
) -> Result<(), PublicError> {
    refinements
        .lock()
        .map_err(|_| internal_error())?
        .remove(&operation);
    Ok(())
}

fn semantic_refinement_operation(operation: OperationId) -> OperationId {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"rootlight.semantic-refinement-operation/1\0");
    hasher.update(operation.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    if bytes == *operation.as_bytes() {
        bytes[0] ^= 1;
    }
    OperationId::from_bytes(bytes)
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
            ControlRequest::OperationCancel {
                operation,
                authority: CancellationAuthority::Internal(
                    InternalCancellationAuthority::ClientDisconnect,
                ),
            },
            lifecycle_deadline,
        ),
    )?;
    match response {
        ControlResponse::OperationCancel { .. } => Ok(true),
        ControlResponse::Error(error) => Err(error),
        _ => Err(internal_error()),
    }
}

#[cfg(test)]
fn reject_index_admission(
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    operation: OperationId,
    cancellation: &Cancellation,
    error: PublicError,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    // A rejected admission still owns a durable queued record. Promote only
    // its lifecycle so normal terminalization cannot strand it as queued.
    match journal_lifecycle_call(
        runtime,
        journal.activate_operation_until(operation, deadline),
    ) {
        Ok(_) => reject_activated_index_admission(
            runtime,
            deadline,
            journal,
            metadata,
            operation,
            cancellation,
            error,
        ),
        Err(_) => {
            // Both actor lanes may be unavailable. This narrow compensator
            // serializes directly in the journal after activation proved that
            // no worker owns the durable operation.
            let record = journal
                .compensate_unowned_operation(operation, error.clone(), cancellation.reason())
                .map_err(|error| operation_error(&error, Some(operation)))?;
            let mut metadata = lock_metadata(metadata)?;
            metadata.observe_terminal(&record);
            metadata.mark_terminal(operation);
            Err(rejected_index_response_error(
                &record,
                cancellation.reason(),
                error,
            ))
        }
    }
}

#[cfg(test)]
fn reject_activated_index_admission(
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    operation: OperationId,
    cancellation: &Cancellation,
    error: PublicError,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    let record = finish_failed_index(
        runtime,
        deadline,
        journal,
        metadata,
        operation,
        cancellation,
        &error,
    )?;
    Err(rejected_index_response_error(
        &record,
        cancellation.reason(),
        error,
    ))
}

#[cfg(test)]
fn rejected_index_response_error(
    record: &OperationRecord,
    local_reason: Option<CancellationReason>,
    error: PublicError,
) -> PublicError {
    let operation = record.operation;
    let durable_reason = record.cancellation_reason.or_else(|| {
        (record.state == OperationState::Interrupted
            && record.recovery_class == RecoveryClass::DeadlineElapsed)
            .then_some(CancellationReason::DeadlineExceeded)
    });
    match record.state {
        OperationState::Failed => record.error.clone().unwrap_or_else(internal_error),
        OperationState::Cancelled | OperationState::Interrupted => match durable_reason {
            Some(reason) if Some(reason) == local_reason => error,
            Some(reason) => cancellation_error(reason),
            None if record.state == OperationState::Cancelled => {
                terminal_operation_error(operation, "repository index was cancelled")
            }
            None => terminal_operation_error(operation, "repository index was interrupted"),
        },
        OperationState::Queued
        | OperationState::Running
        | OperationState::Cancelling
        | OperationState::Succeeded => internal_error(),
    }
}

fn finish_failed_index(
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    operation: OperationId,
    cancellation: &Cancellation,
    error: &PublicError,
) -> Result<OperationRecord, PublicError> {
    // Terminal state precedes best-effort resource accounting. Each required
    // mutation receives its own bounded window so a claimed predecessor cannot
    // consume the deadline of the mutation that prevents ownerless work.
    let terminal_deadline = fresh_lifecycle_deadline(deadline)?;
    let compensate = |reason| {
        journal
            .compensate_unowned_operation(operation, error.clone(), reason)
            .map_err(|error| operation_error(&error, Some(operation)))
    };
    let fail = || {
        let failure_deadline = fresh_lifecycle_deadline(deadline)?;
        match runtime.block_on(journal.fail_operation_until(
            operation,
            error.clone(),
            failure_deadline,
        )) {
            Ok(record) if record.state.is_terminal() => Ok(record),
            Ok(_) | Err(_) => compensate(None),
        }
    };
    let record = if let Some(reason) = cancellation.reason() {
        match runtime.block_on(journal.finish_operation_until(
            operation,
            Some(reason),
            terminal_deadline,
        )) {
            Ok(record) if record.state.is_terminal() => record,
            Ok(_) | Err(ServiceError::Operations(OperationError::CancellationTooLate)) => fail()?,
            Err(_) => compensate(Some(reason))?,
        }
    } else {
        match runtime.block_on(journal.fail_operation_until(
            operation,
            error.clone(),
            terminal_deadline,
        )) {
            Ok(record) if record.state.is_terminal() => record,
            Ok(_) | Err(_) => compensate(None)?,
        }
    };
    if !record.state.is_terminal() {
        return Err(internal_error());
    }
    let persist_resources = || {
        let resource_deadline = fresh_lifecycle_deadline(deadline)?;
        persist_operation_resources(runtime, resource_deadline, journal, metadata, operation, 0)
    };
    let record = match persist_resources() {
        Ok(record) => record,
        Err(_) => persist_resources().unwrap_or(record),
    };
    let mut metadata = lock_metadata(metadata)?;
    metadata.observe_terminal(&record);
    metadata.mark_terminal(operation);
    Ok(record)
}

fn persist_operation_resources(
    runtime: &tokio::runtime::Runtime,
    deadline: Instant,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    operation: OperationId,
    written_bytes: u64,
) -> Result<OperationRecord, PublicError> {
    let peak_rss_bytes = {
        let mut metadata = lock_metadata(metadata)?;
        metadata.observe_written_bytes(operation, written_bytes);
        metadata.resource_meter(operation)?
    };
    sample_current_process_rss(&peak_rss_bytes, None);
    let (peak_rss_bytes, written_bytes) = lock_metadata(metadata)?.resources(operation)?;
    journal_lifecycle_call(
        runtime,
        journal.update_resources_until(operation, peak_rss_bytes, written_bytes, deadline),
    )
}

fn bind_journal_cancellation_deadline(
    cancellation: &Cancellation,
    deadline: Instant,
) -> Result<(), PublicError> {
    if cancellation.has_deadline() {
        return Err(internal_error());
    }
    cancellation.extend_deadline(deadline).map_err(|_| {
        if let Some(reason) = cancellation.reason() {
            cancellation_error(reason)
        } else {
            internal_error()
        }
    })
}

fn retry_index_response(
    metadata: &Mutex<OperationMetadataSet>,
    operation: OperationRecord,
    mode: FirstSliceIndexMode,
) -> Result<daemon::RepositoryIndexResponse, PublicError> {
    let metadata = lock_metadata(metadata)?
        .records
        .get(&operation.operation)
        .cloned()
        .ok_or_else(unsupported_restart_state)?;
    if metadata.publication == PublicationState::FailedClosed {
        return Err(failed_closed_publication(operation.operation));
    }
    match operation.state {
        OperationState::Queued | OperationState::Running | OperationState::Cancelling => {
            let repository = metadata.repository.ok_or_else(operation_in_progress)?;
            if metadata.estimated_disk_bytes == 0 {
                return Err(operation_in_progress());
            }
            return Ok(pending_index_response(
                repository,
                metadata.parent_generation,
                metadata.estimated_disk_bytes,
                &operation,
                mode,
            ));
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
    match metadata.publication {
        PublicationState::Staged => return Err(operation_in_progress()),
        PublicationState::Committed => {}
        PublicationState::None | PublicationState::FailedClosed => return Err(internal_error()),
    }
    let receipt = metadata.receipt.ok_or_else(internal_error)?;
    Ok(index_response(receipt, &operation, mode))
}

fn admitted_index_response(
    admission: FirstSliceIndexAdmission,
    operation: &OperationRecord,
    mode: FirstSliceIndexMode,
) -> daemon::RepositoryIndexResponse {
    pending_index_response(
        admission.repository,
        admission.parent,
        admission.estimated_disk_bytes,
        operation,
        mode,
    )
}

fn pending_index_response(
    repository: RepositoryId,
    parent: Option<GenerationId>,
    estimated_disk_bytes: u64,
    operation: &OperationRecord,
    mode: FirstSliceIndexMode,
) -> daemon::RepositoryIndexResponse {
    daemon::RepositoryIndexResponse {
        schema_version: Some(schema_version()),
        repository: Some(repository_to_wire(repository)),
        operation: Some(operation_to_wire(operation.operation)),
        state: operation_state_to_wire(operation.state) as i32,
        revision: operation.revision,
        parent_generation: parent.map(generation_to_wire),
        published_generation: None,
        discovered_inputs: 0,
        indexed_files: 0,
        entities: 0,
        elapsed_micros: 0,
        estimated_disk_bytes,
        diagnostics: Vec::new(),
        mode: repository_index_mode_to_wire(mode) as i32,
        semantic_operation: None,
    }
}

fn index_response(
    receipt: FirstSliceIndexReceipt,
    operation: &OperationRecord,
    mode: FirstSliceIndexMode,
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
        estimated_disk_bytes: receipt.estimated_disk_bytes,
        diagnostics: receipt
            .diagnostics
            .into_iter()
            .map(|diagnostic| daemon::RepositoryIndexDiagnostic {
                code: diagnostic.code,
                message: diagnostic.message,
            })
            .collect(),
        mode: repository_index_mode_to_wire(mode) as i32,
        semantic_operation: None,
    }
}

fn repository_operation_status(
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    runtime: &tokio::runtime::Runtime,
    request: daemon::RepositoryOperationStatusRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::RepositoryOperationStatusResponse, PublicError> {
    context
        .cancellation
        .check()
        .map_err(|cancelled| cancellation_error(cancelled.reason()))?;
    let operation = parse_operation(request.operation.as_ref())?;
    let action = daemon::RepositoryOperationAction::try_from(request.action)
        .map_err(|_| invalid_argument())?;
    let control = if action == daemon::RepositoryOperationAction::RepositoryOperationCancel {
        ControlRequest::OperationCancel {
            operation,
            authority: CancellationAuthority::Client(context.client_instance_id),
        }
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
    let metadata = match lock_metadata(metadata)?.records.get(&operation).cloned() {
        Some(metadata) => metadata,
        None => {
            let durable_context = journal_call(
                runtime,
                context.deadline,
                journal.repository_operation_context(operation),
            )?;
            OperationMetadata::from_durable_context(durable_context, &record)
        }
    };
    if metadata.publication == PublicationState::FailedClosed {
        // Journal success closes the cancellation race before the process-local
        // generation commit. A later commit failure cannot rewrite that durable
        // terminal record, so the publication projection is authoritative at
        // the public boundary and exposes the required rebuild recovery.
        let mut visible = record;
        visible.state = OperationState::Failed;
        visible.error = Some(failed_closed_publication(operation));
        let (peak_rss_bytes, written_bytes) = public_operation_resources(&visible, &metadata);
        return Ok(daemon::RepositoryOperationStatusResponse {
            schema_version: Some(schema_version()),
            operation: Some(operation_record_to_wire(&visible)),
            published_generation: None,
            started_unix_ms: metadata.started_unix_ms,
            peak_rss_bytes,
            written_bytes,
            files_examined: metadata.files_examined,
            retry_after_ms: None,
            bytes_examined: metadata.bytes_examined,
            index_stage: repository_operation_stage(&visible).to_owned(),
            semantic_operation: None,
        });
    }
    let published_generation = if record.state == OperationState::Succeeded {
        match metadata.publication {
            PublicationState::Staged => return Err(operation_in_progress()),
            PublicationState::Committed => {}
            PublicationState::None | PublicationState::FailedClosed => {
                return Err(internal_error());
            }
        }
        Some(
            metadata
                .published_generation
                .or_else(|| metadata.receipt.as_ref().map(|receipt| receipt.generation))
                .ok_or_else(internal_error)?,
        )
    } else {
        None
    };
    let (peak_rss_bytes, written_bytes) = public_operation_resources(&record, &metadata);
    let semantic_operation = if record.state == OperationState::Succeeded {
        let repository_context = match journal_call(
            runtime,
            context.deadline,
            journal.repository_operation_context(operation),
        ) {
            Ok(context) => Some(context),
            Err(error) if error.code() == ErrorCode::NotFound => None,
            Err(error) => return Err(error),
        };
        if repository_context.is_some_and(|context| context.mode == RepositoryOperationMode::Auto) {
            let candidate = semantic_refinement_operation(operation);
            match journal_call(
                runtime,
                context.deadline,
                journal.control(ControlRequest::OperationStatus(candidate)),
            ) {
                Ok(ControlResponse::OperationStatus(child))
                    if child.kind == OperationKind::RepositoryIndex =>
                {
                    Some(candidate)
                }
                Ok(ControlResponse::OperationStatus(_)) | Ok(_) => return Err(internal_error()),
                Err(error) if error.code() == ErrorCode::NotFound => None,
                Err(error) => return Err(error),
            }
        } else {
            None
        }
    } else {
        None
    };
    Ok(daemon::RepositoryOperationStatusResponse {
        schema_version: Some(schema_version()),
        operation: Some(operation_record_to_wire(&record)),
        published_generation: published_generation.map(generation_to_wire),
        started_unix_ms: metadata.started_unix_ms,
        peak_rss_bytes,
        written_bytes,
        files_examined: metadata.files_examined,
        retry_after_ms: (!record.state.is_terminal()).then_some(RETRY_AFTER_MS),
        bytes_examined: metadata.bytes_examined,
        index_stage: repository_operation_stage(&record).to_owned(),
        semantic_operation: semantic_operation.map(operation_to_wire),
    })
}

const fn repository_operation_stage(record: &OperationRecord) -> &'static str {
    if matches!(record.state, OperationState::Succeeded) {
        return "complete";
    }
    match record.progress.completed {
        0 => "discovery",
        1 => "snapshot",
        2 => "analysis",
        3 => "merge",
        4 => "persistence",
        5 => "search",
        _ => "search",
    }
}

fn public_operation_resources(
    record: &OperationRecord,
    metadata: &OperationMetadata,
) -> (u64, u64) {
    if record.state.is_terminal() {
        // Terminal journal resources are the durable public snapshot. The live
        // sampler may observe a later process-wide peak that cannot survive a
        // restart and therefore must not change an already terminal response.
        return (record.peak_rss_bytes, record.written_bytes);
    }
    (
        record
            .peak_rss_bytes
            .max(metadata.peak_rss_bytes.load(Ordering::Relaxed)),
        record.written_bytes.max(metadata.written_bytes),
    )
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
    let languages = parse_code_locate_languages(request.languages)?;
    let response = service
        .code_locate_with_languages_and_budget(
            generation.generation,
            request.query,
            mode,
            languages,
            usize::try_from(request.maximum_results).map_err(|_| invalid_argument())?,
            usize::try_from(request.page_offset).map_err(|_| invalid_argument())?,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        response.data.next_page_offset.is_some(),
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope,
    );
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
            service,
            generation,
            &response.usage,
            &response.data.coverage,
        )?),
        hits,
        matched_candidates: response.data.matched_candidates,
        truncated: response.data.truncated,
        next_page_offset: response.data.next_page_offset,
        completeness: Some(completeness),
    })
}

fn parse_code_locate_languages(languages: Vec<String>) -> Result<Vec<String>, PublicError> {
    let valid = languages.len() <= MAX_CODE_LOCATE_LANGUAGES
        && languages.windows(2).all(|pair| pair[0] < pair[1])
        && languages.iter().all(|language| {
            !language.is_empty()
                && language.len() <= MAX_CODE_LOCATE_LANGUAGE_BYTES
                && language.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-' | b'.' | b'+' | b'#')
                })
        });
    if !valid {
        return Err(invalid_argument());
    }
    Ok(languages)
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
    let mut unresolved_symbols = Vec::new();
    unresolved_symbols
        .try_reserve_exact(request.symbols.len())
        .map_err(|_| resource_exhausted())?;
    let mut usage = UsageAccumulator::default();
    let mut coverage = Vec::new();
    let mut limiting_resources = Vec::new();
    let mut execution_state = ExecutionCompletenessState::Complete;
    for symbol in request.symbols {
        let budget = match remaining_service_budget(context, &usage) {
            Ok(budget) => budget,
            Err(BudgetExhaustion::Resource(resource)) => {
                if !limiting_resources.contains(&resource) {
                    limiting_resources.push(resource);
                }
                execution_state = execution_state.max(ExecutionCompletenessState::Truncated);
                break;
            }
            Err(BudgetExhaustion::Duration) => return Err(budget_exceeded()),
        };
        let symbol = parse_symbol(Some(&symbol))?;
        let response = match service.symbol_explain_with_budget(
            generation.generation,
            symbol,
            budget,
            &context.cancellation,
        ) {
            Ok(response) => response,
            Err(FirstSliceError::SymbolNotFound) => {
                unresolved_symbols.push(symbol_to_wire(symbol));
                continue;
            }
            Err(error) => return Err(service_error(error)),
        };
        for resource in &response.data.limiting_resources {
            if !limiting_resources.contains(resource) {
                limiting_resources.push(*resource);
            }
        }
        execution_state = execution_state.max(response.data.execution.state());
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
        let (language, tier) = service
            .source_language_coverage(generation.generation, definition.span().file())
            .map_err(service_error)?;
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
            language,
            tier: analysis_tier_to_wire(tier) as i32,
        });
    }
    Ok(daemon::SymbolExplainResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(
            service,
            generation,
            &usage.finish(),
            &coverage,
        )?),
        symbols,
        unresolved_symbols,
        truncated: !limiting_resources.is_empty(),
        completeness: Some(execution_completeness(
            execution_state,
            &limiting_resources,
            false,
            daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceSplitRequest,
        )),
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
        .symbol_relationships_with_budget(
            generation.generation,
            seeds,
            families,
            direction,
            min_confidence,
            max_results,
            usize::try_from(request.page_offset).map_err(|_| invalid_argument())?,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        response.data.next_page_offset.is_some(),
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceReduceRelations,
    );
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
        context: Some(query_context(service, generation, &response.usage, &[])?),
        groups,
        returned_edges: u64::from(response.data.returned_edges),
        total_edges: u64::from(response.data.total_edges),
        exact: response.data.exact,
        truncated: response.data.truncated,
        next_page_offset: response.data.next_page_offset,
        completeness: Some(completeness),
    })
}

fn flow_trace(
    service: &FirstSliceService,
    request: daemon::FlowTraceRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::FlowTraceResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let from = parse_symbol(request.from.as_ref())?;
    let to = match request.to.as_ref() {
        Some(to) => Some(parse_symbol(Some(to))?),
        None => None,
    };
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
    let max_depth = reduce_optional_u8(
        u8::try_from(request.max_depth.unwrap_or(DEFAULT_FLOW_DEPTH))
            .map_err(|_| invalid_argument())?,
        context.effective_budget.and_then(|budget| budget.depth()),
    )?;
    let max_paths = reduce_optional_usize(
        usize::try_from(request.max_paths.unwrap_or(DEFAULT_FLOW_PATHS))
            .map_err(|_| invalid_argument())?,
        context.effective_budget.and_then(|budget| budget.paths()),
    )?;
    if request.cross_repository {
        let target = to.ok_or_else(invalid_argument)?;
        let direction = direction.unwrap_or(RelationDirection::Outbound);
        let link = service
            .cross_repository_flow_link(
                generation.generation,
                from,
                target,
                &families,
                direction,
                min_confidence,
                &context.cancellation,
            )
            .map_err(service_error)?;
        let response = service
            .flow_trace_with_budget(
                generation.generation,
                from,
                None,
                families.clone(),
                Some(direction),
                min_confidence,
                max_depth,
                max_paths,
                service_budget(context),
                &context.cancellation,
            )
            .map_err(service_error)?;
        let completeness = execution_completeness(
            response.data.execution.state(),
            &response.data.limiting_resources,
            false,
            daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceReduceDepth,
        );
        let (paths, reached_nodes, examined_edges, unresolved_boundaries) = match link {
            Some(link) => (
                vec![daemon::FirstSliceTracePath {
                    confidence: u32::from(link.confidence),
                    nodes: vec![symbol_to_wire(from), symbol_to_wire(target)],
                    edges: vec![daemon::FirstSliceTraceEdge {
                        kind: link.family.as_str().to_owned(),
                        confidence: u32::from(link.confidence),
                        source_refs: link.source_refs.iter().map(source_ref_to_wire).collect(),
                    }],
                    cyclic: false,
                }],
                2,
                1,
                0,
            ),
            None => (Vec::new(), 1, 0, 1),
        };
        return Ok(daemon::FlowTraceResponse {
            schema_version: Some(schema_version()),
            context: Some(query_context(service, generation, &response.usage, &[])?),
            paths,
            frontier: Some(daemon::FirstSliceTraceFrontier {
                reached_nodes,
                examined_edges,
                truncated: false,
                unresolved_boundaries,
            }),
            projection: Some(daemon::FirstSliceTraceProjection {
                relations: families
                    .iter()
                    .map(|family| family.as_str().to_owned())
                    .collect(),
                min_confidence: u32::from(min_confidence),
            }),
            completeness: Some(completeness),
        });
    }
    let response = service
        .flow_trace_with_budget(
            generation.generation,
            from,
            to,
            families,
            direction,
            min_confidence,
            max_depth,
            max_paths,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceReduceDepth,
    );
    let mut paths = Vec::new();
    paths
        .try_reserve_exact(response.data.paths.len())
        .map_err(|_| resource_exhausted())?;
    for path in response.data.paths {
        let mut edges = Vec::new();
        edges
            .try_reserve_exact(path.edges.len())
            .map_err(|_| resource_exhausted())?;
        for edge in path.edges {
            edges.push(daemon::FirstSliceTraceEdge {
                kind: edge.family.as_str().to_owned(),
                confidence: u32::from(edge.confidence),
                source_refs: edge.source_refs.iter().map(source_ref_to_wire).collect(),
            });
        }
        paths.push(daemon::FirstSliceTracePath {
            confidence: u32::from(path.confidence),
            nodes: path.nodes.iter().copied().map(symbol_to_wire).collect(),
            edges,
            cyclic: path.cyclic,
        });
    }
    let frontier = response.data.frontier;
    let projection = response.data.projection;
    Ok(daemon::FlowTraceResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        paths,
        frontier: Some(daemon::FirstSliceTraceFrontier {
            reached_nodes: frontier.reached_nodes,
            examined_edges: frontier.examined_edges,
            truncated: frontier.truncated,
            unresolved_boundaries: frontier.unresolved_boundaries,
        }),
        projection: Some(daemon::FirstSliceTraceProjection {
            relations: projection
                .families
                .iter()
                .map(|family| family.as_str().to_owned())
                .collect(),
            min_confidence: u32::from(projection.min_confidence),
        }),
        completeness: Some(completeness),
    })
}

fn architecture_cycles(
    service: &FirstSliceService,
    request: daemon::ArchitectureCyclesRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::ArchitectureCyclesResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
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
    #[cfg(feature = "process-test-hooks")]
    await_process_cancellation(context)?;
    let min_size = u8::try_from(request.min_size.unwrap_or(DEFAULT_CYCLE_MIN_SIZE))
        .map_err(|_| invalid_argument())?;
    let max_cycles = usize::try_from(request.max_cycles.unwrap_or(DEFAULT_CYCLE_MAX_CYCLES))
        .map_err(|_| invalid_argument())?;
    let include_self_cycles = request.include_self_cycles.unwrap_or(false);
    let response = service
        .architecture_cycles_with_budget(
            generation.generation,
            families,
            min_size,
            max_cycles,
            include_self_cycles,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceReduceRelations,
    );
    let mut components = Vec::new();
    components
        .try_reserve_exact(response.data.components.len())
        .map_err(|_| resource_exhausted())?;
    for component in response.data.components {
        components.push(daemon::FirstSliceCycleComponent {
            size: component.size,
            members: component
                .members
                .iter()
                .copied()
                .map(symbol_to_wire)
                .collect(),
            internal_edges: component.internal_edges,
        });
    }
    let mut cycles = Vec::new();
    cycles
        .try_reserve_exact(response.data.cycles.len())
        .map_err(|_| resource_exhausted())?;
    for cycle in response.data.cycles {
        cycles.push(daemon::FirstSliceCycle {
            nodes: cycle.nodes.iter().copied().map(symbol_to_wire).collect(),
            edge_evidence: cycle.edge_evidence.iter().map(source_ref_to_wire).collect(),
            confidence: u32::from(cycle.confidence),
        });
    }
    let mut break_candidates = Vec::new();
    break_candidates
        .try_reserve_exact(response.data.break_candidates.len())
        .map_err(|_| resource_exhausted())?;
    for candidate in response.data.break_candidates {
        break_candidates.push(daemon::FirstSliceCycleBreak {
            from: Some(symbol_to_wire(candidate.from)),
            to: Some(symbol_to_wire(candidate.to)),
            kind: candidate.family.as_str().to_owned(),
            break_cost: u32::from(candidate.break_cost),
            source_refs: candidate
                .source_refs
                .iter()
                .map(source_ref_to_wire)
                .collect(),
        });
    }
    let projection = response.data.projection;
    Ok(daemon::ArchitectureCyclesResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        components,
        cycles,
        break_candidates,
        projection: Some(daemon::FirstSliceCycleProjection {
            relations: projection
                .families
                .iter()
                .map(|family| family.as_str().to_owned())
                .collect(),
            min_confidence: u32::from(projection.min_confidence),
        }),
        completeness: Some(completeness),
    })
}

#[cfg(feature = "process-test-hooks")]
fn await_process_cancellation(context: &FirstSliceIpcContext) -> Result<(), PublicError> {
    const HOOK_ENDPOINT_ENV: &str = "ROOTLIGHT_PROCESS_TEST_CANCELLATION_ENDPOINT";
    const HOOK_TIMEOUT: Duration = Duration::from_secs(10);

    let Some(endpoint) = std::env::var_os(HOOK_ENDPOINT_ENV) else {
        return Ok(());
    };
    let endpoint = rootlight_ipc::Endpoint::new(endpoint.into()).map_err(|_| internal_error())?;
    let mut stream = rootlight_ipc::connect(&endpoint).map_err(|_| internal_error())?;
    write_process_hook_signal(&mut stream, b'E', HOOK_TIMEOUT)?;

    let deadline = Instant::now()
        .checked_add(HOOK_TIMEOUT)
        .ok_or_else(internal_error)?;
    loop {
        match context.cancellation.reason() {
            Some(rootlight_operations::CancellationReason::ClientRequest) => {
                write_process_hook_signal(&mut stream, b'C', HOOK_TIMEOUT)?;
                return Err(cancelled_error());
            }
            Some(reason) => return Err(cancellation_error(reason)),
            None if Instant::now() >= deadline => return Err(internal_error()),
            // This feature is test-only. Yielding keeps the production
            // cancellation type unchanged while the transport task records
            // the peer-disconnect event on the shared token.
            None => thread::yield_now(),
        }
    }
}

#[cfg(feature = "process-test-hooks")]
fn write_process_hook_signal(
    stream: &mut rootlight_ipc::LocalStream,
    signal: u8,
    timeout: Duration,
) -> Result<(), PublicError> {
    use std::io::Write as _;

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(internal_error)?;
    let bytes = [signal];
    let mut written = 0;
    while written < bytes.len() {
        match stream.write(&bytes[written..]) {
            Ok(0) => return Err(internal_error()),
            Ok(count) => written += count,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) && Instant::now() < deadline =>
            {
                thread::yield_now();
            }
            Err(_) => return Err(internal_error()),
        }
    }
    Ok(())
}

fn code_dead(
    service: &FirstSliceService,
    request: daemon::CodeDeadRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::CodeDeadResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let entry_point_policy = match request.entry_point_policy.as_deref() {
        Some(label) => CodeDeadEntryPointPolicy::from_label(label).ok_or_else(invalid_argument)?,
        None => CodeDeadEntryPointPolicy::Standard,
    };
    let include_exported = request.include_exported.unwrap_or(false);
    let include_tests = request.include_tests.unwrap_or(false);
    let min_confidence =
        u16::try_from(request.min_confidence.unwrap_or(0)).map_err(|_| invalid_argument())?;
    let max_candidates = usize::try_from(
        request
            .max_candidates
            .unwrap_or(DEFAULT_CODE_DEAD_MAX_CANDIDATES),
    )
    .map_err(|_| invalid_argument())?;
    let response = service
        .code_dead_with_budget(
            generation.generation,
            entry_point_policy,
            include_exported,
            include_tests,
            min_confidence,
            max_candidates,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceRefreshCoverage,
    );
    let mut candidates = Vec::new();
    candidates
        .try_reserve_exact(response.data.candidates.len())
        .map_err(|_| resource_exhausted())?;
    for candidate in response.data.candidates {
        candidates.push(daemon::FirstSliceDeadCandidate {
            symbol_id: Some(symbol_to_wire(candidate.symbol_id)),
            classification: candidate.classification.as_str().to_owned(),
            confidence: u32::from(candidate.confidence),
            why: candidate.why,
            suppressions_checked: candidate.suppressions_checked,
            source_refs: candidate
                .source_refs
                .iter()
                .map(source_ref_to_wire)
                .collect(),
        });
    }
    let mut blind_spots = Vec::new();
    blind_spots
        .try_reserve_exact(response.data.blind_spots.len())
        .map_err(|_| resource_exhausted())?;
    for spot in response.data.blind_spots {
        blind_spots.push(daemon::FirstSliceBlindSpot {
            category: spot.category,
            affected_count: spot.affected_count,
        });
    }
    let mut false_positive_controls = Vec::new();
    false_positive_controls
        .try_reserve_exact(response.data.suppression_rules.len())
        .map_err(|_| resource_exhausted())?;
    for rule in response.data.suppression_rules {
        false_positive_controls.push(daemon::FirstSliceSuppressionRule {
            rule: rule.rule,
            suppressed_count: rule.suppressed_count,
        });
    }
    let entry_points = response.data.entry_points;
    Ok(daemon::CodeDeadResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        candidates,
        entry_points: Some(daemon::FirstSliceEntryPointSummary {
            policy: entry_points.policy.as_str().to_owned(),
            entry_point_count: entry_points.entry_point_count,
            complete: entry_points.complete,
        }),
        blind_spots,
        false_positive_controls,
        completeness: Some(completeness),
    })
}

fn architecture_overview(
    service: &FirstSliceService,
    request: daemon::ArchitectureOverviewRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::ArchitectureOverviewResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mut views = Vec::new();
    views
        .try_reserve_exact(request.views.len())
        .map_err(|_| resource_exhausted())?;
    for label in &request.views {
        let view = ArchitectureOverviewView::from_label(label).ok_or_else(invalid_argument)?;
        views.push(view);
    }
    let include_edges = request.include_edges.unwrap_or(true);
    let min_confidence =
        u16::try_from(request.min_confidence.unwrap_or(0)).map_err(|_| invalid_argument())?;
    let max_components = usize::try_from(
        request
            .max_components
            .unwrap_or(DEFAULT_ARCHITECTURE_OVERVIEW_MAX_COMPONENTS),
    )
    .map_err(|_| invalid_argument())?;
    let response = service
        .architecture_overview_with_budget(
            generation.generation,
            views,
            min_confidence,
            max_components,
            include_edges,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope,
    );
    let mut components = Vec::new();
    components
        .try_reserve_exact(response.data.components.len())
        .map_err(|_| resource_exhausted())?;
    for component in response.data.components {
        components.push(daemon::FirstSliceArchitectureComponent {
            id: component.id,
            kind: component.kind,
            name: component.name,
            symbol_count: component.symbol_count,
            responsibility_evidence: component.responsibility_evidence,
            confidence: u32::from(component.confidence),
        });
    }
    let mut connections = Vec::new();
    connections
        .try_reserve_exact(response.data.connections.len())
        .map_err(|_| resource_exhausted())?;
    for connection in response.data.connections {
        connections.push(daemon::FirstSliceArchitectureConnection {
            from: connection.from,
            to: connection.to,
            kind: connection.kind.as_str().to_owned(),
            weight: connection.weight,
            confidence: u32::from(connection.confidence),
        });
    }
    let mut hotspots = Vec::new();
    hotspots
        .try_reserve_exact(response.data.hotspots.len())
        .map_err(|_| resource_exhausted())?;
    for hotspot in response.data.hotspots {
        hotspots.push(daemon::FirstSliceHotspot {
            component_id: hotspot.component_id,
            fan_in: hotspot.fan_in,
            fan_out: hotspot.fan_out,
            change_frequency: hotspot.change_frequency,
            complexity: hotspot.complexity,
            score: u32::from(hotspot.score),
        });
    }
    let mut communities = Vec::new();
    communities
        .try_reserve_exact(response.data.communities.len())
        .map_err(|_| resource_exhausted())?;
    for community in response.data.communities {
        communities.push(daemon::FirstSliceArchitectureCommunity {
            id: community.id,
            members: community.members,
            internal_connection_weight: community.internal_connection_weight,
            ownership_truth: community.ownership_truth,
        });
    }
    let mut wire_views = Vec::new();
    wire_views
        .try_reserve_exact(response.data.views.len())
        .map_err(|_| resource_exhausted())?;
    for view in response.data.views {
        wire_views.push(daemon::FirstSliceDerivedView {
            view: view.view.as_str().to_owned(),
            algorithm_version: view.algorithm_version,
            parameters: view.parameters.into_iter().collect(),
        });
    }
    Ok(daemon::ArchitectureOverviewResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        components,
        connections,
        hotspots,
        views: wire_views,
        completeness: Some(completeness),
        communities,
    })
}

fn tests_select(
    service: &FirstSliceService,
    request: daemon::TestsSelectRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::TestsSelectResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mut seeds = BTreeSet::new();
    for seed in &request.seeds {
        seeds.insert(parse_symbol(Some(seed))?);
    }
    let mut test_kinds = Vec::new();
    test_kinds
        .try_reserve_exact(request.test_kinds.len())
        .map_err(|_| resource_exhausted())?;
    for label in &request.test_kinds {
        let kind = TestsSelectKind::from_label(label).ok_or_else(invalid_argument)?;
        if !test_kinds.contains(&kind) {
            test_kinds.push(kind);
        }
    }
    let max_tests = usize::try_from(request.max_tests.unwrap_or(DEFAULT_TESTS_SELECT_MAX_TESTS))
        .map_err(|_| invalid_argument())?;
    let include_commands = request.include_commands.unwrap_or(false);
    let response = service
        .tests_select_with_budget(
            generation.generation,
            seeds,
            test_kinds,
            max_tests,
            include_commands,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceSplitRequest,
    );
    let strategy = response.data.coverage_strategy;
    let mut tests = Vec::new();
    tests
        .try_reserve_exact(response.data.tests.len())
        .map_err(|_| resource_exhausted())?;
    for test in response.data.tests {
        tests.push(daemon::FirstSliceRankedTest {
            test_id: test.test_id.to_string(),
            kind: test.kind.as_str().to_owned(),
            path: test.path,
            score: u32::from(test.score),
            why: test.why,
            estimated_cost_ms: test.estimated_cost_ms,
            command_hint: test.command_hint,
        });
    }
    let mut gaps = Vec::new();
    gaps.try_reserve_exact(response.data.gaps.len())
        .map_err(|_| resource_exhausted())?;
    for gap in response.data.gaps {
        gaps.push(daemon::FirstSliceTestGap {
            scope: gap.scope,
            reason: gap.reason,
        });
    }
    Ok(daemon::TestsSelectResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        tests,
        coverage_strategy: Some(daemon::FirstSliceTestCoverageStrategy {
            direct_edges: strategy.direct_edges,
            transitive_signals: strategy.transitive_signals,
            history_signals: strategy.history_signals,
            build_target_signals: false,
            file_colocation_signals: strategy.file_colocation_signals,
        }),
        gaps,
        completeness: Some(completeness),
    })
}

fn change_impact(
    service: &FirstSliceService,
    request: daemon::ChangeImpactRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::ChangeImpactResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let mut changed_symbols = BTreeSet::new();
    for symbol in &request.changed_symbols {
        changed_symbols.insert(parse_symbol(Some(symbol))?);
    }
    let mut changed_paths = Vec::new();
    changed_paths
        .try_reserve_exact(request.changed_paths.len())
        .map_err(|_| resource_exhausted())?;
    for path in &request.changed_paths {
        changed_paths.push(path.clone());
    }
    let max_depth = reduce_optional_u8(
        u8::try_from(request.max_depth.unwrap_or(DEFAULT_CHANGE_IMPACT_MAX_DEPTH))
            .map_err(|_| invalid_argument())?,
        context.effective_budget.and_then(|budget| budget.depth()),
    )?;
    let min_confidence =
        u16::try_from(request.min_confidence.unwrap_or(0)).map_err(|_| invalid_argument())?;
    let include_tests = request.include_tests.unwrap_or(false);
    let max_dependents = usize::try_from(
        request
            .max_dependents
            .unwrap_or(DEFAULT_CHANGE_IMPACT_MAX_DEPENDENTS),
    )
    .map_err(|_| invalid_argument())?;
    let response = service
        .change_impact_with_budget(
            generation.generation,
            changed_symbols,
            changed_paths,
            max_depth,
            min_confidence,
            include_tests,
            max_dependents,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceReduceDepth,
    );
    let risk = response.data.risk_summary;
    let mut resolved_changes = Vec::new();
    resolved_changes
        .try_reserve_exact(response.data.resolved_changes.len())
        .map_err(|_| resource_exhausted())?;
    for change in response.data.resolved_changes {
        resolved_changes.push(daemon::FirstSliceResolvedChange {
            symbol_id: change.symbol_id.map(symbol_to_wire),
            file_id: change.file_id.map(file_to_wire),
            classification: change.classification.as_str().to_owned(),
            kind: change.kind,
        });
    }
    let mut impacted = Vec::new();
    impacted
        .try_reserve_exact(response.data.impacted.len())
        .map_err(|_| resource_exhausted())?;
    for group in response.data.impacted {
        let mut dependents = Vec::new();
        dependents
            .try_reserve_exact(group.dependents.len())
            .map_err(|_| resource_exhausted())?;
        for entry in group.dependents {
            dependents.push(daemon::FirstSliceImpactEntry {
                symbol_id: Some(symbol_to_wire(entry.symbol_id)),
                kind: entry.kind,
                distance: u32::from(entry.distance),
                confidence: u32::from(entry.confidence),
                via: entry.via,
                is_public: entry.is_public,
            });
        }
        impacted.push(daemon::FirstSliceImpactGroup {
            source_index: u32::from(group.source_index),
            dependents,
        });
    }
    let mut tests = Vec::new();
    tests
        .try_reserve_exact(response.data.tests.len())
        .map_err(|_| resource_exhausted())?;
    for test in response.data.tests {
        tests.push(daemon::FirstSliceChangeImpactTest {
            test_id: test.test_id,
            relevance: u32::from(test.relevance),
            why: test.why,
            estimated_cost_ms: test.estimated_cost_ms,
        });
    }
    Ok(daemon::ChangeImpactResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        resolved_changes,
        impacted,
        tests,
        risk_summary: Some(daemon::FirstSliceImpactRiskSummary {
            level: risk.level.as_str().to_owned(),
            reasons: risk.reasons,
            coverage: coverage_label(risk.coverage).to_owned(),
            breaking_surface: risk.breaking_surface,
            fanout: risk.fanout,
            dynamic_blind_spots: risk.dynamic_blind_spots,
        }),
        completeness: Some(completeness),
    })
}

fn plan_change(
    service: &FirstSliceService,
    request: daemon::PlanChangeRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::PlanChangeResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    let objective =
        PlanChangeObjective::from_label(&request.objective).ok_or_else(invalid_argument)?;
    let mut target_symbols = BTreeSet::new();
    for symbol in &request.target_symbols {
        target_symbols.insert(parse_symbol(Some(symbol))?);
    }
    let mut target_files = BTreeSet::new();
    for file in &request.target_files {
        target_files.insert(parse_file(Some(file))?);
    }
    let max_steps = usize::try_from(request.max_steps.unwrap_or(DEFAULT_PLAN_CHANGE_MAX_STEPS))
        .map_err(|_| invalid_argument())?;
    let response = service
        .plan_change_with_budget(
            generation.generation,
            objective,
            request.objective_text,
            target_symbols,
            target_files,
            max_steps,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope,
    );
    let affected_scope = response.data.affected_scope;
    let context_pack = response.data.context_pack_request;
    let mut plan = Vec::new();
    plan.try_reserve_exact(response.data.plan.len())
        .map_err(|_| resource_exhausted())?;
    for step in response.data.plan {
        plan.push(daemon::FirstSliceChangePlanStep {
            step: u32::from(step.step),
            action: step.action,
            targets: step.targets.into_iter().map(symbol_to_wire).collect(),
            depends_on: step.depends_on.into_iter().map(u32::from).collect(),
            risks: step.risks,
            verification: step.verification,
        });
    }
    let mut test_plan = Vec::new();
    test_plan
        .try_reserve_exact(response.data.test_plan.len())
        .map_err(|_| resource_exhausted())?;
    for test in response.data.test_plan {
        test_plan.push(daemon::FirstSliceChangeImpactTest {
            test_id: test.test_id,
            relevance: u32::from(test.relevance),
            why: test.why,
            estimated_cost_ms: test.estimated_cost_ms,
        });
    }
    let mut open_decisions = Vec::new();
    open_decisions
        .try_reserve_exact(response.data.open_decisions.len())
        .map_err(|_| resource_exhausted())?;
    for decision in response.data.open_decisions {
        open_decisions.push(daemon::FirstSlicePlanDecision {
            question: decision.question,
            recommended_default: decision.recommended_default,
        });
    }
    Ok(daemon::PlanChangeResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        plan,
        affected_scope: Some(daemon::FirstSlicePlanImpactSummary {
            affected_symbols: affected_scope.affected_symbols,
            affected_files: affected_scope.affected_files,
            risk_level: affected_scope.risk_level.as_str().to_owned(),
            touches_public_surface: affected_scope.touches_public_surface,
        }),
        test_plan,
        open_decisions,
        context_pack_request: Some(daemon::FirstSliceContextPackRequest {
            symbols: context_pack
                .symbols
                .into_iter()
                .map(symbol_to_wire)
                .collect(),
            files: context_pack.files.into_iter().map(file_to_wire).collect(),
        }),
        completeness: Some(completeness),
    })
}

fn history_compare(
    service: &FirstSliceService,
    request: daemon::HistoryCompareRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::HistoryCompareResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let base = parse_revision_generation(request.base.as_ref())?;
    let head = parse_revision_generation(request.head.as_ref())?;
    // Resolve both generations against the repository: the head context drives
    // the query context while the base must also belong to the repository.
    let head_context = service
        .resolve_generation(repository, Some(head))
        .map_err(service_error)?;
    service
        .resolve_generation(repository, Some(base))
        .map_err(service_error)?;
    let mut change_kinds = BTreeSet::new();
    for kind in &request.change_kinds {
        change_kinds.insert(HistoryChangeKind::from_label(kind).ok_or_else(invalid_argument)?);
    }
    let max_results = usize::try_from(
        request
            .max_results
            .unwrap_or(DEFAULT_HISTORY_COMPARE_MAX_RESULTS),
    )
    .map_err(|_| invalid_argument())?;
    let response = service
        .history_compare_with_budget(
            base,
            head,
            change_kinds,
            max_results,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let completeness = execution_completeness(
        response.data.execution.state(),
        &response.data.limiting_resources,
        false,
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope,
    );
    let data = response.data;
    let architecture_delta = data.architecture_delta;
    let mut changes = Vec::new();
    changes
        .try_reserve_exact(data.changes.len())
        .map_err(|_| resource_exhausted())?;
    for change in data.changes {
        changes.push(daemon::FirstSliceSemanticChange {
            kind: change.kind.as_str().to_owned(),
            symbol_id: Some(symbol_to_wire(change.symbol_id)),
            entity_kind: change.entity_kind,
            breaking_candidate: change.breaking_candidate,
            significance: u32::from(change.significance),
        });
    }
    let mut breaking_candidates = Vec::new();
    breaking_candidates
        .try_reserve_exact(data.breaking_candidates.len())
        .map_err(|_| resource_exhausted())?;
    for candidate in data.breaking_candidates {
        breaking_candidates.push(daemon::FirstSliceBreakingCandidate {
            symbol_id: Some(symbol_to_wire(candidate.symbol_id)),
            consumer_count: candidate.consumer_count,
            is_public_surface: candidate.is_public_surface,
            reason: candidate.reason,
        });
    }
    let mut lineage = Vec::new();
    lineage
        .try_reserve_exact(data.lineage.len())
        .map_err(|_| resource_exhausted())?;
    for lineage_match in data.lineage {
        lineage.push(daemon::FirstSliceLineageMatch {
            base_symbol_id: Some(symbol_to_wire(lineage_match.base_symbol_id)),
            head_symbol_id: Some(symbol_to_wire(lineage_match.head_symbol_id)),
            confidence: u32::from(lineage_match.confidence),
            is_rename: lineage_match.is_rename,
        });
    }
    Ok(daemon::HistoryCompareResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, head_context, &response.usage, &[])?),
        matched_states: Some(daemon::FirstSliceMatchedStates {
            base_generation: Some(generation_to_wire(data.base_generation)),
            head_generation: Some(generation_to_wire(data.head_generation)),
            coverage: coverage_label(data.coverage).to_owned(),
        }),
        changes,
        architecture_delta: Some(daemon::FirstSliceArchitectureDelta {
            new_cross_service_edges: architecture_delta.new_cross_service_edges,
            removed_cross_service_edges: architecture_delta.removed_cross_service_edges,
            new_boundaries: architecture_delta.new_boundaries,
            removed_boundaries: architecture_delta.removed_boundaries,
        }),
        breaking_candidates,
        lineage,
        completeness: Some(completeness),
    })
}

fn advanced_query(
    service: &FirstSliceService,
    request: daemon::AdvancedQueryRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::AdvancedQueryResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let generation = service
        .resolve_generation(repository, selected)
        .map_err(service_error)?;
    // The wire carries the safe typed AST as JSON; it crosses the boundary
    // unchanged because the query-layer AST is wire-compatible with the public
    // contract AST.
    let ast: AdvancedAstNode =
        serde_json::from_str(&request.query_ast).map_err(|_| invalid_argument())?;
    let explain = request.explain.unwrap_or(false);
    let max_results = match request.max_results {
        Some(results) => usize::try_from(results).map_err(|_| invalid_argument())?,
        None => ADVANCED_DEFAULT_MAX_RESULTS,
    };
    let max_depth = match request.max_depth {
        Some(depth) => usize::try_from(depth).map_err(|_| invalid_argument())?,
        None => ADVANCED_DEFAULT_MAX_DEPTH,
    };
    let max_depth = reduce_optional_usize(
        max_depth,
        context.effective_budget.and_then(|budget| budget.depth()),
    )?;
    let budget = service_budget(context);
    let max_traversal = advanced_edge_work_limit(budget)?;
    #[cfg(feature = "process-test-hooks")]
    await_process_cancellation(context)?;
    let response = service
        .advanced_query_with_budget(
            generation.generation,
            ast,
            explain,
            max_results,
            usize::try_from(request.page_offset).map_err(|_| invalid_argument())?,
            max_depth,
            max_traversal,
            request.cost_limit,
            budget,
            &context.cancellation,
        )
        .map_err(service_error)?;
    let data = response.data;
    let result_completeness = execution_completeness(
        data.execution.state(),
        &data.limiting_resources,
        data.next_page_offset.is_some(),
        daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceNarrowScope,
    );
    let mut columns = Vec::new();
    columns
        .try_reserve_exact(data.columns.len())
        .map_err(|_| resource_exhausted())?;
    for column in data.columns {
        columns.push(daemon::FirstSliceAdvancedColumn {
            name: column.name,
            column_type: column.column_type.as_str().to_owned(),
        });
    }
    let mut rows = Vec::new();
    rows.try_reserve_exact(data.rows.len())
        .map_err(|_| resource_exhausted())?;
    for row in data.rows {
        rows.push(serde_json::to_string(&row).map_err(|_| resource_exhausted())?);
    }
    let plan = data.plan.map(|plan| daemon::FirstSliceAdvancedPlan {
        estimated_cost: plan.estimated_cost,
        operators: plan.operators,
        applied_limits: plan.applied_limits,
    });
    let completeness = data.completeness.as_str().to_owned();
    Ok(daemon::AdvancedQueryResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        columns,
        rows,
        plan,
        completeness,
        next_page_offset: data.next_page_offset,
        result_completeness: Some(result_completeness),
    })
}

fn advanced_edge_work_limit(budget: FirstSliceBudget) -> Result<usize, PublicError> {
    reduce_optional_usize(ADVANCED_MAX_TRAVERSAL, Some(budget.query().max_edges()))
}

/// Parses a history-compare revision selector into an explicit generation.
///
/// Git revision selectors are rejected because the first-slice daemon maps no
/// git ref to a retained generation.
fn parse_revision_generation(
    selector: Option<&daemon::FirstSliceRevisionSelector>,
) -> Result<GenerationId, PublicError> {
    match selector.and_then(|selector| selector.selector.as_ref()) {
        Some(daemon::first_slice_revision_selector::Selector::Generation(generation)) => {
            parse_generation(Some(generation))
        }
        Some(daemon::first_slice_revision_selector::Selector::Git(_)) => {
            Err(unsupported_capability())
        }
        None => Err(invalid_argument()),
    }
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
    let encoding = match daemon::SourceReadEncoding::try_from(request.encoding) {
        Ok(daemon::SourceReadEncoding::Utf8) => ServiceSourceEncoding::Utf8,
        Ok(daemon::SourceReadEncoding::Bytes) => ServiceSourceEncoding::Bytes,
        Err(_) => return Err(invalid_argument()),
    };
    let context_lines_before =
        u16::try_from(request.context_lines_before).map_err(|_| invalid_argument())?;
    let context_lines_after =
        u16::try_from(request.context_lines_after).map_err(|_| invalid_argument())?;
    let options = SourceReadOptions::new()
        .with_context_lines_before(context_lines_before)
        .with_context_lines_after(context_lines_after)
        .with_encoding(encoding);
    let include_line_numbers = request.include_line_numbers.unwrap_or(true);
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
    if request.merge_overlaps {
        references = merge_source_references(references)?;
    }
    let response = service
        .source_read_with_options_and_budget(
            generation.generation,
            references,
            options,
            service_budget(context),
            &context.cancellation,
        )
        .map_err(service_error)?;
    let data = response.data;
    let execution = data.execution;
    let mut chunks = Vec::new();
    chunks
        .try_reserve_exact(data.chunks.len())
        .map_err(|_| resource_exhausted())?;
    for chunk in data.chunks {
        let (language, tier) = service
            .source_language_coverage(generation.generation, chunk.reference.span().file())
            .map_err(service_error)?;
        if language != chunk.language {
            return Err(internal_error());
        }
        let included_start_line = include_line_numbers.then_some(chunk.start_line).flatten();
        let included_end_line = include_line_numbers.then_some(chunk.end_line).flatten();
        let (encoding, legacy_content) = match chunk.encoding {
            rootlight_query::SourceChunkEncoding::Utf8 => (
                daemon::SourceReadEncoding::Utf8 as i32,
                String::from_utf8(chunk.bytes.clone()).map_err(|_| internal_error())?,
            ),
            rootlight_query::SourceChunkEncoding::Bytes => {
                (daemon::SourceReadEncoding::Bytes as i32, String::new())
            }
        };
        chunks.push(daemon::FirstSliceSourceChunk {
            source: Some(source_ref_to_wire(&chunk.reference)),
            path: chunk.path,
            start_byte: chunk.start_byte,
            end_byte: chunk.end_byte,
            start_line: included_start_line.unwrap_or(0),
            end_line: included_end_line.unwrap_or(0),
            content: legacy_content,
            content_hash: Some(content_hash_to_wire(chunk.content_hash)),
            language,
            generated: chunk.generated,
            encoding,
            included_start_line,
            included_end_line,
            exact_content: chunk.bytes,
            tier: analysis_tier_to_wire(tier) as i32,
        });
    }
    Ok(daemon::SourceReadResponse {
        schema_version: Some(schema_version()),
        context: Some(query_context(service, generation, &response.usage, &[])?),
        chunks,
        total_source_bytes: response.usage.source_bytes,
        truncated: execution.is_truncated(),
        completeness: Some(execution_completeness(
            execution.state(),
            execution.limiting_resources(),
            false,
            daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceSplitRequest,
        )),
    })
}

fn merge_source_references(mut references: Vec<SourceRef>) -> Result<Vec<SourceRef>, PublicError> {
    references.sort_by(|left, right| {
        let left_span = left.span();
        let right_span = right.span();
        left.repository()
            .cmp(&right.repository())
            .then_with(|| left.generation().cmp(&right.generation()))
            .then_with(|| left_span.file().cmp(&right_span.file()))
            .then_with(|| left.content_hash().cmp(&right.content_hash()))
            .then_with(|| left_span.start_byte().cmp(&right_span.start_byte()))
            .then_with(|| left_span.end_byte().cmp(&right_span.end_byte()))
    });
    let mut merged = Vec::<SourceRef>::new();
    merged
        .try_reserve_exact(references.len())
        .map_err(|_| resource_exhausted())?;
    for reference in references {
        let span = reference.span();
        let merge_with_previous = merged.last().is_some_and(|existing| {
            let existing_span = existing.span();
            existing.repository() == reference.repository()
                && existing.generation() == reference.generation()
                && existing_span.file() == span.file()
                && existing.content_hash() == reference.content_hash()
                && span.start_byte() < existing_span.end_byte()
        });
        if merge_with_previous {
            let existing = merged.last_mut().ok_or_else(resource_exhausted)?;
            let existing_span = existing.span();
            let combined = SourceSpan::new(
                span.file(),
                existing_span.start_byte().min(span.start_byte()),
                existing_span.end_byte().max(span.end_byte()),
            )
            .map_err(|_| invalid_argument())?;
            *existing = SourceRef::new(
                reference.repository(),
                reference.generation(),
                combined,
                reference.content_hash(),
                None,
            );
        } else {
            merged.push(reference);
        }
    }
    Ok(merged)
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

fn repository_catalog_page(
    service: &FirstSliceService,
    request: daemon::RepositoryCatalogPageRequest,
    catalog_epoch: Instant,
) -> Result<daemon::RepositoryCatalogPageResponse, PublicError> {
    let sort_version = u16::try_from(request.sort_version).map_err(|_| invalid_cursor())?;
    if sort_version != CATALOG_SORT_VERSION {
        return Err(invalid_cursor());
    }
    if !request.states_present && !request.states.is_empty() {
        return Err(invalid_argument());
    }

    let states = request
        .states_present
        .then(|| {
            request
                .states
                .iter()
                .map(|state| catalog_state(state))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let filter = CatalogListFilter::new(request.normalized_query.as_deref(), states, None)
        .map_err(catalog_error)?;
    let page_size = u16::try_from(request.page_size)
        .map_err(|_| invalid_argument())
        .and_then(|page_size| CatalogPageSize::new(page_size).map_err(catalog_error))?;
    let snapshot_id = request
        .snapshot
        .map(|snapshot| {
            let bytes: [u8; 32] = snapshot.value.try_into().map_err(|_| invalid_cursor())?;
            Ok(CatalogSnapshotId::from_bytes(bytes))
        })
        .transpose()?;
    let after = request
        .after
        .map(|after| CatalogSortKey::from_bytes(sort_version, &after.value).map_err(catalog_error))
        .transpose()?;
    let request =
        CatalogPageRequest::new(snapshot_id, after, filter, page_size).map_err(catalog_error)?;
    let page = service
        .repository_catalog_page(request, catalog_now(catalog_epoch)?)
        .map_err(catalog_error)?;

    let snapshot = daemon::RepositoryCatalogSnapshotId {
        value: page.snapshot_id().as_bytes().to_vec(),
    };
    let next_after = page
        .next_after()
        .map(|key| daemon::RepositoryCatalogSortKey {
            value: key.to_bytes(),
        });
    let total_count = page.total_count();
    let sort_version = u32::from(page.sort_version());
    let truncated = next_after.is_some();
    let mut repositories = Vec::new();
    repositories
        .try_reserve_exact(page.items().len())
        .map_err(|_| resource_exhausted())?;
    for record in page.into_items() {
        repositories.push(catalog_record_to_wire(&record)?);
    }

    Ok(daemon::RepositoryCatalogPageResponse {
        repositories,
        snapshot: Some(snapshot),
        next_after,
        total_count: Some(total_count),
        truncated,
        sort_version,
    })
}

fn catalog_state(value: &str) -> Result<CatalogRepositoryState, PublicError> {
    match value {
        "ready" => Ok(CatalogRepositoryState::Ready),
        "indexing" => Ok(CatalogRepositoryState::Indexing),
        "degraded" => Ok(CatalogRepositoryState::Degraded),
        "corrupt" => Ok(CatalogRepositoryState::Corrupt),
        "migration_required" => Ok(CatalogRepositoryState::MigrationRequired),
        "rebuild_required" => Ok(CatalogRepositoryState::RebuildRequired),
        _ => Err(invalid_argument()),
    }
}

fn catalog_record_to_wire(
    record: &CatalogRepositoryRecord,
) -> Result<daemon::RepositoryCatalogEntry, PublicError> {
    let mut languages = Vec::new();
    languages
        .try_reserve_exact(record.coverage().len())
        .map_err(|_| resource_exhausted())?;
    let mut coverage = Vec::new();
    coverage
        .try_reserve_exact(record.coverage().len())
        .map_err(|_| resource_exhausted())?;
    for entry in record.coverage() {
        languages.push(entry.language().to_owned());
        coverage.push(daemon::RepositoryCoverageEntry {
            language: entry.language().to_owned(),
            tier: catalog_tier_label(entry.tier())?.to_owned(),
            status: catalog_coverage_label(entry.status())?.to_owned(),
            discovered_files: entry.discovered_files(),
            indexed_files: entry.indexed_files(),
        });
    }
    Ok(daemon::RepositoryCatalogEntry {
        repository: Some(repository_to_wire(record.repository())),
        active_generation: record.active_generation().map(generation_to_wire),
        display_name: record.display_name().to_owned(),
        alias: record.alias().map(str::to_owned),
        generation_count: record.generation_count(),
        state: record.state().as_str().to_owned(),
        languages,
        structural_freshness: record.structural_freshness().as_str().to_owned(),
        semantic_freshness: record.semantic_freshness().as_str().to_owned(),
        coverage,
    })
}

fn catalog_tier_label(tier: AnalysisTier) -> Result<&'static str, PublicError> {
    match tier {
        AnalysisTier::TierA => Ok("tier_a"),
        AnalysisTier::TierB => Ok("tier_b"),
        AnalysisTier::TierC => Ok("tier_c"),
        AnalysisTier::TierD => Ok("tier_d"),
        _ => Err(internal_error()),
    }
}

fn catalog_coverage_label(status: CoverageStatus) -> Result<&'static str, PublicError> {
    match status {
        CoverageStatus::Complete => Ok("complete"),
        CoverageStatus::Bounded => Ok("bounded"),
        CoverageStatus::Sampled => Ok("sampled"),
        CoverageStatus::Unknown => Ok("unknown"),
        _ => Err(internal_error()),
    }
}

fn catalog_now(epoch: Instant) -> Result<CatalogInstant, PublicError> {
    let elapsed = u64::try_from(epoch.elapsed().as_millis()).map_err(|_| internal_error())?;
    Ok(CatalogInstant::from_millis(elapsed))
}

fn repository_status(
    service: &FirstSliceService,
    journal: &JournalActorHandle,
    metadata: &Mutex<OperationMetadataSet>,
    runtime: &tokio::runtime::Runtime,
    request: daemon::RepositoryStatusRequest,
    context: &FirstSliceIpcContext,
) -> Result<daemon::RepositoryStatusResponse, PublicError> {
    let repository = parse_repository(request.repository.as_ref())?;
    let selected = parse_generation_selector(request.generation.as_ref())?;
    let coverage_detail = match request.coverage_detail.as_str() {
        "" | "summary" => "summary",
        "language" => "language",
        _ => return Err(unsupported_capability()),
    };
    let freshness_requirement = match request.require_freshness.as_str() {
        "" | "none" => None,
        "structural" => Some("structural"),
        "semantic" => Some("semantic"),
        _ => return Err(invalid_argument()),
    };
    let status = service
        .repository_status(repository, selected)
        .map_err(|error| repository_status_error(error, repository, selected))?;
    let selected_freshness = match freshness_requirement {
        Some("structural") => &status.structural_freshness,
        Some("semantic") => &status.semantic_freshness,
        Some(_) => return Err(internal_error()),
        None => "current",
    };
    if selected_freshness != "current" {
        let mut builder =
            PublicError::builder(ErrorCode::StaleGeneration, "generation is not fresh")
                .repository(repository)
                .generation(status.resolved_generation)
                .next_action(NextAction::RebuildRepository);
        if selected.is_none() {
            builder = builder.next_action(NextAction::Retry);
        }
        return Err(builder
            .build()
            .unwrap_or_else(|_| unreachable!("freshness errors are statically bounded")));
    }
    let operation_candidates = lock_metadata(metadata)?.repository_operations(repository);
    let mut operations = Vec::new();
    operations
        .try_reserve_exact(operation_candidates.len())
        .map_err(|_| resource_exhausted())?;
    let mut repository_state = status.state.clone();
    for (operation, operation_metadata) in operation_candidates {
        let snapshot = match operation_metadata.terminal_snapshot {
            Some(snapshot) => snapshot,
            None => {
                let response = journal_call(
                    runtime,
                    context.deadline,
                    journal.control(ControlRequest::OperationStatus(operation)),
                );
                let record = match response {
                    Ok(ControlResponse::OperationStatus(record)) => record,
                    Err(error) if error.code() == ErrorCode::NotFound => continue,
                    Ok(_) => return Err(internal_error()),
                    Err(error) => return Err(error),
                };
                let snapshot = OperationStatusSnapshot::from_record(&record);
                lock_metadata(metadata)?.cache_snapshot(operation, snapshot);
                snapshot
            }
        };
        let visible_state = if operation_metadata.publication == PublicationState::FailedClosed {
            OperationState::Failed
        } else {
            snapshot.state
        };
        if !visible_state.is_terminal() {
            repository_state = "indexing".to_owned();
        }
        if request.include_operations {
            operations.push(daemon::RepositoryStatusOperation {
                operation: Some(operation_to_wire(operation)),
                kind: operation_kind_to_wire(snapshot.kind) as i32,
                state: operation_state_to_wire(visible_state) as i32,
                completed_units: snapshot.completed_units,
                total_units: snapshot.total_units,
                owned_by_client: snapshot.owner == context.client_instance_id,
                started_unix_ms: operation_metadata.started_unix_ms,
            });
        }
    }
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
        state: repository_state,
        coverage,
        resolved_generation: Some(generation_to_wire(status.resolved_generation)),
        active_parent_generation: status.active_parent_generation.map(generation_to_wire),
        display_name: status.display_name,
        alias: status.alias,
        publication_state: status.publication_state,
        operations,
        coverage_detail: coverage_detail.to_owned(),
        active_structural_freshness: status.active_structural_freshness,
        active_semantic_freshness: status.active_semantic_freshness,
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
    service: &FirstSliceService,
    generation: FirstSliceGenerationContext,
    usage: &QueryUsage,
    coverage: &[CoverageRecord],
) -> Result<daemon::FirstSliceQueryContext, PublicError> {
    let (tier, status, skipped) = aggregate_coverage(coverage, &generation.receipt);
    let freshness = service
        .generation_freshness(generation.repository, generation.generation)
        .map_err(service_error)?;
    Ok(daemon::FirstSliceQueryContext {
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
            token_accounting: Some(
                daemon::FirstSliceTokenAccountingProfile::FirstSliceTokenAccountingUtf8ByteUpperBoundV1
                    as i32,
                ),
            memory_bytes: Some(usage.memory_bytes),
        }),
        structural_freshness: query_freshness_label(freshness.structural).to_owned(),
        semantic_freshness: query_freshness_label(freshness.semantic).to_owned(),
    })
}

const fn query_freshness_label(freshness: FirstSliceObservedFreshness) -> &'static str {
    match freshness {
        FirstSliceObservedFreshness::CurrentAtLastAuthoritativeScan => "current",
        FirstSliceObservedFreshness::PendingSemanticRefinement => "stale",
        FirstSliceObservedFreshness::Superseded => "superseded",
        // A newer service state must not be promoted to current by an older daemon.
        _ => "stale",
    }
}

fn execution_completeness(
    state: ExecutionCompletenessState,
    resources: &[QueryResource],
    continuation_available: bool,
    guidance: daemon::FirstSliceContinuationGuidance,
) -> daemon::FirstSliceCompleteness {
    if state == ExecutionCompletenessState::Complete {
        return daemon::FirstSliceCompleteness {
            state: daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete as i32,
            limiting_resources: Vec::new(),
            continuation:
                daemon::FirstSliceContinuationAvailability::FirstSliceContinuationNotApplicable
                    as i32,
            guidance: Vec::new(),
        };
    }
    let limiting_resources = resources
        .iter()
        .copied()
        .map(|resource| daemon::FirstSliceLimitingResource {
            kind: limiting_resource_to_wire(resource) as i32,
            limit: None,
            observed: None,
        })
        .collect();
    daemon::FirstSliceCompleteness {
        state: match state {
            ExecutionCompletenessState::Complete => {
                daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete
            }
            ExecutionCompletenessState::Truncated => {
                daemon::FirstSliceCompletenessState::FirstSliceCompletenessTruncated
            }
            ExecutionCompletenessState::UnsupportedPartial => {
                daemon::FirstSliceCompletenessState::FirstSliceCompletenessUnsupportedPartial
            }
            _ => daemon::FirstSliceCompletenessState::FirstSliceCompletenessIndeterminate,
        } as i32,
        limiting_resources,
        continuation: if continuation_available && state == ExecutionCompletenessState::Truncated {
            daemon::FirstSliceContinuationAvailability::FirstSliceContinuationAvailable as i32
        } else {
            daemon::FirstSliceContinuationAvailability::FirstSliceContinuationUnavailable as i32
        },
        guidance: vec![
            if continuation_available && state == ExecutionCompletenessState::Truncated {
                daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceUseCursor as i32
            } else if state == ExecutionCompletenessState::UnsupportedPartial {
                daemon::FirstSliceContinuationGuidance::FirstSliceGuidanceUnsupportedNoContinuation
                    as i32
            } else {
                guidance as i32
            },
        ],
    }
}

const fn limiting_resource_to_wire(
    resource: QueryResource,
) -> daemon::FirstSliceLimitingResourceKind {
    match resource {
        QueryResource::Rows => daemon::FirstSliceLimitingResourceKind::FirstSliceLimitRows,
        QueryResource::Edges => daemon::FirstSliceLimitingResourceKind::FirstSliceLimitEdges,
        QueryResource::Results => daemon::FirstSliceLimitingResourceKind::FirstSliceLimitResults,
        QueryResource::SourceBytes => {
            daemon::FirstSliceLimitingResourceKind::FirstSliceLimitSourceBytes
        }
        QueryResource::JsonBytes => {
            daemon::FirstSliceLimitingResourceKind::FirstSliceLimitResponseBytes
        }
        QueryResource::Tokens => {
            daemon::FirstSliceLimitingResourceKind::FirstSliceLimitEstimatedTokens
        }
        QueryResource::MemoryBytes => {
            daemon::FirstSliceLimitingResourceKind::FirstSliceLimitMemoryBytes
        }
        QueryResource::Depth => daemon::FirstSliceLimitingResourceKind::FirstSliceLimitDepth,
        QueryResource::Paths => daemon::FirstSliceLimitingResourceKind::FirstSliceLimitPaths,
        QueryResource::Capability => {
            daemon::FirstSliceLimitingResourceKind::FirstSliceLimitCapability
        }
        _ => daemon::FirstSliceLimitingResourceKind::FirstSliceLimitUnspecified,
    }
}

fn aggregate_coverage(
    coverage: &[CoverageRecord],
    receipt: &FirstSliceIndexReceipt,
) -> (AnalysisTier, CoverageStatus, u64) {
    if coverage.is_empty() {
        let (tier, status) = if receipt.indexed_files == 0 {
            (AnalysisTier::TierD, CoverageStatus::Unknown)
        } else if receipt.discovered_inputs == receipt.indexed_files {
            (AnalysisTier::TierB, CoverageStatus::Complete)
        } else {
            (AnalysisTier::TierB, CoverageStatus::Bounded)
        };
        return (
            tier,
            status,
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
    if receipt.oversized_inputs > 0 {
        status = weaker_coverage(status, CoverageStatus::Bounded);
        skipped = skipped.saturating_add(receipt.oversized_inputs);
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

/// Returns the stable wire label for a coverage status, matching the client's
/// `coverage_status_from_label` parsing.
const fn coverage_label(status: CoverageStatus) -> &'static str {
    match status {
        CoverageStatus::Complete => "complete",
        CoverageStatus::Bounded => "bounded",
        CoverageStatus::Sampled => "sampled",
        CoverageStatus::Unknown => "unknown",
        _ => "unknown",
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

const fn repository_index_mode_tag(mode: FirstSliceIndexMode) -> u8 {
    match mode {
        FirstSliceIndexMode::Structural => 1,
        FirstSliceIndexMode::Deep => 2,
    }
}

const fn repository_index_mode_to_wire(mode: FirstSliceIndexMode) -> daemon::RepositoryIndexMode {
    match mode {
        FirstSliceIndexMode::Structural => daemon::RepositoryIndexMode::RepositoryIndexStructural,
        FirstSliceIndexMode::Deep => daemon::RepositoryIndexMode::RepositoryIndexDeep,
    }
}

const fn operation_kind_to_wire(kind: OperationKind) -> daemon::OperationKind {
    match kind {
        OperationKind::ControlProbe => daemon::OperationKind::ControlProbe,
        OperationKind::RepositoryIndex => daemon::OperationKind::RepositoryIndex,
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
            .map_err(|_| cancellation_error(CancellationReason::DeadlineExceeded))?
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

fn operation_progress(observed: FirstSliceIndexProgress) -> Result<Progress, PublicError> {
    let completed = u32::try_from(observed.completed).map_err(|_| internal_error())?;
    let total = u32::try_from(observed.total).map_err(|_| internal_error())?;
    Progress::new(completed, total).map_err(|_| internal_error())
}

fn index_support_inventory(
    service: &FirstSliceService,
) -> Result<IndexSupportInventory, FirstSliceError> {
    let snapshot = service.support_inventory_snapshot()?;
    Ok(map_index_support_inventory(snapshot))
}

fn map_index_support_inventory(snapshot: FirstSliceSupportInventory) -> IndexSupportInventory {
    let generation_format = snapshot.generation_format.clone();
    IndexSupportInventory {
        adapters: snapshot
            .adapters
            .into_iter()
            .map(|adapter| SupportAdapterInventory {
                name: adapter.name,
                version: Some(env!("CARGO_PKG_VERSION").to_owned()),
                languages: adapter.languages,
                available: true,
                isolated: adapter.isolated,
                binary_sha256: None,
                artifact_sha256: None,
            })
            .collect(),
        repositories: snapshot
            .repositories
            .into_iter()
            .map(|repository| SupportRepositoryInventory {
                repository_id: support_hex(repository.repository.as_bytes()),
                root_fingerprint_sha256: None,
                languages: repository.languages,
                tiers: repository.tiers,
                state: "ready".to_owned(),
                file_count: repository.files,
                symbol_count: repository.symbols,
                relationship_count: repository.relationships,
                generation_count: repository.generation_count,
            })
            .collect(),
        generations: snapshot
            .generations
            .into_iter()
            .map(|generation| SupportGenerationInventory {
                repository_id: support_hex(generation.repository.as_bytes()),
                generation_id: support_hex(generation.generation.as_bytes()),
                format_version: generation_format.clone(),
                checksum_status: SupportChecksumStatus::Verified,
                disk_bytes: generation.disk_bytes,
                state: if generation.active {
                    "active".to_owned()
                } else {
                    "superseded".to_owned()
                },
            })
            .collect(),
        generation_format_version: Some(snapshot.generation_format),
        generation_disk_bytes: snapshot.generation_disk_bytes,
        unreclaimed_temporary_bytes: 0,
        disk_margin_bytes: None,
    }
}

fn refresh_index_support_inventory(
    service: &RwLock<FirstSliceService>,
    state: Option<&DaemonState>,
) {
    let Some(state) = state else {
        return;
    };
    let inventory = match service.read() {
        Ok(service) => index_support_inventory(&service),
        Err(_) => {
            state.set_catalog_status(HealthStatus::Degraded);
            return;
        }
    };
    if inventory.is_err()
        || inventory
            .is_ok_and(|inventory| state.replace_index_support_inventory(inventory).is_err())
    {
        state.set_catalog_status(HealthStatus::Degraded);
    }
}

fn support_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn lifecycle_deadline(client_deadline: Instant) -> Result<Instant, PublicError> {
    client_deadline
        .checked_add(LIFECYCLE_FINALIZATION_GRACE)
        .ok_or_else(internal_error)
}

fn fresh_lifecycle_deadline(deadline: Instant) -> Result<Instant, PublicError> {
    fresh_lifecycle_deadline_at(deadline, Instant::now())
}

fn fresh_lifecycle_deadline_at(deadline: Instant, now: Instant) -> Result<Instant, PublicError> {
    Ok(deadline.max(lifecycle_deadline(now)?))
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
    build_service_error(error, None)
}

#[derive(Debug, Clone, Copy)]
struct RepositoryIndexErrorContext {
    operation: OperationId,
    repository: RepositoryId,
    provider: &'static str,
}

fn repository_index_error(
    error: FirstSliceError,
    context: RepositoryIndexErrorContext,
) -> PublicError {
    build_service_error(error, Some(context))
}

fn build_service_error(
    error: FirstSliceError,
    context: Option<RepositoryIndexErrorContext>,
) -> PublicError {
    let (code, message, retryable, failure_family, failure_stage) = match error {
        FirstSliceError::Configuration => (
            ErrorCode::Internal,
            "first-slice configuration failed",
            false,
            "configuration",
            "configuration",
        ),
        FirstSliceError::RandomUnavailable => (
            ErrorCode::Internal,
            "repository identity allocation failed",
            true,
            "identity_allocation",
            "admission",
        ),
        FirstSliceError::DeadlineRequired => (
            ErrorCode::InvalidArgument,
            "repository indexing requires a deadline",
            false,
            "deadline",
            "admission",
        ),
        FirstSliceError::Cancelled(CancellationReason::DeadlineExceeded) => (
            ErrorCode::Busy,
            "operation deadline elapsed",
            true,
            "deadline",
            "executing",
        ),
        FirstSliceError::Cancelled(CancellationReason::Shutdown) => (
            ErrorCode::Busy,
            "operation was interrupted by shutdown",
            true,
            "shutdown",
            "executing",
        ),
        FirstSliceError::Cancelled(CancellationReason::ResourceLimit) => (
            ErrorCode::ResourceExhausted,
            "operation resource limit was reached",
            false,
            "resource_limit",
            "executing",
        ),
        FirstSliceError::Cancelled(
            CancellationReason::ClientRequest | CancellationReason::ParentCancelled,
        ) => (
            ErrorCode::Cancelled,
            "operation was cancelled",
            false,
            "cancellation",
            "executing",
        ),
        FirstSliceError::RepositoryNotFound => (
            ErrorCode::NotFound,
            "repository was not found",
            false,
            "repository_lookup",
            "admission",
        ),
        FirstSliceError::GenerationNotFound => (
            ErrorCode::StaleGeneration,
            "generation is not retained",
            false,
            "generation_lookup",
            "query",
        ),
        FirstSliceError::GenerationMismatch => (
            ErrorCode::Conflict,
            "generation does not belong to repository",
            false,
            "generation_identity",
            "query",
        ),
        FirstSliceError::FixtureShape => (
            ErrorCode::UnsupportedCapability,
            "repository shape is unsupported",
            false,
            "repository_shape",
            "discovery",
        ),
        FirstSliceError::Repository => (
            ErrorCode::NotFound,
            "repository root is unavailable",
            false,
            "repository_access",
            "discovery",
        ),
        FirstSliceError::Discovery => (
            ErrorCode::Internal,
            "repository discovery failed",
            false,
            "discovery",
            "discovery",
        ),
        FirstSliceError::Incremental => (
            ErrorCode::Internal,
            "incremental planning failed",
            false,
            "incremental_planning",
            "planning",
        ),
        FirstSliceError::DiscoveryDrift => (
            ErrorCode::Conflict,
            "repository changed during indexing",
            true,
            "discovery_drift",
            "snapshot",
        ),
        FirstSliceError::Retention => (
            ErrorCode::ResourceExhausted,
            "first-slice retention limit was reached",
            true,
            "retention_limit",
            "executing",
        ),
        FirstSliceError::ResourceLimit { .. } => (
            ErrorCode::ResourceExhausted,
            "first-slice resource limit was reached",
            false,
            "resource_limit",
            "executing",
        ),
        FirstSliceError::Limits => (
            ErrorCode::ResourceExhausted,
            "first-slice safety limit was reached",
            true,
            "safety_limit",
            "executing",
        ),
        FirstSliceError::InsufficientDiskSpace { .. } => (
            ErrorCode::ResourceExhausted,
            "insufficient disk space for repository indexing",
            true,
            "disk_space",
            "admission",
        ),
        FirstSliceError::SymbolNotFound => (
            ErrorCode::NotFound,
            "symbol was not found",
            false,
            "symbol_lookup",
            "query",
        ),
        FirstSliceError::BudgetExceeded => (
            ErrorCode::BudgetExceeded,
            "first-slice execution budget is exhausted",
            false,
            "execution_budget",
            "query",
        ),
        FirstSliceError::Query => (
            ErrorCode::NotFound,
            "query target was not found",
            false,
            "query",
            "query",
        ),
        FirstSliceError::Adapter => (
            ErrorCode::AdapterFailed,
            "repository analysis failed",
            false,
            "adapter",
            "analysis",
        ),
        FirstSliceError::AdapterWallTimeLimit => (
            ErrorCode::ResourceExhausted,
            "project adapter wall-time limit was reached",
            true,
            "adapter_wall_time_limit",
            "analysis",
        ),
        FirstSliceError::AdapterProcessFailure => (
            ErrorCode::AdapterFailed,
            "project adapter process failed",
            true,
            "adapter_process_failure",
            "analysis",
        ),
        FirstSliceError::Resolution => (
            ErrorCode::Internal,
            "semantic resolution failed",
            false,
            "resolution",
            "resolution",
        ),
        FirstSliceError::Identity => (
            ErrorCode::Internal,
            "generation identity verification failed",
            false,
            "identity_verification",
            "verification",
        ),
        FirstSliceError::Catalog => (
            ErrorCode::Internal,
            "generation persistence failed",
            false,
            "catalog",
            "publication",
        ),
        FirstSliceError::Search => (
            ErrorCode::Internal,
            "search index construction failed",
            false,
            "search",
            "indexing",
        ),
        FirstSliceError::Source => (
            ErrorCode::Internal,
            "source retention failed",
            false,
            "source",
            "publication",
        ),
        FirstSliceError::Sharing => (
            ErrorCode::Internal,
            "generation transfer failed",
            false,
            "generation_transfer",
            "transfer",
        ),
        FirstSliceError::RuntimeTrace(_) => (
            ErrorCode::InvalidArgument,
            "runtime trace import failed",
            false,
            "runtime_trace",
            "query",
        ),
        FirstSliceError::CatalogCorrupt => (
            ErrorCode::IndexCorrupt,
            "repository generation is corrupt",
            false,
            "catalog_integrity",
            "verification",
        ),
        FirstSliceError::CatalogMigrationRequired => (
            ErrorCode::MigrationRequired,
            "repository generation requires migration",
            false,
            "catalog_schema",
            "verification",
        ),
        _ => (
            ErrorCode::Internal,
            "first-slice operation failed",
            false,
            "unknown",
            "executing",
        ),
    };
    let mut builder = PublicError::builder(code, message)
        .detail(
            static_detail_key("failure_family"),
            PublicValue::Label(static_safe_label(failure_family)),
        )
        .detail(
            static_detail_key("failure_stage"),
            PublicValue::Label(static_safe_label(failure_stage)),
        );
    if let Some(context) = context {
        builder = builder
            .operation(context.operation)
            .repository(context.repository)
            .detail(
                static_detail_key("provider"),
                PublicValue::Label(static_safe_label(context.provider)),
            )
            .next_action(NextAction::InspectOperation);
    }
    if let FirstSliceError::ResourceLimit {
        resource,
        observed,
        limit,
    } = error
    {
        builder = builder
            .detail(
                static_detail_key("resource"),
                PublicValue::Label(static_safe_label(resource.as_str())),
            )
            .detail(
                static_detail_key("observed"),
                PublicValue::Unsigned(observed),
            )
            .detail(static_detail_key("limit"), PublicValue::Unsigned(limit))
            .next_action(NextAction::CollectSupportBundle);
    }
    if let FirstSliceError::InsufficientDiskSpace {
        required_bytes,
        available_bytes,
    } = error
    {
        builder = builder
            .detail(
                static_detail_key("resource"),
                PublicValue::Label(static_safe_label("disk_bytes")),
            )
            .detail(
                static_detail_key("required_bytes"),
                PublicValue::Unsigned(required_bytes),
            )
            .detail(
                static_detail_key("available_bytes"),
                PublicValue::Unsigned(available_bytes),
            );
    }
    if matches!(
        error,
        FirstSliceError::AdapterWallTimeLimit | FirstSliceError::AdapterProcessFailure
    ) {
        builder = builder
            .detail(
                static_detail_key("structural_fallback"),
                PublicValue::Boolean(true),
            )
            .next_action(NextAction::CollectSupportBundle);
    }
    if error == FirstSliceError::AdapterWallTimeLimit {
        builder = builder
            .detail(
                static_detail_key("resource"),
                PublicValue::Label(static_safe_label("adapter_wall_time_ms")),
            )
            .detail(
                static_detail_key("limit"),
                PublicValue::Unsigned(PROJECT_ADAPTER_WALL_TIME_MS),
            );
    }
    if error == FirstSliceError::AdapterProcessFailure {
        builder = builder.detail(
            static_detail_key("resource"),
            PublicValue::Label(static_safe_label("adapter_process")),
        );
    }
    if matches!(
        error,
        FirstSliceError::CatalogCorrupt | FirstSliceError::CatalogMigrationRequired
    ) {
        builder = builder.next_action(NextAction::RebuildRepository);
    }
    if retryable {
        builder = if matches!(
            error,
            FirstSliceError::AdapterWallTimeLimit | FirstSliceError::AdapterProcessFailure
        ) {
            builder.retry_after(retry_after())
        } else {
            builder.retryable()
        }
        .next_action(NextAction::Retry);
    }
    if matches!(
        error,
        FirstSliceError::Discovery
            | FirstSliceError::Incremental
            | FirstSliceError::Adapter
            | FirstSliceError::Resolution
            | FirstSliceError::Identity
            | FirstSliceError::Catalog
            | FirstSliceError::Search
            | FirstSliceError::Source
    ) {
        builder = builder.next_action(NextAction::CollectSupportBundle);
    }
    builder
        .build()
        .unwrap_or_else(|_| unreachable!("closed first-slice errors are statically bounded"))
}

fn repository_index_provider(mode: FirstSliceIndexMode) -> &'static str {
    match mode {
        FirstSliceIndexMode::Structural => "rootlight-first-slice-treesitter",
        FirstSliceIndexMode::Deep => "rootlight-project-analyzer",
    }
}

const fn first_slice_index_provider(mode: FirstSliceIndexMode) -> FirstSliceIndexProvider {
    match mode {
        FirstSliceIndexMode::Structural => FirstSliceIndexProvider::TreeSitter,
        FirstSliceIndexMode::Deep => FirstSliceIndexProvider::ProjectAnalyzer,
    }
}

const fn repository_operation_mode(mode: FirstSliceIndexMode) -> RepositoryOperationMode {
    match mode {
        FirstSliceIndexMode::Structural => RepositoryOperationMode::Structural,
        FirstSliceIndexMode::Deep => RepositoryOperationMode::Deep,
    }
}

const fn repository_operation_support_provider(
    mode: RepositoryOperationMode,
) -> RepositoryIndexProvider {
    match mode {
        RepositoryOperationMode::Auto | RepositoryOperationMode::Structural => {
            RepositoryIndexProvider::TreeSitter
        }
        RepositoryOperationMode::Deep => RepositoryIndexProvider::ProjectAnalyzer,
    }
}

const fn repository_index_support_provider(
    provider: FirstSliceIndexProvider,
) -> RepositoryIndexProvider {
    match provider {
        FirstSliceIndexProvider::Unknown => RepositoryIndexProvider::Legacy,
        FirstSliceIndexProvider::TreeSitter => RepositoryIndexProvider::TreeSitter,
        FirstSliceIndexProvider::ProjectAnalyzer => RepositoryIndexProvider::ProjectAnalyzer,
    }
}

fn static_detail_key(value: &'static str) -> DetailKey {
    DetailKey::parse(value)
        .unwrap_or_else(|_| unreachable!("static error detail keys are valid safe labels"))
}

fn static_safe_label(value: &'static str) -> SafeLabel {
    SafeLabel::parse(value)
        .unwrap_or_else(|_| unreachable!("static error detail values are valid safe labels"))
}

fn repository_status_error(
    error: FirstSliceError,
    repository: RepositoryId,
    selected: Option<GenerationId>,
) -> PublicError {
    let FirstSliceError::GenerationNotFound = error else {
        return service_error(error);
    };
    let builder = match selected {
        Some(generation) => {
            PublicError::builder(ErrorCode::StaleGeneration, "generation is not retained")
                .repository(repository)
                .generation(generation)
                .next_action(NextAction::RestartEnumeration)
        }
        None => PublicError::builder(
            ErrorCode::StaleGeneration,
            "repository has no published generation",
        )
        .repository(repository)
        .retryable()
        .next_action(NextAction::InspectOperation)
        .next_action(NextAction::Retry),
    };
    builder
        .build()
        .unwrap_or_else(|_| unreachable!("repository status errors are statically bounded"))
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
        | OperationError::CancellationTooLate
        | OperationError::InvalidTerminalError
        | OperationError::LeaseOwnerMismatch
        | OperationError::InvalidLease => (
            ErrorCode::Conflict,
            "operation state conflicts with request",
            false,
        ),
        OperationError::CancellationDenied => (
            ErrorCode::PermissionDenied,
            "operation cancellation is not authorized",
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

fn failed_closed_publication(operation: OperationId) -> PublicError {
    PublicError::builder(
        ErrorCode::IndexCorrupt,
        "repository publication failed before becoming queryable",
    )
    .operation(operation)
    .next_action(NextAction::RebuildRepository)
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

fn budget_exceeded() -> PublicError {
    PublicError::builder(
        ErrorCode::BudgetExceeded,
        "first-slice execution budget is exhausted",
    )
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn invalid_cursor() -> PublicError {
    PublicError::builder(
        ErrorCode::InvalidCursor,
        "pagination cursor is invalid or expired",
    )
    .next_action(NextAction::RestartEnumeration)
    .build()
    .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn catalog_error(error: CatalogError) -> PublicError {
    match error {
        CatalogError::InvalidQuery | CatalogError::InvalidPageSize => invalid_argument(),
        CatalogError::UnsupportedFilter(_) => unsupported_capability(),
        CatalogError::UnsupportedSortVersion
        | CatalogError::InvalidSortKey
        | CatalogError::SnapshotMismatch
        | CatalogError::SnapshotExpired
        | CatalogError::SnapshotEvicted
        | CatalogError::SnapshotUnavailable => invalid_cursor(),
        CatalogError::SnapshotEntryBound | CatalogError::SnapshotByteBound => resource_exhausted(),
        CatalogError::InvalidLimits
        | CatalogError::InvalidLabel
        | CatalogError::InvalidLanguage
        | CatalogError::InvalidCoverage
        | CatalogError::TooManyLanguages
        | CatalogError::DuplicateLanguage
        | CatalogError::InvalidGenerationState
        | CatalogError::DuplicateRepository
        | CatalogError::TimeRegressed
        | CatalogError::IdentityExhausted
        | CatalogError::CatalogInvariant => internal_error(),
        _ => internal_error(),
    }
}

fn cancelled_error() -> PublicError {
    PublicError::builder(ErrorCode::Cancelled, "first-slice request was cancelled")
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
}

fn cancellation_error(reason: CancellationReason) -> PublicError {
    match reason {
        CancellationReason::DeadlineExceeded => {
            PublicError::builder(ErrorCode::Busy, "first-slice request deadline elapsed")
                .retryable()
                .next_action(NextAction::Retry)
                .build()
                .unwrap_or_else(|_| unreachable!("closed public error is statically bounded"))
        }
        CancellationReason::Shutdown => PublicError::builder(
            ErrorCode::Busy,
            "first-slice request was interrupted by shutdown",
        )
        .retryable()
        .next_action(NextAction::Retry)
        .build()
        .unwrap_or_else(|_| unreachable!("closed public error is statically bounded")),
        CancellationReason::ResourceLimit => resource_exhausted(),
        CancellationReason::ClientRequest | CancellationReason::ParentCancelled => {
            cancelled_error()
        }
        _ => internal_error(),
    }
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
    use rootlight_runtime::RuntimePaths;
    use std::{
        fs,
        sync::atomic::{AtomicU8, AtomicUsize, Ordering as AtomicOrdering},
        time::Duration,
    };
    use tempfile::TempDir;

    static OBSERVED_STARTUP_SIGNAL: AtomicU8 = AtomicU8::new(0);

    fn record_startup_signal(signal: CoordinatedStartupSignal) -> io::Result<()> {
        OBSERVED_STARTUP_SIGNAL.store(signal.to_byte(), AtomicOrdering::SeqCst);
        Ok(())
    }

    struct FailingSemanticAnalyzer {
        calls: Arc<AtomicUsize>,
        error: FirstSliceProjectAnalysisError,
    }

    impl FirstSliceProjectAnalyzer for FailingSemanticAnalyzer {
        fn provider_identity(&self) -> ContentHash {
            content_hash(b"daemon-failing-semantic-analyzer")
        }

        fn analyze(
            &self,
            _request: FirstSliceProjectAnalysisRequest<'_>,
            _cancellation: &Cancellation,
        ) -> Result<FirstSliceProjectAnalysis, FirstSliceProjectAnalysisError> {
            self.calls.fetch_add(1, AtomicOrdering::Relaxed);
            Err(self.error)
        }
    }

    fn durable_test_tempdir() -> TempDir {
        #[cfg(target_os = "macos")]
        {
            // Avoid the default `/var` alias rejected by repository-root VFS checks.
            tempfile::Builder::new()
                .prefix("rl-daemon-durable-")
                .tempdir_in("/private/tmp")
                .expect("durable daemon test directory is available")
        }
        #[cfg(not(target_os = "macos"))]
        {
            TempDir::new().expect("durable daemon test directory is available")
        }
    }

    #[test]
    fn support_inventory_mapping_omits_repository_root_fingerprints() {
        let repository = RepositoryId::from_bytes([41; 16]);
        let mapped = map_index_support_inventory(FirstSliceSupportInventory {
            adapters: Vec::new(),
            repositories: vec![rootlight_service::FirstSliceSupportRepository {
                repository,
                languages: vec!["rust".to_owned()],
                tiers: vec!["tier_b".to_owned()],
                files: 3,
                symbols: 5,
                relationships: 7,
                generation_count: 1,
            }],
            generations: Vec::new(),
            generation_format: "1.2".to_owned(),
            generation_disk_bytes: 0,
        });

        assert_eq!(mapped.repositories.len(), 1);
        assert_eq!(
            mapped.repositories[0].repository_id,
            support_hex(repository.as_bytes())
        );
        assert_eq!(mapped.repositories[0].root_fingerprint_sha256, None);
        assert_eq!(mapped.repositories[0].generation_count, 1);
    }

    #[test]
    fn resource_pressure_classification_is_bounded_and_monotonic() {
        let gibibyte = 1024 * 1024 * 1024;
        let total = 16 * gibibyte;

        assert_eq!(
            classify_resource_pressure(0, 0, 0),
            ResourcePressure::Unknown
        );
        assert_eq!(
            classify_resource_pressure(total, 8 * gibibyte, gibibyte),
            ResourcePressure::Normal
        );
        assert_eq!(
            classify_resource_pressure(total, 3 * gibibyte, 8 * gibibyte),
            ResourcePressure::Elevated
        );
        assert_eq!(
            classify_resource_pressure(total, gibibyte, 11 * gibibyte),
            ResourcePressure::High
        );
        assert_eq!(
            classify_resource_pressure(total, gibibyte / 2, 13 * gibibyte),
            ResourcePressure::Critical
        );
    }

    #[test]
    fn first_slice_budget_exhaustion_keeps_its_public_error_code() {
        let error = service_error(FirstSliceError::BudgetExceeded);

        assert_eq!(error.code(), ErrorCode::BudgetExceeded);
        assert!(!error.retryable());
    }

    #[test]
    fn service_cancellation_preserves_non_client_causes() {
        let deadline = service_error(FirstSliceError::Cancelled(
            CancellationReason::DeadlineExceeded,
        ));
        assert_eq!(deadline.code(), ErrorCode::Busy);
        assert!(deadline.retryable());
        assert_eq!(deadline.next_actions(), &[NextAction::Retry]);

        let shutdown = service_error(FirstSliceError::Cancelled(CancellationReason::Shutdown));
        assert_eq!(shutdown.code(), ErrorCode::Busy);
        assert!(shutdown.retryable());

        let resource = service_error(FirstSliceError::Cancelled(
            CancellationReason::ResourceLimit,
        ));
        assert_eq!(resource.code(), ErrorCode::ResourceExhausted);
        assert!(!resource.retryable());

        let client = service_error(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest,
        ));
        assert_eq!(client.code(), ErrorCode::Cancelled);
        assert!(!client.retryable());

        let queued_deadline = cancellation_error(CancellationReason::DeadlineExceeded);
        assert_eq!(queued_deadline.code(), ErrorCode::Busy);
        assert!(queued_deadline.retryable());
        assert_eq!(queued_deadline.next_actions(), &[NextAction::Retry]);
    }

    #[test]
    fn project_partition_buffer_splits_oversized_input_deterministically() {
        fn input(file_byte: u8, path: &str, source_bytes: usize) -> adapter::ProjectInput {
            adapter::ProjectInput {
                file: Some(common::FileId {
                    value: vec![file_byte; 20],
                }),
                path: path.to_owned(),
                language: "python".to_owned(),
                source_digest: Some(common::ContentHash {
                    value: vec![file_byte; 32],
                }),
                source: vec![file_byte; source_bytes],
                generated: false,
                origins: Vec::new(),
            }
        }

        let base_request = adapter::ProjectAnalysisRequest {
            session_id: vec![1; ADAPTER_NONCE_BYTES],
            request_id: vec![2; ADAPTER_NONCE_BYTES],
            repository: Some(common::RepositoryId { value: vec![3; 16] }),
            generation: Some(common::GenerationId { value: vec![4; 20] }),
            analysis_unit: "first-slice.python.partition".to_owned(),
            target: "//rootlight:python/partition".to_owned(),
            build_context: Some(common::ContentHash { value: vec![5; 32] }),
            config_digest: Some(common::ContentHash { value: vec![6; 32] }),
            inputs: Vec::new(),
            context_manifest: b"context".to_vec(),
            requested_tier: adapter::RequestedAnalysisTier::TierB as i32,
        };
        let base_payload = project_analysis_request_payload_bytes(&base_request);
        let first = input(7, "Lib/first.py", 3 * 1024 * 1024);
        let second = input(8, "Lib/second.py", 3 * 1024 * 1024);
        let mut buffer = ProjectPartitionBuffer::new(base_payload, 7, 4_096)
            .expect("partition buffer initializes");

        assert!(
            buffer
                .try_push(first)
                .expect("first input is measurable")
                .is_none()
        );
        let rejected = buffer
            .try_push(second)
            .expect("second input is measurable")
            .expect("aggregate input crosses the partition limit");
        assert!(
            project_analysis_frame_bytes(buffer.request_payload_bytes)
                .is_some_and(|bytes| bytes <= MAX_ADAPTER_FRAME_BYTES)
        );
        let first_batch = buffer.take();
        assert_eq!(first_batch.len(), 1);
        assert!(
            buffer
                .try_push(rejected)
                .expect("rejected input fits an empty partition")
                .is_none()
        );
        let second_batch = buffer.take();
        assert_eq!(second_batch.len(), 1);
        assert!(
            first_batch[0]
                .source
                .len()
                .checked_add(second_batch[0].source.len())
                .is_some_and(|bytes| {
                    bytes
                        > usize::try_from(PROJECT_ADAPTER_PARTITION_SOURCE_BYTES)
                            .expect("partition source limit fits usize")
                        && bytes
                            < usize::try_from(PROJECT_ADAPTER_INPUT_BYTES)
                                .expect("adapter input limit fits usize")
                })
        );
    }

    #[test]
    fn project_partition_buffer_enforces_the_adapter_file_ceiling() {
        let mut buffer =
            ProjectPartitionBuffer::new(128, 8, 2).expect("partition buffer initializes");
        for file_byte in [1_u8, 2] {
            assert!(
                buffer
                    .try_push(adapter::ProjectInput {
                        file: Some(common::FileId {
                            value: vec![file_byte; 20],
                        }),
                        path: format!("Lib/{file_byte}.py"),
                        language: "python".to_owned(),
                        source_digest: Some(common::ContentHash {
                            value: vec![file_byte; 32],
                        }),
                        source: b"pass\n".to_vec(),
                        generated: false,
                        origins: Vec::new(),
                    })
                    .expect("input is measurable")
                    .is_none()
            );
        }
        assert!(
            buffer
                .try_push(adapter::ProjectInput {
                    file: Some(common::FileId { value: vec![3; 20] }),
                    path: "Lib/3.py".to_owned(),
                    language: "python".to_owned(),
                    source_digest: Some(common::ContentHash { value: vec![3; 32] }),
                    source: b"pass\n".to_vec(),
                    generated: false,
                    origins: Vec::new(),
                })
                .expect("input is measurable")
                .is_some()
        );
    }

    #[test]
    fn project_partition_buffer_batches_small_files_for_large_repositories() {
        let mut buffer =
            ProjectPartitionBuffer::new(128, 8, usize::MAX).expect("partition buffer initializes");
        for file_index in 0..PROJECT_ADAPTER_PARTITION_FILES {
            let encoded = u16::try_from(file_index)
                .expect("partition fixture index fits")
                .to_le_bytes();
            let file_id = encoded.repeat(10);
            let source_digest = encoded.repeat(16);
            assert!(
                buffer
                    .try_push(adapter::ProjectInput {
                        file: Some(common::FileId { value: file_id }),
                        path: format!("src/{file_index}.ts"),
                        language: "typescript".to_owned(),
                        source_digest: Some(common::ContentHash {
                            value: source_digest,
                        }),
                        source: b"export const value = 1;\n".to_vec(),
                        generated: false,
                        origins: Vec::new(),
                    })
                    .expect("input is measurable")
                    .is_none()
            );
        }
        assert_eq!(buffer.take().len(), PROJECT_ADAPTER_PARTITION_FILES);
    }

    #[test]
    fn project_adapter_failures_keep_their_public_resource_class() {
        assert_eq!(
            map_project_adapter_error(AdapterHostError::ProcessTimeout),
            FirstSliceProjectAnalysisError::WallTimeLimit
        );
        assert_eq!(
            map_project_adapter_error(AdapterHostError::ProcessFailed),
            FirstSliceProjectAnalysisError::ProcessFailure
        );

        let operation = OperationId::from_bytes([17; 16]);
        let repository = RepositoryId::from_bytes([18; 16]);
        let context = RepositoryIndexErrorContext {
            operation,
            repository,
            provider: repository_index_provider(FirstSliceIndexMode::Deep),
        };
        let wall_time = repository_index_error(FirstSliceError::AdapterWallTimeLimit, context);

        assert_eq!(wall_time.code(), ErrorCode::ResourceExhausted);
        assert!(wall_time.retryable());
        assert_eq!(wall_time.retry_after_ms(), Some(u64::from(RETRY_AFTER_MS)));
        assert_eq!(
            wall_time.details().get(&static_detail_key("resource")),
            Some(&PublicValue::Label(static_safe_label(
                "adapter_wall_time_ms"
            )))
        );
        assert_eq!(
            wall_time.details().get(&static_detail_key("limit")),
            Some(&PublicValue::Unsigned(PROJECT_ADAPTER_WALL_TIME_MS))
        );
        assert_eq!(
            wall_time
                .details()
                .get(&static_detail_key("structural_fallback")),
            Some(&PublicValue::Boolean(true))
        );
        assert_eq!(
            wall_time.next_actions(),
            &[
                NextAction::InspectOperation,
                NextAction::CollectSupportBundle,
                NextAction::Retry,
            ]
        );

        let process = repository_index_error(FirstSliceError::AdapterProcessFailure, context);
        assert_eq!(process.code(), ErrorCode::AdapterFailed);
        assert_eq!(
            process.details().get(&static_detail_key("resource")),
            Some(&PublicValue::Label(static_safe_label("adapter_process")))
        );
        assert_eq!(process.retry_after_ms(), Some(u64::from(RETRY_AFTER_MS)));
    }

    #[test]
    fn repository_index_failures_retain_safe_operation_context() {
        let operation = OperationId::from_bytes([19; 16]);
        let repository = RepositoryId::from_bytes([23; 16]);
        let error = repository_index_error(
            FirstSliceError::Discovery,
            RepositoryIndexErrorContext {
                operation,
                repository,
                provider: repository_index_provider(FirstSliceIndexMode::Structural),
            },
        );

        assert_eq!(error.code(), ErrorCode::Internal);
        assert_eq!(error.operation(), Some(operation));
        assert_eq!(error.repository(), Some(repository));
        assert_eq!(
            error.details().get(&static_detail_key("failure_family")),
            Some(&PublicValue::Label(static_safe_label("discovery")))
        );
        assert_eq!(
            error.details().get(&static_detail_key("failure_stage")),
            Some(&PublicValue::Label(static_safe_label("discovery")))
        );
        assert_eq!(
            error.details().get(&static_detail_key("provider")),
            Some(&PublicValue::Label(static_safe_label(
                "rootlight-first-slice-treesitter"
            )))
        );
        assert_eq!(
            error.next_actions(),
            &[
                NextAction::InspectOperation,
                NextAction::CollectSupportBundle
            ]
        );

        let bounded = repository_index_error(
            FirstSliceError::ResourceLimit {
                resource: rootlight_service::FirstSliceResource::Extensions,
                observed: 10_001,
                limit: 10_000,
            },
            RepositoryIndexErrorContext {
                operation,
                repository,
                provider: repository_index_provider(FirstSliceIndexMode::Structural),
            },
        );
        assert_eq!(bounded.code(), ErrorCode::ResourceExhausted);
        assert!(!bounded.retryable());
        assert_eq!(
            bounded.details().get(&static_detail_key("resource")),
            Some(&PublicValue::Label(static_safe_label("extensions")))
        );
        assert_eq!(
            bounded.details().get(&static_detail_key("observed")),
            Some(&PublicValue::Unsigned(10_001))
        );
        assert_eq!(
            bounded.details().get(&static_detail_key("limit")),
            Some(&PublicValue::Unsigned(10_000))
        );
        assert_eq!(
            bounded.next_actions(),
            &[
                NextAction::InspectOperation,
                NextAction::CollectSupportBundle
            ]
        );
    }

    #[test]
    fn auto_index_keeps_the_structural_stage_when_semantic_refinement_fails() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn structural_stage() -> bool { true }\n",
        )
        .expect("fixture source writes");

        let calls = Arc::new(AtomicUsize::new(0));
        let analyzer: Arc<dyn FirstSliceProjectAnalyzer> = Arc::new(FailingSemanticAnalyzer {
            calls: Arc::clone(&calls),
            error: FirstSliceProjectAnalysisError::WallTimeLimit,
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let cancellation = Cancellation::with_deadline(deadline);
        let service = Arc::new(RwLock::new(
            FirstSliceService::new_durable_with_project_analyzer(
                3,
                paths.state_dir(),
                analyzer,
                &cancellation,
            )
            .expect("durable semantic service initializes"),
        ));
        let journal = Arc::new(
            OperationJournal::open(&paths.operation_journal_path())
                .expect("operation journal opens"),
        );
        let actor = JournalActor::start(Arc::clone(&journal), 8, 8).expect("journal actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(8));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let resources = ServiceRequestResources {
            journal: &handle,
            metadata: &metadata,
            runtime: &runtime,
            catalog_epoch: Instant::now(),
            publication_hook: None,
        };
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation,
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let operation = OperationId::from_bytes([91; 16]);
        let request = daemon::RepositoryIndexRequest {
            schema_version: Some(schema_version()),
            root: fixture.path().to_string_lossy().into_owned(),
            operation: Some(operation_to_wire(operation)),
            detached: false,
            mode: daemon::RepositoryIndexMode::RepositoryIndexAuto as i32,
        };
        let (reply_sender, reply_receiver) = tokio::sync::oneshot::channel();
        let mut reply = Some(reply_sender);
        let index_serialization = Arc::new(Mutex::new(()));
        let semantic_refinements = Arc::new(Mutex::new(BTreeMap::new()));
        let (refinement, refinement_receiver) = mpsc::sync_channel(1);
        let lanes = FirstSliceServiceLanes {
            service: Arc::clone(&service),
            index_serialization,
            semantic_refinements,
            refinement,
            recovery_ready: Arc::new(AtomicBool::new(true)),
            support_state: None,
        };

        let response = repository_index(&lanes, resources, request, &context, &mut reply)
            .expect("structural stage publishes");

        assert!(reply.is_none());
        assert_eq!(
            response.mode,
            daemon::RepositoryIndexMode::RepositoryIndexStructural as i32
        );
        assert!(response.published_generation.is_some());
        assert!(
            response.semantic_operation.is_none(),
            "the parent cannot advertise a child before durable child admission"
        );
        assert_eq!(
            journal
                .repository_operation_context(operation)
                .expect("auto operation context persists")
                .mode,
            RepositoryOperationMode::Auto
        );
        let delivered = runtime
            .block_on(reply_receiver)
            .expect("structural response is delivered")
            .expect("structural response succeeds");
        assert!(matches!(
            delivered,
            FirstSliceIpcResponse::RepositoryIndex(ref delivered) if delivered == &response
        ));
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 0);
        let repositories = read_service(&service)
            .expect("service read lock is available")
            .list_repositories();
        let [repository] = repositories.as_slice() else {
            panic!("only one repository is active");
        };
        assert_eq!(repository.semantic_freshness, "pending_refinement");
        let catalog = {
            let service = read_service(&service).expect("service read lock is available");
            repository_catalog_page(
                &service,
                catalog_request(20, false, Vec::new(), None, None),
                Instant::now(),
            )
            .expect("catalog remains available during semantic refinement")
        };
        assert_eq!(catalog.repositories.len(), 1);
        assert_eq!(catalog.repositories[0].structural_freshness, "current");
        assert_eq!(catalog.repositories[0].semantic_freshness, "stale");
        let locate = {
            let service = read_service(&service).expect("service read lock is available");
            code_locate(
                &service,
                daemon::CodeLocateRequest {
                    schema_version: Some(schema_version()),
                    repository: Some(repository_to_wire(repository.repository)),
                    generation: Some(daemon::GenerationSelector {
                        selector: Some(daemon::generation_selector::Selector::Active(true)),
                    }),
                    query: "structural_stage".to_owned(),
                    mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
                    maximum_results: 8,
                    page_offset: 0,
                    languages: vec!["rust".to_owned()],
                },
                &context,
            )
            .expect("the structural generation remains queryable")
        };
        let query_context = locate.context.expect("query context exists");
        assert!(!locate.hits.is_empty());
        assert!(locate.hits.iter().all(|hit| hit.language == "rust"));
        assert_eq!(query_context.structural_freshness, "current");
        assert_eq!(query_context.semantic_freshness, "stale");
        let unknown_language = {
            let service = read_service(&service).expect("service read lock is available");
            code_locate(
                &service,
                daemon::CodeLocateRequest {
                    schema_version: Some(schema_version()),
                    repository: Some(repository_to_wire(repository.repository)),
                    generation: Some(daemon::GenerationSelector {
                        selector: Some(daemon::generation_selector::Selector::Active(true)),
                    }),
                    query: "structural_stage".to_owned(),
                    mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
                    maximum_results: 8,
                    page_offset: 0,
                    languages: vec!["unknown".to_owned()],
                },
                &context,
            )
            .expect("an unknown canonical language selects an empty query domain")
        };
        assert!(unknown_language.hits.is_empty());
        assert_eq!(unknown_language.matched_candidates, 0);
        assert_eq!(
            unknown_language
                .context
                .expect("query context exists")
                .coverage_status,
            daemon::FirstSliceCoverageStatus::FirstSliceCoverageComplete as i32
        );
        let refinement = refinement_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("semantic refinement is scheduled");
        let mut refinement_reply = None;
        drop(repository_index_with_intent(
            &lanes,
            resources,
            refinement.request,
            &refinement.context,
            &mut refinement_reply,
            RepositoryIndexIntent::SemanticRefinement {
                structural_generation: refinement.structural_generation,
            },
            None,
        ));
        assert_eq!(calls.load(AtomicOrdering::Relaxed), 1);
        assert_eq!(
            journal
                .status(operation)
                .expect("structural operation remains queryable")
                .state,
            OperationState::Succeeded
        );
        let semantic_operation = semantic_refinement_operation(operation);
        let semantic_record = journal
            .status(semantic_operation)
            .expect("semantic operation remains queryable");
        assert_eq!(semantic_record.state, OperationState::Failed);
        let semantic_error = semantic_record.error.as_ref().expect("failure is recorded");
        assert_eq!(semantic_error.code(), ErrorCode::ResourceExhausted);
        assert_eq!(
            semantic_error.details().get(&static_detail_key("resource")),
            Some(&PublicValue::Label(static_safe_label(
                "adapter_wall_time_ms"
            )))
        );
        assert_eq!(
            semantic_error
                .details()
                .get(&static_detail_key("structural_fallback")),
            Some(&PublicValue::Boolean(true))
        );
        assert_eq!(
            semantic_error.retry_after_ms(),
            Some(u64::from(RETRY_AFTER_MS))
        );
        assert!(semantic_record.progress.completed > 0);
        assert_eq!(semantic_record.progress.total, 6);
        let semantic_context = journal
            .repository_operation_context(semantic_operation)
            .expect("semantic operation observations remain queryable");
        assert!(semantic_context.files_examined > 0);
        assert!(semantic_context.bytes_examined > 0);
        let status = repository_operation_status(
            &handle,
            &metadata,
            &runtime,
            daemon::RepositoryOperationStatusRequest {
                schema_version: Some(schema_version()),
                operation: Some(operation_to_wire(operation)),
                action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                wait_ms: None,
                after_revision: None,
            },
            &context,
        )
        .expect("parent status is available");
        assert_eq!(
            parse_operation(status.semantic_operation.as_ref())
                .expect("durably admitted semantic child is exposed"),
            semantic_refinement_operation(operation)
        );

        drop(handle);
        actor.join().expect("journal actor joins");
        drop(journal);
        let reopened = OperationJournal::open(&paths.operation_journal_path())
            .expect("operation journal reopens");
        let recovered = reopened
            .status(semantic_operation)
            .expect("semantic progress survives restart");
        assert_eq!(recovered.progress, semantic_record.progress);
        let recovered_context = reopened
            .repository_operation_context(semantic_operation)
            .expect("semantic resource observations survive restart");
        assert_eq!(
            (
                recovered_context.files_examined,
                recovered_context.bytes_examined
            ),
            (
                semantic_context.files_examined,
                semantic_context.bytes_examined
            )
        );
    }

    #[test]
    fn full_refinement_queue_preserves_the_published_structural_response() {
        let repository = RepositoryId::from_bytes([31; 16]);
        let generation = GenerationId::from_bytes([32; 20]);
        let operation = OperationId::from_bytes([33; 16]);
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([34; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let (refinement, receiver) = mpsc::sync_channel(1);
        let (occupied_admitted, _occupied_admission) = mpsc::sync_channel(1);
        refinement
            .try_send(SemanticRefinementCommand {
                request: daemon::RepositoryIndexRequest::default(),
                context: context.clone(),
                operation: OperationId::from_bytes([35; 16]),
                repository,
                structural_generation: generation,
                admitted: occupied_admitted,
            })
            .expect("refinement queue is occupied");
        let response = daemon::RepositoryIndexResponse {
            repository: Some(repository_to_wire(repository)),
            operation: Some(operation_to_wire(operation)),
            state: daemon::OperationState::Succeeded as i32,
            published_generation: Some(generation_to_wire(generation)),
            mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            ..daemon::RepositoryIndexResponse::default()
        };
        let semantic_refinements = Arc::new(Mutex::new(BTreeMap::new()));
        let mut reply = None;

        let completed = complete_auto_structural_index(
            "repository",
            operation,
            &context,
            &mut reply,
            response.clone(),
            &semantic_refinements,
            &refinement,
        )
        .expect("queue pressure cannot replace structural success");

        assert_eq!(
            completed.published_generation,
            response.published_generation
        );
        assert!(completed.semantic_operation.is_none());
        assert!(
            semantic_refinements
                .lock()
                .expect("refinement metadata locks")
                .is_empty()
        );
        drop(receiver);
    }

    #[test]
    fn receipt_fallback_preserves_the_rust_analysis_tier() {
        let receipt = FirstSliceIndexReceipt {
            repository: RepositoryId::from_bytes([1; 16]),
            generation: GenerationId::from_bytes([2; 20]),
            parent: None,
            discovered_inputs: 2,
            visited_entries: 2,
            excluded_inputs: 0,
            oversized_inputs: 0,
            indexed_files: 1,
            entities: 1,
            lexical_documents: 1,
            oracle_allocated_bytes: 1,
            estimated_disk_bytes: 1,
            diagnostics: Vec::new(),
            elapsed_micros: 1,
        };

        assert_eq!(
            aggregate_coverage(&[], &receipt),
            (AnalysisTier::TierB, CoverageStatus::Bounded, 1)
        );
    }

    #[test]
    fn catalog_integrity_failures_keep_stable_public_recovery() {
        for (service, code) in [
            (FirstSliceError::CatalogCorrupt, ErrorCode::IndexCorrupt),
            (
                FirstSliceError::CatalogMigrationRequired,
                ErrorCode::MigrationRequired,
            ),
        ] {
            let error = service_error(service);
            assert_eq!(error.code(), code);
            assert_eq!(error.next_actions(), &[NextAction::RebuildRepository]);
            assert!(!error.retryable());
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_marker_reconciles_the_journal_after_publication_crash() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn crash_safe_answer() -> u32 {\n    42\n}\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let operation = OperationId::from_bytes([79; 16]);
        let receipt = {
            let journal = OperationJournal::open(&paths.operation_journal_path())
                .expect("persistent journal opens");
            journal
                .submit(
                    OperationSubmission::new(
                        operation,
                        OperationKind::RepositoryIndex,
                        PlanHash::from_bytes([79; 32]),
                        ClientInstanceId::new([79; 16]).expect("client identity is valid"),
                        true,
                        None,
                        None,
                    )
                    .expect("submission is valid"),
                )
                .expect("operation submits");
            journal
                .start_execution(operation)
                .expect("operation starts");
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let prepared = service
                .prepare_rust_fixture(fixture.path(), &cancellation)
                .expect("generation prepares");
            let staged = service
                .stage_prepared(prepared, &cancellation)
                .expect("generation stages");
            let authorized = journal
                .authorize_repository_publication(operation)
                .expect("publication is authorized");
            assert_eq!(authorized.state, OperationState::Running);
            assert_eq!(authorized.stage, OperationStage::Cleanup);
            service
                .commit_staged_for_operation(
                    staged,
                    FirstSliceOperationContext {
                        operation,
                        started_unix_ms: 79,
                        provider: FirstSliceIndexProvider::TreeSitter,
                    },
                )
                .expect("durable marker commits before simulated crash")
        };

        let journal = Arc::new(
            OperationJournal::open(&paths.operation_journal_path())
                .expect("persistent journal reopens"),
        );
        let interrupted = journal.status(operation).expect("recovered status loads");
        assert_eq!(interrupted.state, OperationState::Interrupted);
        assert_eq!(interrupted.stage, OperationStage::Cleanup);
        assert_eq!(
            interrupted.recovery_class,
            RecoveryClass::InterruptedByRestart
        );

        let restored = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
            .expect("durable service restores");
        let publications = restored
            .durable_operation_publications()
            .collect::<Vec<_>>();
        assert_eq!(publications.len(), 1);
        assert_eq!(publications[0].operation, operation);
        assert_eq!(publications[0].receipt, receipt);
        drop(restored);

        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        OBSERVED_STARTUP_SIGNAL.store(0, AtomicOrdering::SeqCst);
        let (daemon, workers) = runtime
            .block_on(FirstSliceDaemon::start_durable(
                actor.handle(),
                paths.state_dir(),
                Arc::new(DaemonState::starting()),
                Some(record_startup_signal),
            ))
            .expect("daemon reconciles the durable marker");
        assert_eq!(
            OBSERVED_STARTUP_SIGNAL.load(AtomicOrdering::SeqCst),
            CoordinatedStartupSignal::ActiveGenerationRestore.to_byte()
        );
        let first_status_started = Instant::now();
        let first_status = execute_with_timeout(
            &daemon,
            FirstSliceIpcRequest::RepositoryStatus(status_request(
                receipt.repository,
                Some(receipt.generation),
            )),
        );
        assert!(
            first_status_started.elapsed() < Duration::from_secs(1),
            "generation recovery must not hide inside the first status request"
        );
        match first_status {
            Ok(FirstSliceIpcResponse::RepositoryStatus(status)) => {
                assert_eq!(
                    status
                        .resolved_generation
                        .as_ref()
                        .map(|id| id.value.as_slice()),
                    Some(receipt.generation.as_bytes().as_slice())
                );
            }
            Err(error) => {
                assert_eq!(error.code(), ErrorCode::Busy);
                assert_eq!(error.retry_after_ms(), Some(u64::from(RETRY_AFTER_MS)));
            }
            Ok(_) => panic!("repository status response expected"),
        }
        let status_deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            let status = execute_with_timeout(
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
            let status = match status {
                Ok(status) => status,
                Err(error)
                    if matches!(
                        error.code(),
                        ErrorCode::UnsupportedCapability | ErrorCode::NotFound
                    ) && Instant::now() < status_deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("durable operation status failed: {error:?}"),
            };
            let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
                panic!("repository operation status response expected");
            };
            if status.operation.as_ref().is_some_and(|operation| {
                operation.state == daemon::OperationState::Succeeded as i32
            }) {
                break status;
            }
            assert!(
                Instant::now() < status_deadline,
                "durable operation reconciliation timed out"
            );
            thread::sleep(Duration::from_millis(10));
        };
        let reconciled = journal.status(operation).expect("reconciled status loads");
        assert_eq!(reconciled.state, OperationState::Succeeded);
        assert_eq!(reconciled.stage, OperationStage::Cleanup);
        assert_eq!(reconciled.recovery_class, RecoveryClass::NotApplicable);

        assert!(status.published_generation.is_some());
        assert_eq!(
            status
                .operation
                .as_ref()
                .expect("operation status exists")
                .state,
            daemon::OperationState::Succeeded as i32
        );

        let located = execute(
            &daemon,
            FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest {
                schema_version: Some(schema_version()),
                repository: Some(repository_to_wire(receipt.repository)),
                generation: Some(daemon::GenerationSelector {
                    selector: Some(daemon::generation_selector::Selector::Generation(
                        generation_to_wire(receipt.generation),
                    )),
                }),
                query: "crash_safe_answer".to_owned(),
                mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
                maximum_results: 8,
                page_offset: 0,
                languages: Vec::new(),
            }),
        );
        let FirstSliceIpcResponse::CodeLocate(located) = located else {
            panic!("code locate response expected");
        };
        assert_eq!(located.hits.len(), 1);

        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_unpublished_repository_operation_remains_queryable_after_restart() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn restarted_answer() -> u32 { 42 }\n",
        )
        .expect("fixture source writes");
        let admission = {
            let cancellation =
                Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
            FirstSliceService::new_durable(
                DEFAULT_GENERATION_RETENTION,
                paths.state_dir(),
                &cancellation,
            )
            .expect("durable service initializes")
            .admit_repository(fixture.path(), &cancellation)
            .expect("repository registration is reserved")
        };
        let operation = OperationId::from_bytes([78; 16]);
        let repository = admission.repository;
        {
            let journal = OperationJournal::open(&paths.operation_journal_path())
                .expect("persistent journal opens");
            let submission = OperationSubmission::new(
                operation,
                OperationKind::RepositoryIndex,
                PlanHash::from_bytes([78; 32]),
                ClientInstanceId::from_bytes([78; 16]),
                true,
                None,
                None,
            )
            .expect("operation submission is valid")
            .with_repository_context(
                RepositoryOperationSubmission::new(
                    repository,
                    None,
                    1_700_000_000_000,
                    128 * 1024 * 1024,
                    RepositoryOperationMode::Structural,
                )
                .expect("repository context is valid")
                .with_root_identity(*admission.root_identity.as_bytes()),
            )
            .expect("repository context attaches");
            journal.submit(submission).expect("operation submits");
            journal
                .start_execution(operation)
                .expect("operation starts");
            journal
                .update_progress(operation, Progress::new(2, 6).expect("progress is valid"))
                .expect("progress persists");
            journal
                .update_repository_observation(operation, 12, 4_096)
                .expect("input observations persist");
            journal
                .update_resources(operation, 64 * 1024 * 1024, 8_192)
                .expect("resource observations persist");
        }

        let journal = Arc::new(
            OperationJournal::open(&paths.operation_journal_path())
                .expect("persistent journal reopens"),
        );
        let interrupted = journal.status(operation).expect("recovered status loads");
        assert_eq!(interrupted.state, OperationState::Interrupted);
        assert_eq!(
            interrupted.recovery_class,
            RecoveryClass::InterruptedByRestart
        );
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let (daemon, workers) = runtime
            .block_on(FirstSliceDaemon::start_durable(
                actor.handle(),
                paths.state_dir(),
                Arc::new(DaemonState::starting()),
                None,
            ))
            .expect("daemon restores unpublished operation context");

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
            panic!("repository operation status response expected");
        };
        let visible = status.operation.expect("operation remains visible");
        assert_eq!(visible.state, daemon::OperationState::Interrupted as i32);
        assert_eq!(status.started_unix_ms, 1_700_000_000_000);
        assert_eq!(status.peak_rss_bytes, 64 * 1024 * 1024);
        assert_eq!(status.written_bytes, 8_192);
        assert_eq!(status.files_examined, 12);
        assert_eq!(status.bytes_examined, 4_096);
        assert_eq!(status.index_stage, "analysis");
        assert!(status.published_generation.is_none());
        assert!(status.semantic_operation.is_none());

        let catalog = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryCatalogPage(catalog_request(
                20,
                true,
                vec!["indexing"],
                None,
                None,
            )),
        );
        let FirstSliceIpcResponse::RepositoryCatalogPage(catalog) = catalog else {
            panic!("repository catalog response expected");
        };
        let [cataloged] = catalog.repositories.as_slice() else {
            panic!("one restored unpublished registration is expected");
        };
        assert_eq!(
            parse_repository(cataloged.repository.as_ref()).expect("repository identity decodes"),
            repository
        );
        assert_eq!(cataloged.state, "indexing");
        assert!(cataloged.active_generation.is_none());

        let pending_status = execute_with_timeout(
            &daemon,
            FirstSliceIpcRequest::RepositoryStatus(status_request(repository, None)),
        )
        .expect_err("unpublished repository has no generation status yet");
        assert_eq!(pending_status.code(), ErrorCode::StaleGeneration);
        assert_eq!(pending_status.repository(), Some(repository));
        assert_eq!(
            pending_status.next_actions(),
            &[NextAction::InspectOperation, NextAction::Retry]
        );

        let resumed = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(OperationId::from_bytes([76; 16]))),
                detached: false,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
        );
        let FirstSliceIpcResponse::RepositoryIndex(resumed) = resumed else {
            panic!("repository index response expected");
        };
        assert_eq!(
            parse_repository(resumed.repository.as_ref())
                .expect("restored repository registration maps"),
            repository
        );
        assert!(resumed.published_generation.is_some());

        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_generation_survives_pruned_operation_history() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("fixture source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn retained_answer() -> u32 {\n    42\n}\n",
        )
        .expect("fixture source writes");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        );
        let operation = OperationId::from_bytes([80; 16]);
        let receipt = {
            let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation)
                .expect("durable service initializes");
            let prepared = service
                .prepare_rust_fixture(fixture.path(), &cancellation)
                .expect("generation prepares");
            let staged = service
                .stage_prepared(prepared, &cancellation)
                .expect("generation stages");
            service
                .commit_staged_for_operation(
                    staged,
                    FirstSliceOperationContext {
                        operation,
                        started_unix_ms: 80,
                        provider: FirstSliceIndexProvider::TreeSitter,
                    },
                )
                .expect("durable marker commits without retained journal history")
        };

        let journal = Arc::new(
            OperationJournal::open(&paths.operation_journal_path())
                .expect("empty persistent journal opens"),
        );
        assert!(matches!(
            journal.status(operation),
            Err(OperationError::NotFound)
        ));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let (daemon, workers) = runtime
            .block_on(FirstSliceDaemon::start_durable(
                actor.handle(),
                paths.state_dir(),
                Arc::new(DaemonState::starting()),
                None,
            ))
            .expect("daemon skips pruned operation history");

        let located = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::CodeLocate(daemon::CodeLocateRequest {
                    schema_version: Some(schema_version()),
                    repository: Some(repository_to_wire(receipt.repository)),
                    generation: Some(daemon::GenerationSelector {
                        selector: Some(daemon::generation_selector::Selector::Generation(
                            generation_to_wire(receipt.generation),
                        )),
                    }),
                    query: "retained_answer".to_owned(),
                    mode: daemon::FirstSliceLocateMode::FirstSliceLocateExact as i32,
                    maximum_results: 8,
                    page_offset: 0,
                    languages: Vec::new(),
                })
            },
            "last-good generation becomes readable after background recovery",
        );
        let FirstSliceIpcResponse::CodeLocate(located) = located else {
            panic!("code locate response expected");
        };
        assert_eq!(located.hits.len(), 1);

        let mut request = status_request(receipt.repository, None);
        request.include_operations = true;
        let status = execute(&daemon, FirstSliceIpcRequest::RepositoryStatus(request));
        let FirstSliceIpcResponse::RepositoryStatus(status) = status else {
            panic!("repository status response expected");
        };
        assert!(
            status.operations.is_empty(),
            "pruned operation history must not be reconstructed"
        );

        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn source_reference_merging_is_transitive_and_permutation_invariant() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let hash = ContentHash::from_bytes([4; 32]);
        let reference = |start, end| {
            SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, start, end).expect("fixture span is valid"),
                hash,
                None,
            )
        };
        let left = reference(0, 5);
        let right = reference(10, 15);
        let bridge = reference(4, 11);
        let first = merge_source_references(vec![left.clone(), right.clone(), bridge.clone()])
            .expect("bridge ranges merge");
        let permuted = merge_source_references(vec![right, bridge, left])
            .expect("permuted bridge ranges merge");

        assert_eq!(first, permuted);
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].span().start_byte(), 0);
        assert_eq!(first[0].span().end_byte(), 15);
    }

    #[test]
    fn every_query_limiting_resource_has_a_stable_wire_mapping() {
        let cases = [
            (
                QueryResource::Rows,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitRows,
            ),
            (
                QueryResource::Edges,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitEdges,
            ),
            (
                QueryResource::Results,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitResults,
            ),
            (
                QueryResource::SourceBytes,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitSourceBytes,
            ),
            (
                QueryResource::JsonBytes,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitResponseBytes,
            ),
            (
                QueryResource::Tokens,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitEstimatedTokens,
            ),
            (
                QueryResource::MemoryBytes,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitMemoryBytes,
            ),
            (
                QueryResource::Depth,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitDepth,
            ),
            (
                QueryResource::Paths,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitPaths,
            ),
            (
                QueryResource::Capability,
                daemon::FirstSliceLimitingResourceKind::FirstSliceLimitCapability,
            ),
        ];
        for (resource, expected) in cases {
            assert_eq!(limiting_resource_to_wire(resource), expected);
        }
    }

    #[test]
    fn reduced_budget_changes_locate_plan_and_truncates_results() {
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn budget_alpha() {}\npub fn budget_beta() {}\n",
        )
        .expect("source writes");
        let mut service =
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION).expect("service initializes");
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let indexed = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("fixture indexes");

        let unrestricted = service
            .code_locate_with_budget(
                indexed.generation,
                "budget".to_owned(),
                LocateMode::Prefix,
                8,
                0,
                FirstSliceBudget::new(),
                &cancellation,
            )
            .expect("unrestricted locate succeeds");
        let reduced = service
            .code_locate_with_budget(
                indexed.generation,
                "budget".to_owned(),
                LocateMode::Prefix,
                8,
                0,
                reduced_service_budget(Some(ServiceBudgetReduction {
                    rows: u64::MAX,
                    edges: u64::MAX,
                    results: 1,
                    source_bytes: u64::MAX,
                    json_bytes: u64::MAX,
                    estimated_tokens: u64::MAX,
                    memory_bytes: u64::MAX,
                    duration: Duration::MAX,
                })),
                &cancellation,
            )
            .expect("reduced locate succeeds");

        assert_eq!(unrestricted.data.hits.len(), 2);
        assert_eq!(reduced.plan.estimate.results, 1);
        assert_eq!(reduced.data.hits.len(), 1);
        assert!(reduced.data.truncated);
    }

    #[test]
    fn symbol_explain_partitions_resolved_and_absent_identifiers() {
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn explained_symbol() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let mut service =
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION).expect("service initializes");
        let deadline = Instant::now() + Duration::from_secs(30);
        let cancellation = Cancellation::with_deadline(deadline);
        let indexed = service
            .index_rust_fixture(fixture.path(), &cancellation)
            .expect("fixture indexes");
        let located = service
            .code_locate(
                indexed.generation,
                "explained_symbol".to_owned(),
                LocateMode::Exact,
                1,
                0,
                &cancellation,
            )
            .expect("fixture symbol locates");
        let resolved = located.data.hits[0].symbol;
        let absent = SymbolId::from_bytes([0xff; 20]);
        assert_ne!(resolved, absent);
        let mut requested = [resolved, absent];
        requested.sort_unstable();
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation,
            deadline,
            effective_budget: None,
            index_admission: None,
        };

        let response = symbol_explain(
            &service,
            daemon::SymbolExplainRequest {
                schema_version: Some(schema_version()),
                repository: Some(repository_to_wire(indexed.repository)),
                generation: Some(daemon::GenerationSelector {
                    selector: Some(daemon::generation_selector::Selector::Generation(
                        generation_to_wire(indexed.generation),
                    )),
                }),
                symbols: requested.iter().copied().map(symbol_to_wire).collect(),
            },
            &context,
        )
        .expect("mixed explanation succeeds");

        assert!(!response.truncated);
        assert_eq!(response.symbols.len(), 1);
        assert_eq!(
            parse_symbol(response.symbols[0].symbol.as_ref()).expect("resolved symbol parses"),
            resolved
        );
        assert_eq!(
            response
                .unresolved_symbols
                .iter()
                .map(|symbol| parse_symbol(Some(symbol)).expect("unresolved symbol parses"))
                .collect::<Vec<_>>(),
            [absent]
        );
        assert_eq!(
            daemon::FirstSliceCompletenessState::try_from(
                response.completeness.expect("completeness exists").state
            )
            .expect("completeness state is valid"),
            daemon::FirstSliceCompletenessState::FirstSliceCompletenessComplete
        );
    }

    #[test]
    fn optional_traversal_limits_only_reduce_request_dimensions() {
        let default_depth = u8::try_from(DEFAULT_FLOW_DEPTH).expect("default depth converts");
        let default_paths = usize::try_from(DEFAULT_FLOW_PATHS).expect("default paths convert");
        assert_eq!(
            reduce_optional_u8(default_depth, Some(1)).expect("validated depth converts"),
            1
        );
        assert_eq!(
            reduce_optional_u8(default_depth, Some(4)).expect("validated depth converts"),
            default_depth
        );
        assert_eq!(
            reduce_optional_usize(default_paths, Some(2)).expect("validated path limit converts"),
            2
        );
        assert_eq!(
            reduce_optional_usize(default_paths, Some(20)).expect("validated path limit converts"),
            default_paths
        );
    }

    #[test]
    fn advanced_work_limit_uses_the_effective_edge_budget() {
        let reduced = reduced_service_budget(Some(ServiceBudgetReduction {
            rows: u64::MAX,
            edges: 7,
            results: u64::MAX,
            source_bytes: u64::MAX,
            json_bytes: u64::MAX,
            estimated_tokens: u64::MAX,
            memory_bytes: u64::MAX,
            duration: Duration::MAX,
        }));
        assert_eq!(
            advanced_edge_work_limit(reduced).expect("edge budget converts"),
            7
        );
        assert_eq!(
            advanced_edge_work_limit(FirstSliceBudget::new()).expect("default budget converts"),
            ADVANCED_MAX_TRAVERSAL
        );
    }

    fn catalog_request(
        page_size: u32,
        states_present: bool,
        states: Vec<&str>,
        snapshot: Option<daemon::RepositoryCatalogSnapshotId>,
        after: Option<daemon::RepositoryCatalogSortKey>,
    ) -> daemon::RepositoryCatalogPageRequest {
        daemon::RepositoryCatalogPageRequest {
            page_size,
            normalized_query: None,
            states: states.into_iter().map(str::to_owned).collect(),
            snapshot,
            after,
            sort_version: u32::from(CATALOG_SORT_VERSION),
            states_present,
        }
    }

    fn indexed_catalog(names: &[&str]) -> (FirstSliceService, TempDir) {
        let root = TempDir::new().expect("catalog fixture root exists");
        let mut service =
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION).expect("service initializes");
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        for name in names {
            let repository = root.path().join(name);
            fs::create_dir_all(repository.join("src")).expect("repository directory exists");
            fs::write(
                repository.join("src/lib.rs"),
                format!("pub fn {name}_answer() -> u32 {{ 42 }}\n"),
            )
            .expect("repository source writes");
            service
                .index_rust_fixture(&repository, &cancellation)
                .expect("catalog fixture indexes");
        }
        (service, root)
    }

    fn status_request(
        repository: RepositoryId,
        generation: Option<GenerationId>,
    ) -> daemon::RepositoryStatusRequest {
        let selector = generation.map_or(
            daemon::generation_selector::Selector::Active(true),
            |generation| {
                daemon::generation_selector::Selector::Generation(generation_to_wire(generation))
            },
        );
        daemon::RepositoryStatusRequest {
            repository: Some(repository_to_wire(repository)),
            generation: Some(daemon::GenerationSelector {
                selector: Some(selector),
            }),
            coverage_detail: "summary".to_owned(),
            include_operations: false,
            require_freshness: "none".to_owned(),
        }
    }

    fn status_response(
        service: &FirstSliceService,
        request: daemon::RepositoryStatusRequest,
    ) -> Result<daemon::RepositoryStatusResponse, PublicError> {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(journal, 4, 4).expect("journal actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(4));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let response = repository_status(service, &handle, &metadata, &runtime, request, &context);
        drop(handle);
        actor.join().expect("journal actor joins");
        response
    }

    #[test]
    fn repository_status_returns_exact_and_actionable_generation_results() {
        let (service, _root) = indexed_catalog(&["alpha", "beta"]);
        let repositories = service.list_repositories();
        let alpha = &repositories[0];
        let beta = &repositories[1];

        let exact = status_response(
            &service,
            status_request(alpha.repository, Some(alpha.active_generation)),
        )
        .expect("retained exact generation reports status");
        assert_eq!(
            parse_generation(exact.resolved_generation.as_ref()).expect("resolved generation maps"),
            alpha.active_generation
        );
        assert_eq!(
            parse_generation(exact.active_generation.as_ref()).expect("active generation maps"),
            alpha.active_generation
        );

        let wrong_repository = status_response(
            &service,
            status_request(alpha.repository, Some(beta.active_generation)),
        )
        .expect_err("another repository generation is rejected");
        assert_eq!(wrong_repository.code(), ErrorCode::Conflict);

        let missing_generation = GenerationId::from_bytes([0x7f; 20]);
        let missing = status_response(
            &service,
            status_request(alpha.repository, Some(missing_generation)),
        )
        .expect_err("missing exact generation is rejected");
        assert_eq!(missing.code(), ErrorCode::StaleGeneration);
        assert_eq!(missing.repository(), Some(alpha.repository));
        assert_eq!(missing.generation(), Some(missing_generation));
        assert_eq!(missing.next_actions(), &[NextAction::RestartEnumeration]);
    }

    #[test]
    fn repository_status_enforces_requested_freshness() {
        let root = TempDir::new().expect("fixture root exists");
        let repository_root = root.path().join("alpha");
        fs::create_dir_all(repository_root.join("src"))
            .expect("repository source directory exists");
        fs::write(
            repository_root.join("src/lib.rs"),
            "pub fn answer() -> u32 { 1 }\n",
        )
        .expect("initial source writes");
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let mut service =
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION).expect("service initializes");
        let first = service
            .index_rust_fixture(&repository_root, &cancellation)
            .expect("initial generation publishes");
        fs::write(
            repository_root.join("src/lib.rs"),
            "pub fn answer() -> u32 { 2 }\n",
        )
        .expect("updated source writes");
        let second = service
            .index_rust_fixture(&repository_root, &cancellation)
            .expect("successor generation publishes");

        let mut stale_request = status_request(first.repository, Some(first.generation));
        stale_request.require_freshness = "structural".to_owned();
        let stale = status_response(&service, stale_request)
            .expect_err("retained generation does not satisfy structural freshness");
        assert_eq!(stale.code(), ErrorCode::StaleGeneration);
        assert_eq!(stale.repository(), Some(first.repository));
        assert_eq!(stale.generation(), Some(first.generation));
        assert_eq!(stale.next_actions(), &[NextAction::RebuildRepository]);

        let mut active_request = status_request(first.repository, None);
        active_request.require_freshness = "semantic".to_owned();
        let active =
            status_response(&service, active_request).expect("active generation remains fresh");
        assert_eq!(
            parse_generation(active.resolved_generation.as_ref())
                .expect("resolved active generation maps"),
            second.generation
        );
        assert_eq!(active.semantic_freshness, "current");
    }

    #[test]
    fn repository_status_projects_bound_current_and_recent_operations() {
        let (service, _root) = indexed_catalog(&["alpha"]);
        let repository = service.list_repositories()[0].repository;
        let owner = ClientInstanceId::from_bytes([7; 16]);
        let running = OperationId::from_bytes([70; 16]);
        let failed = OperationId::from_bytes([71; 16]);
        let cancelled = OperationId::from_bytes([72; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));

        journal
            .submit(repository_submission(running, 7))
            .expect("running operation submits");
        journal
            .start_execution(running)
            .expect("running operation starts");
        journal
            .submit(repository_submission(failed, 7))
            .expect("failed operation submits");
        journal
            .start_execution(failed)
            .expect("failed operation starts");
        journal
            .update_stage(failed, OperationStage::Cleanup)
            .expect("failed operation enters cleanup");
        let stored_error =
            PublicError::builder(ErrorCode::AdapterFailed, "repository analysis failed")
                .operation(failed)
                .build()
                .expect("failure is valid");
        journal
            .transition(failed, OperationState::Failed, Some(&stored_error))
            .expect("failed operation terminates");
        journal
            .submit(repository_submission(cancelled, 7))
            .expect("cancelled operation submits");
        journal
            .request_cancellation(cancelled, CancellationAuthority::Client(owner))
            .expect("queued operation cancels");

        let mut metadata = OperationMetadataSet::new(8);
        metadata
            .reserve(running, 10, Some(repository))
            .expect("running metadata reserves");
        metadata
            .reserve(failed, 20, Some(repository))
            .expect("failed metadata reserves");
        metadata.mark_terminal(failed);
        metadata
            .reserve(cancelled, 30, Some(repository))
            .expect("cancelled metadata reserves");
        metadata.mark_terminal(cancelled);
        let metadata = Mutex::new(metadata);
        let actor = JournalActor::start(Arc::clone(&journal), 8, 8).expect("journal actor starts");
        let handle = actor.handle();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: owner,
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let mut request = status_request(repository, None);
        request.include_operations = true;
        let response = repository_status(&service, &handle, &metadata, &runtime, request, &context)
            .expect("status projects operations");

        assert_eq!(response.state, "indexing");
        assert_eq!(response.operations.len(), 3);
        assert_eq!(
            parse_operation(response.operations[0].operation.as_ref())
                .expect("cancelled operation identity maps"),
            cancelled
        );
        assert_eq!(
            daemon::OperationState::try_from(response.operations[0].state)
                .expect("cancelled operation state maps"),
            daemon::OperationState::Cancelled
        );
        assert_eq!(
            daemon::OperationState::try_from(response.operations[1].state)
                .expect("failed operation state maps"),
            daemon::OperationState::Failed
        );
        assert_eq!(
            daemon::OperationState::try_from(response.operations[2].state)
                .expect("running operation state maps"),
            daemon::OperationState::Running
        );
        assert!(
            response
                .operations
                .iter()
                .all(|operation| operation.owned_by_client)
        );

        drop(handle);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn repository_status_reuses_verified_terminal_operation_snapshots() {
        let (service, _root) = indexed_catalog(&["alpha"]);
        let repository = service.list_repositories()[0].repository;
        let operation = OperationId::from_bytes([74; 16]);
        let completed_journal =
            OperationJournal::open_in_memory().expect("completed journal opens");
        completed_journal
            .submit(repository_submission(operation, 7))
            .expect("operation submits");
        completed_journal
            .start_execution(operation)
            .expect("operation starts");
        let record = completed_journal
            .complete_repository_publication(operation)
            .expect("operation completes");
        let mut metadata = OperationMetadataSet::new(4);
        metadata
            .reserve(operation, 10, Some(repository))
            .expect("metadata reserves");
        metadata
            .records
            .get_mut(&operation)
            .expect("metadata exists")
            .publication = PublicationState::Committed;
        metadata.observe_terminal(&record);
        let metadata = Mutex::new(metadata);

        // The actor deliberately has no matching record. A journal lookup would
        // omit the operation, proving the terminal snapshot is the status source.
        let empty_journal =
            Arc::new(OperationJournal::open_in_memory().expect("empty journal opens"));
        let actor = JournalActor::start(empty_journal, 4, 4).expect("empty journal actor starts");
        let handle = actor.handle();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let mut request = status_request(repository, None);
        request.include_operations = true;

        let response = repository_status(&service, &handle, &metadata, &runtime, request, &context)
            .expect("terminal snapshot reports status");

        assert_eq!(response.operations.len(), 1);
        assert_eq!(
            daemon::OperationState::try_from(response.operations[0].state)
                .expect("operation state maps"),
            daemon::OperationState::Succeeded
        );
        drop(handle);
        actor.join().expect("empty journal actor joins");
    }

    #[test]
    fn repository_status_refreshes_a_previously_running_operation() {
        let (service, _root) = indexed_catalog(&["alpha"]);
        let repository = service.list_repositories()[0].repository;
        let operation = OperationId::from_bytes([75; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        journal
            .submit(repository_submission(operation, 7))
            .expect("operation submits");
        journal
            .start_execution(operation)
            .expect("operation starts");
        let mut metadata = OperationMetadataSet::new(4);
        metadata
            .reserve(operation, 10, Some(repository))
            .expect("metadata reserves");
        let metadata = Mutex::new(metadata);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("journal actor starts");
        let handle = actor.handle();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let mut request = status_request(repository, None);
        request.include_operations = true;

        let running = repository_status(
            &service,
            &handle,
            &metadata,
            &runtime,
            request.clone(),
            &context,
        )
        .expect("running operation reports status");
        assert_eq!(running.state, "indexing");

        journal
            .complete_repository_publication(operation)
            .expect("operation completes");
        let completed =
            repository_status(&service, &handle, &metadata, &runtime, request, &context)
                .expect("completed operation refreshes status");

        assert_eq!(completed.state, "ready");
        assert_eq!(completed.operations.len(), 1);
        assert_eq!(
            daemon::OperationState::try_from(completed.operations[0].state)
                .expect("operation state maps"),
            daemon::OperationState::Succeeded
        );
        drop(handle);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn failed_publication_keeps_a_verified_active_generation_ready() {
        let (service, _root) = indexed_catalog(&["alpha"]);
        let repository = service.list_repositories()[0].repository;
        let operation = OperationId::from_bytes([73; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        journal
            .submit(repository_submission(operation, 7))
            .expect("operation submits");
        journal
            .start_execution(operation)
            .expect("operation starts");
        journal
            .complete_repository_publication(operation)
            .expect("journal publication completes");
        let mut metadata = OperationMetadataSet::new(4);
        metadata
            .reserve(operation, 10, Some(repository))
            .expect("metadata reserves");
        metadata.fail_closed(operation);
        let metadata = Mutex::new(metadata);
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("journal actor starts");
        let handle = actor.handle();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let mut request = status_request(repository, None);
        request.include_operations = true;
        let response = repository_status(&service, &handle, &metadata, &runtime, request, &context)
            .expect("healthy active generation remains reportable");

        assert_eq!(response.state, "ready");
        assert_eq!(response.operations.len(), 1);
        assert_eq!(
            daemon::OperationState::try_from(response.operations[0].state)
                .expect("visible operation state maps"),
            daemon::OperationState::Failed
        );

        drop(handle);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn pre_completion_failure_is_terminal_and_keeps_active_generation_ready() {
        let root = TempDir::new().expect("fixture root exists");
        let repository_root = root.path().join("alpha");
        fs::create_dir_all(repository_root.join("src"))
            .expect("repository source directory exists");
        fs::write(
            repository_root.join("src/lib.rs"),
            "pub fn answer() -> u32 { 1 }\n",
        )
        .expect("initial source writes");
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        let mut service =
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION).expect("service initializes");
        let active = service
            .index_rust_fixture(&repository_root, &cancellation)
            .expect("active generation publishes");
        fs::write(
            repository_root.join("src/lib.rs"),
            "pub fn answer() -> u32 { 2 }\n",
        )
        .expect("updated source writes");
        let service = Arc::new(RwLock::new(service));

        let operation = OperationId::from_bytes([74; 16]);
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("journal actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(4));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(5);
        let context = FirstSliceIpcContext {
            client_instance_id: ClientInstanceId::from_bytes([7; 16]),
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
            index_admission: None,
        };
        let (reached_sender, _reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        drop(release_sender);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::BeforeCompletion,
            fail_commit: AtomicBool::new(false),
            armed: AtomicBool::new(true),
            reached: reached_sender,
            release: release_receiver,
        };
        let mut reply = None;
        let index_serialization = Arc::new(Mutex::new(()));
        let semantic_refinements = Arc::new(Mutex::new(BTreeMap::new()));
        let (refinement, _refinement_receiver) = mpsc::sync_channel(1);
        let lanes = FirstSliceServiceLanes {
            service: Arc::clone(&service),
            index_serialization,
            semantic_refinements,
            refinement,
            recovery_ready: Arc::new(AtomicBool::new(true)),
            support_state: None,
        };
        let error = repository_index(
            &lanes,
            ServiceRequestResources {
                journal: &handle,
                metadata: &metadata,
                runtime: &runtime,
                catalog_epoch: Instant::now(),
                publication_hook: Some(&hook),
            },
            daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: repository_root.to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(operation)),
                detached: true,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            },
            &context,
            &mut reply,
        )
        .expect_err("closed publication boundary fails the update");
        assert_eq!(error.code(), ErrorCode::Internal);
        assert_eq!(
            journal
                .status(operation)
                .expect("durable operation remains inspectable")
                .state,
            OperationState::Failed
        );

        let mut request = status_request(active.repository, None);
        request.include_operations = true;
        let response = {
            let service = read_service(&service).expect("service read lock is available");
            repository_status(&service, &handle, &metadata, &runtime, request, &context)
                .expect("healthy active generation remains reportable")
        };
        assert_eq!(response.state, "ready");
        assert_eq!(response.operations.len(), 1);
        assert_eq!(
            daemon::OperationState::try_from(response.operations[0].state)
                .expect("visible operation state maps"),
            daemon::OperationState::Failed
        );
        assert_eq!(
            parse_generation(response.resolved_generation.as_ref())
                .expect("active generation maps"),
            active.generation
        );

        drop(handle);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn repository_catalog_page_returns_an_empty_authoritative_snapshot() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(journal, 4, 4).expect("journal actor starts");
        let (host, workers) = FirstSliceDaemon::start(actor.handle()).expect("host starts");
        let response = execute(
            &host,
            FirstSliceIpcRequest::RepositoryCatalogPage(catalog_request(
                20,
                false,
                Vec::new(),
                None,
                None,
            )),
        );
        let FirstSliceIpcResponse::RepositoryCatalogPage(response) = response else {
            panic!("catalog page response expected");
        };

        assert!(response.repositories.is_empty());
        assert_eq!(
            response
                .snapshot
                .as_ref()
                .expect("snapshot identity is returned")
                .value
                .len(),
            32
        );
        assert_eq!(response.total_count, Some(0));
        assert!(!response.truncated);
        assert!(response.next_after.is_none());
        assert_eq!(response.sort_version, u32::from(CATALOG_SORT_VERSION));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        drop(host);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn repository_catalog_continuation_is_stable_across_catalog_mutation() {
        let (mut service, root) = indexed_catalog(&["alpha", "beta"]);
        let epoch = Instant::now();
        let first = repository_catalog_page(
            &service,
            catalog_request(1, false, Vec::new(), None, None),
            epoch,
        )
        .expect("first page succeeds");
        assert_eq!(first.total_count, Some(2));
        assert!(first.truncated);
        assert_eq!(first.repositories.len(), 1);
        let first_entry = &first.repositories[0];
        assert_eq!(first_entry.display_name, "alpha");
        assert!(first_entry.repository.is_some());
        assert!(first_entry.active_generation.is_some());
        assert_eq!(first_entry.alias, None);
        assert_eq!(first_entry.generation_count, 1);
        assert_eq!(first_entry.state, "ready");
        assert_eq!(first_entry.languages, ["rust"]);
        assert_eq!(first_entry.structural_freshness, "current");
        assert_eq!(first_entry.semantic_freshness, "current");
        assert_eq!(first_entry.coverage.len(), 1);
        assert_eq!(first_entry.coverage[0].language, "rust");
        assert_eq!(first_entry.coverage[0].tier, "tier_b");
        assert_eq!(first_entry.coverage[0].status, "complete");
        assert_eq!(first_entry.coverage[0].discovered_files, 1);
        assert_eq!(first_entry.coverage[0].indexed_files, 1);

        let gamma = root.path().join("gamma");
        fs::create_dir_all(gamma.join("src")).expect("new repository directory exists");
        fs::write(
            gamma.join("src/lib.rs"),
            "pub fn gamma_answer() -> u32 { 42 }\n",
        )
        .expect("new repository source writes");
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(30));
        service
            .index_rust_fixture(&gamma, &cancellation)
            .expect("new repository indexes");

        let continuation = repository_catalog_page(
            &service,
            catalog_request(
                1,
                false,
                Vec::new(),
                first.snapshot.clone(),
                first.next_after.clone(),
            ),
            epoch,
        )
        .expect("continuation succeeds");
        assert_eq!(continuation.total_count, Some(2));
        assert_eq!(continuation.repositories.len(), 1);
        assert_eq!(continuation.repositories[0].display_name, "beta");
        assert!(!continuation.truncated);
        assert!(continuation.next_after.is_none());

        let refreshed = repository_catalog_page(
            &service,
            catalog_request(20, false, Vec::new(), None, None),
            epoch,
        )
        .expect("fresh enumeration succeeds");
        assert_eq!(refreshed.total_count, Some(3));
        assert_eq!(refreshed.repositories.len(), 3);
        assert_eq!(refreshed.repositories[2].display_name, "gamma");
    }

    #[test]
    fn repository_catalog_preserves_absent_and_empty_state_filters() {
        let (service, _root) = indexed_catalog(&["alpha"]);
        let epoch = Instant::now();

        let all = repository_catalog_page(
            &service,
            catalog_request(20, false, Vec::new(), None, None),
            epoch,
        )
        .expect("absent state filter succeeds");
        assert_eq!(all.total_count, Some(1));

        let none = repository_catalog_page(
            &service,
            catalog_request(20, true, Vec::new(), None, None),
            epoch,
        )
        .expect("present empty state filter succeeds");
        assert_eq!(none.total_count, Some(0));

        let ready = repository_catalog_page(
            &service,
            catalog_request(20, true, vec!["ready"], None, None),
            epoch,
        )
        .expect("ready state filter succeeds");
        assert_eq!(ready.total_count, Some(1));

        let inconsistent = repository_catalog_page(
            &service,
            catalog_request(20, false, vec!["ready"], None, None),
            epoch,
        )
        .expect_err("states without presence fail closed");
        assert_eq!(inconsistent.code(), ErrorCode::InvalidArgument);

        let unknown = repository_catalog_page(
            &service,
            catalog_request(20, true, vec!["unknown"], None, None),
            epoch,
        )
        .expect_err("unknown state fails closed");
        assert_eq!(unknown.code(), ErrorCode::InvalidArgument);
    }

    #[test]
    fn repository_catalog_cursor_errors_request_restart_without_leaking_sources() {
        let service =
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION).expect("service initializes");
        let mut request = catalog_request(20, false, Vec::new(), None, None);
        request.sort_version = u32::from(CATALOG_SORT_VERSION) + 1;
        let unsupported = repository_catalog_page(&service, request, Instant::now())
            .expect_err("unsupported sort version fails");
        assert_eq!(unsupported.code(), ErrorCode::InvalidCursor);
        assert_eq!(
            unsupported.message(),
            "pagination cursor is invalid or expired"
        );
        assert_eq!(
            unsupported.next_actions(),
            &[NextAction::RestartEnumeration]
        );
        assert!(!unsupported.message().contains('\\'));
        assert!(!unsupported.message().contains('/'));

        let malformed_snapshot = repository_catalog_page(
            &service,
            catalog_request(
                20,
                false,
                Vec::new(),
                Some(daemon::RepositoryCatalogSnapshotId { value: vec![7; 31] }),
                None,
            ),
            Instant::now(),
        )
        .expect_err("malformed snapshot identity fails");
        assert_eq!(malformed_snapshot.code(), ErrorCode::InvalidCursor);

        let oversized_page = repository_catalog_page(
            &service,
            catalog_request(u32::from(u16::MAX) + 1, false, Vec::new(), None, None),
            Instant::now(),
        )
        .expect_err("oversized page fails");
        assert_eq!(oversized_page.code(), ErrorCode::InvalidArgument);

        for error in [
            CatalogError::UnsupportedSortVersion,
            CatalogError::InvalidSortKey,
            CatalogError::SnapshotMismatch,
            CatalogError::SnapshotExpired,
            CatalogError::SnapshotEvicted,
            CatalogError::SnapshotUnavailable,
        ] {
            let public = catalog_error(error);
            assert_eq!(public.code(), ErrorCode::InvalidCursor);
            assert_eq!(public.next_actions(), &[NextAction::RestartEnumeration]);
        }

        let capacity = catalog_error(CatalogError::SnapshotEntryBound);
        assert_eq!(capacity.code(), ErrorCode::ResourceExhausted);
        assert!(capacity.retryable());
        assert_eq!(capacity.next_actions(), &[NextAction::Retry]);
        assert_eq!(
            catalog_error(CatalogError::UnsupportedFilter("workspace")).code(),
            ErrorCode::UnsupportedCapability
        );
        assert_eq!(
            catalog_error(CatalogError::CatalogInvariant).code(),
            ErrorCode::Internal
        );
    }

    #[test]
    fn repository_catalog_epoch_is_monotonic_and_process_relative() {
        let epoch = Instant::now();
        let first = catalog_now(epoch).expect("first catalog timestamp exists");
        let second = catalog_now(epoch).expect("second catalog timestamp exists");

        assert!(second >= first);
        assert!(second.as_millis() < 60_000);
    }

    #[test]
    fn operation_metadata_reuses_capacity_only_after_terminalization() {
        let first = OperationId::from_bytes([60; 16]);
        let second = OperationId::from_bytes([61; 16]);
        let mut metadata = OperationMetadataSet::new(1);
        metadata
            .reserve(first, 1, None)
            .expect("first metadata reserves");
        assert_eq!(
            metadata
                .reserve(second, 2, None)
                .expect_err("nonterminal metadata is retained")
                .code(),
            ErrorCode::ResourceExhausted
        );

        metadata.mark_terminal(first);
        metadata
            .reserve(second, 2, None)
            .expect("terminal metadata is evicted");
        assert!(!metadata.records.contains_key(&first));
        assert!(metadata.records.contains_key(&second));
    }

    #[test]
    fn committed_restore_accepts_a_reused_generation_lineage() {
        let operation = OperationId::from_bytes([62; 16]);
        let repository = RepositoryId::from_bytes([63; 16]);
        let generation = GenerationId::from_bytes([64; 20]);
        let lineage_parent = GenerationId::from_bytes([65; 20]);
        let started_unix_ms = 1_700_000_000_066;
        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let submission = repository_submission(operation, 62)
            .with_repository_context(
                RepositoryOperationSubmission::new(
                    repository,
                    Some(generation),
                    started_unix_ms,
                    64 * 1024 * 1024,
                    RepositoryOperationMode::Deep,
                )
                .expect("repository context is valid")
                .with_root_identity([67; 32]),
            )
            .expect("repository context attaches");
        journal.submit(submission).expect("operation submits");
        journal
            .start_execution(operation)
            .expect("operation starts");
        journal
            .complete_repository_publication(operation)
            .expect("operation succeeds");
        let context = journal
            .record_repository_publication(operation, generation)
            .expect("published generation projects durably");
        let mut unrelated_parent_context = context;
        unrelated_parent_context.parent_generation = Some(GenerationId::from_bytes([66; 20]));
        let record = journal
            .status(operation)
            .expect("operation remains visible");
        let receipt = FirstSliceIndexReceipt {
            repository,
            generation,
            parent: Some(lineage_parent),
            discovered_inputs: 3,
            visited_entries: 4,
            excluded_inputs: 1,
            oversized_inputs: 0,
            indexed_files: 3,
            entities: 6,
            lexical_documents: 3,
            oracle_allocated_bytes: 4_096,
            estimated_disk_bytes: 64 * 1024 * 1024,
            diagnostics: Vec::new(),
            elapsed_micros: 10,
        };
        let publication = FirstSliceDurableOperation {
            operation,
            started_unix_ms,
            provider: FirstSliceIndexProvider::Unknown,
            receipt,
        };
        let mut metadata = OperationMetadataSet::new(1);
        metadata
            .restore_context(context, &record)
            .expect("durable context restores");

        metadata
            .restore_committed(publication.clone(), &record)
            .expect("the exact reused generation restores");
        let restored = metadata
            .records
            .get(&operation)
            .expect("restored metadata remains visible");
        assert_eq!(restored.parent_generation, Some(generation));
        assert_eq!(restored.published_generation, Some(generation));
        assert_eq!(restored.receipt.as_ref(), Some(&publication.receipt));

        let mut unrelated_parent = OperationMetadataSet::new(1);
        unrelated_parent
            .restore_context(unrelated_parent_context, &record)
            .expect("durable context with an unrelated parent restores");
        assert_eq!(
            unrelated_parent
                .restore_committed(publication, &record)
                .expect_err("an unrelated parent cannot reuse the lineage exception"),
            FirstSliceError::Retention
        );
    }

    #[test]
    fn startup_context_restore_bounds_terminal_metadata_without_failure() {
        fn retained_operation(index: u16) -> OperationId {
            let mut bytes = [0_u8; 16];
            bytes[..2].copy_from_slice(&index.to_be_bytes());
            OperationId::from_bytes(bytes)
        }

        let journal = OperationJournal::open_in_memory().expect("journal opens");
        let repository = RepositoryId::from_bytes([62; 16]);
        for index in 1_u16..=257 {
            let operation = retained_operation(index);
            let submission = repository_submission(operation, 62)
                .with_repository_context(
                    RepositoryOperationSubmission::new(
                        repository,
                        None,
                        1_700_000_000_000 + u64::from(index),
                        64 * 1024 * 1024,
                        RepositoryOperationMode::Structural,
                    )
                    .expect("repository context is valid")
                    .with_root_identity([63; 32]),
                )
                .expect("repository context attaches");
            journal.submit(submission).expect("operation submits");
            journal
                .interrupt_deadline(operation)
                .expect("operation becomes terminal");
        }

        let mut metadata = OperationMetadataSet::new(DEFAULT_OPERATION_METADATA);
        for context in journal
            .repository_operation_contexts()
            .expect("startup contexts load")
        {
            let record = journal
                .status(context.operation)
                .expect("startup operation status loads");
            metadata
                .restore_context(context, &record)
                .expect("bounded startup restore cannot fail on terminal history");
        }

        assert_eq!(metadata.records.len(), DEFAULT_OPERATION_METADATA);
        assert!(metadata.records.contains_key(&retained_operation(257)));
        assert!(!metadata.records.contains_key(&retained_operation(1)));
    }

    #[test]
    fn deferred_recovery_keeps_catalog_reads_and_lifecycle_failures_bounded() {
        let service = Arc::new(RwLock::new(
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION)
                .expect("empty service initializes"),
        ));
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(4));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let (_refinement, refinement_receiver) = mpsc::sync_channel(1);
        let lanes = FirstSliceServiceLanes {
            service,
            index_serialization: Arc::new(Mutex::new(())),
            semantic_refinements: Arc::new(Mutex::new(BTreeMap::new())),
            refinement: _refinement,
            recovery_ready: Arc::new(AtomicBool::new(false)),
            support_state: None,
        };
        let resources = ServiceRequestResources {
            journal: &handle,
            metadata: &metadata,
            runtime: &runtime,
            catalog_epoch: Instant::now(),
            publication_hook: None,
        };
        let context = || {
            let deadline = Instant::now() + Duration::from_secs(5);
            FirstSliceIpcContext {
                client_instance_id: ClientInstanceId::from_bytes([93; 16]),
                selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
                cancellation: Cancellation::with_deadline(deadline),
                deadline,
                effective_budget: None,
                index_admission: None,
            }
        };
        let mut reply = None;

        let listed = execute_service_request(
            &lanes,
            resources,
            FirstSliceIpcRequest::RepositoryList(daemon::RepositoryListRequest {
                max_results: Some(20),
                query: None,
            }),
            context(),
            &mut reply,
        )
        .expect("catalog remains readable while recovery runs");
        assert!(matches!(
            listed,
            FirstSliceIpcResponse::RepositoryList(ref response)
                if response.repositories.is_empty()
        ));

        let error = execute_service_request(
            &lanes,
            resources,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: ".".to_owned(),
                operation: Some(operation_to_wire(OperationId::from_bytes([93; 16]))),
                detached: false,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
            context(),
            &mut reply,
        )
        .expect_err("mutations wait for durable recovery");
        assert_eq!(error.code(), ErrorCode::Busy);
        assert!(error.retryable());
        assert_eq!(error.next_actions(), &[NextAction::Retry]);

        drop(refinement_receiver);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn contended_publication_preserves_the_prepared_operation() {
        let service = Arc::new(RwLock::new(
            FirstSliceService::new(DEFAULT_GENERATION_RETENTION)
                .expect("empty service initializes"),
        ));
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 8, 8).expect("actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(8));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let (refinement, refinement_receiver) = mpsc::sync_channel(1);
        let lanes = FirstSliceServiceLanes {
            service: Arc::clone(&service),
            index_serialization: Arc::new(Mutex::new(())),
            semantic_refinements: Arc::new(Mutex::new(BTreeMap::new())),
            refinement,
            recovery_ready: Arc::new(AtomicBool::new(true)),
            support_state: None,
        };
        let resources = ServiceRequestResources {
            journal: &handle,
            metadata: &metadata,
            runtime: &runtime,
            catalog_epoch: Instant::now(),
            publication_hook: None,
        };
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn admitted_after_retry() -> bool { true }\n",
        )
        .expect("source writes");
        let context = |admission| {
            let deadline = Instant::now() + Duration::from_secs(5);
            FirstSliceIpcContext {
                client_instance_id: ClientInstanceId::from_bytes([94; 16]),
                selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
                cancellation: Cancellation::with_deadline(deadline),
                deadline,
                effective_budget: None,
                index_admission: Some(admission),
            }
        };
        let request = |operation, detached| {
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(operation)),
                detached,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            })
        };
        let operation = OperationId::from_bytes([94; 16]);
        let admission = rootlight_daemon_core::FirstSliceAdmission::default();
        let (locked_sender, locked_receiver) = mpsc::sync_channel(1);
        let blocking_service = Arc::clone(&service);
        let blocker = thread::spawn(move || {
            let _read_guard = blocking_service
                .read()
                .expect("service read lock is healthy");
            locked_sender.send(()).expect("lock signal sends");
            thread::sleep(Duration::from_millis(100));
        });
        locked_receiver.recv().expect("read lock is held");
        let mut reply = None;

        let started = Instant::now();
        let indexed = execute_service_request(
            &lanes,
            resources,
            request(operation, false),
            context(admission.clone()),
            &mut reply,
        )
        .expect("prepared generation waits for the publication boundary");
        assert!(
            started.elapsed() >= Duration::from_millis(75),
            "publication did not overlap the held read lock"
        );
        assert!(matches!(
            indexed,
            FirstSliceIpcResponse::RepositoryIndex(ref response)
                if response.operation.as_ref().map(|id| id.value.as_slice())
                    == Some(operation.as_bytes().as_slice())
        ));
        assert!(admission.was_inserted());
        assert_eq!(
            journal
                .status(operation)
                .expect("original operation is durable")
                .state,
            OperationState::Succeeded
        );
        let metadata = metadata.lock().expect("metadata lock is healthy");
        let completed = metadata
            .records
            .get(&operation)
            .expect("operation metadata remains inspectable");
        assert!(completed.terminal);
        assert!(completed.files_examined > 0);
        assert!(completed.bytes_examined > 0);
        drop(metadata);
        blocker.join().expect("read lock holder joins");

        drop(refinement_receiver);
        actor.join().expect("journal actor joins");
    }

    #[test]
    fn cancelled_rejection_reconciles_terminal_operation_metadata() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([96; 16]);
        journal
            .submit(repository_submission(operation, 96))
            .expect("operation submits");
        journal
            .request_cancellation(
                operation,
                CancellationAuthority::Internal(InternalCancellationAuthority::ClientDisconnect),
            )
            .expect("queued operation cancels");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let metadata = Mutex::new(OperationMetadataSet::new(1));
        metadata
            .lock()
            .expect("metadata locks")
            .reserve(operation, 1, None)
            .expect("metadata reserves");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(1);
        let cancellation = Cancellation::with_deadline(deadline);

        let error = reject_index_admission(
            &runtime,
            deadline,
            &actor.handle(),
            &metadata,
            operation,
            &cancellation,
            index_admission_in_progress(operation),
        )
        .expect_err("terminal cancellation wins over admission rejection");

        assert_eq!(error.code(), ErrorCode::Cancelled);
        let metadata = metadata.lock().expect("metadata lock is healthy");
        let reconciled = metadata
            .records
            .get(&operation)
            .expect("cancelled operation metadata remains inspectable");
        assert!(reconciled.terminal);
        assert!(matches!(
            reconciled.terminal_snapshot.as_ref(),
            Some(snapshot) if snapshot.state == OperationState::Cancelled
        ));
        drop(metadata);
        actor.join().expect("actor joins");
    }

    #[test]
    fn expired_activation_preserves_deadline_interruption() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([97; 16]);
        journal
            .submit(repository_submission(operation, 97))
            .expect("operation submits");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let metadata = Mutex::new(OperationMetadataSet::new(1));
        metadata
            .lock()
            .expect("metadata locks")
            .reserve(operation, 1, None)
            .expect("metadata reserves");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let elapsed = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("elapsed deadline derives");
        let cancellation = Cancellation::with_deadline(elapsed);

        let error = reject_index_admission(
            &runtime,
            elapsed,
            &actor.handle(),
            &metadata,
            operation,
            &cancellation,
            index_admission_in_progress(operation),
        )
        .expect_err("expired activation is compensated");

        assert_eq!(error.code(), ErrorCode::Busy);
        assert!(error.retryable());
        assert_eq!(
            error.next_actions(),
            &[NextAction::InspectOperation, NextAction::Retry]
        );
        let terminal = journal.status(operation).expect("terminal state loads");
        assert_eq!(terminal.state, OperationState::Interrupted);
        assert_eq!(terminal.recovery_class, RecoveryClass::DeadlineElapsed);
        let metadata = metadata.lock().expect("metadata lock is healthy");
        let reconciled = metadata
            .records
            .get(&operation)
            .expect("rejected operation metadata remains inspectable");
        assert!(reconciled.terminal);
        assert!(matches!(
            reconciled.terminal_snapshot.as_ref(),
            Some(snapshot) if snapshot.state == OperationState::Interrupted
        ));
        drop(metadata);
        actor.join().expect("actor joins");
    }

    #[test]
    fn compensated_rejection_preserves_non_client_public_errors() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let metadata = Mutex::new(OperationMetadataSet::new(2));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");

        for (seed, reason) in [
            (98, CancellationReason::Shutdown),
            (99, CancellationReason::ResourceLimit),
        ] {
            let operation = OperationId::from_bytes([seed; 16]);
            journal
                .submit(repository_submission(operation, seed))
                .expect("operation submits");
            metadata
                .lock()
                .expect("metadata locks")
                .reserve(operation, u64::from(seed), None)
                .expect("metadata reserves");
            let cancellation = Cancellation::new();
            assert!(cancellation.cancel(reason));
            let elapsed = Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("elapsed deadline derives");
            let expected = service_error(FirstSliceError::Cancelled(reason));

            let error = reject_index_admission(
                &runtime,
                elapsed,
                &actor.handle(),
                &metadata,
                operation,
                &cancellation,
                expected.clone(),
            )
            .expect_err("cancelled admission is compensated");

            assert_eq!(error, expected);
            let terminal = journal.status(operation).expect("terminal state loads");
            assert_eq!(terminal.state, OperationState::Cancelled);
            assert_eq!(terminal.cancellation_reason, Some(reason));
            assert!(
                metadata
                    .lock()
                    .expect("metadata lock is healthy")
                    .records
                    .get(&operation)
                    .is_some_and(|metadata| metadata.terminal)
            );
        }

        actor.join().expect("actor joins");
    }

    #[test]
    fn activated_rejection_reports_a_winning_durable_shutdown() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([100; 16]);
        journal
            .submit(repository_submission(operation, 100))
            .expect("operation submits");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(1));
        metadata
            .lock()
            .expect("metadata locks")
            .reserve(operation, 1, None)
            .expect("metadata reserves");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(1);
        runtime
            .block_on(handle.activate_operation_until(operation, deadline))
            .expect("admission activation succeeds");
        journal
            .request_cancellation(
                operation,
                CancellationAuthority::Internal(InternalCancellationAuthority::Shutdown),
            )
            .expect("durable shutdown wins before failure finalization");
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("elapsed local deadline derives"),
        );
        assert_eq!(
            cancellation.reason(),
            Some(CancellationReason::DeadlineExceeded)
        );
        let admission_error = index_admission_in_progress(operation);

        let error = reject_activated_index_admission(
            &runtime,
            deadline,
            &handle,
            &metadata,
            operation,
            &cancellation,
            admission_error,
        )
        .expect_err("winning shutdown replaces the admission error");

        assert_eq!(error, cancellation_error(CancellationReason::Shutdown));
        let terminal = journal.status(operation).expect("terminal state loads");
        assert_eq!(terminal.state, OperationState::Cancelled);
        assert_eq!(
            terminal.cancellation_reason,
            Some(CancellationReason::Shutdown)
        );
        let metadata = metadata.lock().expect("metadata lock is healthy");
        let reconciled = metadata
            .records
            .get(&operation)
            .expect("terminal metadata remains inspectable");
        assert!(reconciled.terminal);
        assert!(matches!(
            reconciled.terminal_snapshot.as_ref(),
            Some(snapshot) if snapshot.state == OperationState::Cancelled
        ));
        drop(metadata);
        actor.join().expect("actor joins");
    }

    #[test]
    fn unavailable_actor_lane_uses_direct_failure_compensation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([103; 16]);
        journal
            .submit(repository_submission(operation, 103))
            .expect("operation submits");
        let actor = JournalActor::start(Arc::clone(&journal), 1, 1).expect("actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(1));
        metadata
            .lock()
            .expect("metadata locks")
            .reserve(operation, 1, None)
            .expect("metadata reserves");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(1);
        runtime
            .block_on(handle.activate_operation_until(operation, deadline))
            .expect("admission activation succeeds");
        actor.join().expect("actor closes before terminalization");
        let failure = index_admission_in_progress(operation);

        let terminal = finish_failed_index(
            &runtime,
            deadline,
            &handle,
            &metadata,
            operation,
            &Cancellation::new(),
            &failure,
        )
        .expect("direct fallback terminalizes unowned work");

        assert_eq!(terminal.state, OperationState::Failed);
        assert_eq!(terminal.stage, OperationStage::Cleanup);
        assert_eq!(terminal.error.as_ref(), Some(&failure));
        assert_eq!(
            journal.status(operation).expect("terminal state loads"),
            terminal
        );
        let metadata = metadata.lock().expect("metadata lock is healthy");
        let reconciled = metadata
            .records
            .get(&operation)
            .expect("terminal metadata remains inspectable");
        assert!(reconciled.terminal);
        assert!(matches!(
            reconciled.terminal_snapshot.as_ref(),
            Some(snapshot) if snapshot.state == OperationState::Failed
        ));
    }

    #[test]
    fn late_local_cancellation_preserves_publication_failure() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(Arc::clone(&journal), 8, 8).expect("actor starts");
        let handle = actor.handle();
        let metadata = Mutex::new(OperationMetadataSet::new(2));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let lifecycle_deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("elapsed lifecycle deadline derives");
        let deadline_cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("elapsed deadline derives"),
        );
        let resource_cancellation = Cancellation::new();
        assert!(resource_cancellation.cancel(CancellationReason::ResourceLimit));

        for (seed, cancellation) in [(101, deadline_cancellation), (102, resource_cancellation)] {
            let operation = OperationId::from_bytes([seed; 16]);
            journal
                .submit(repository_submission(operation, seed))
                .expect("operation submits");
            journal
                .start_execution(operation)
                .expect("operation starts");
            let authorized = journal
                .authorize_repository_publication(operation)
                .expect("publication authorization closes cancellation admission");
            assert_eq!(authorized.state, OperationState::Running);
            assert_eq!(authorized.stage, OperationStage::Cleanup);
            metadata
                .lock()
                .expect("metadata locks")
                .reserve(operation, u64::from(seed), None)
                .expect("metadata reserves");
            let failure =
                PublicError::builder(ErrorCode::AdapterFailed, "repository publication failed")
                    .operation(operation)
                    .build()
                    .expect("failure validates");

            let terminal = finish_failed_index(
                &runtime,
                lifecycle_deadline,
                &handle,
                &metadata,
                operation,
                &cancellation,
                &failure,
            )
            .expect("late cancellation falls back to failure");

            assert_eq!(terminal.state, OperationState::Failed);
            assert_eq!(terminal.stage, OperationStage::Cleanup);
            assert_eq!(terminal.error.as_ref(), Some(&failure));
            assert_eq!(
                journal.status(operation).expect("terminal state loads"),
                terminal
            );
            let metadata = metadata.lock().expect("metadata lock is healthy");
            let reconciled = metadata
                .records
                .get(&operation)
                .expect("terminal metadata remains inspectable");
            assert!(reconciled.terminal);
            assert!(matches!(
                reconciled.terminal_snapshot.as_ref(),
                Some(snapshot) if snapshot.state == OperationState::Failed
            ));
        }

        actor.join().expect("actor joins");
    }

    #[test]
    fn lifecycle_finalization_deadlines_reanchor_each_mutation() {
        let origin = Instant::now();
        let expired = origin
            .checked_sub(Duration::from_secs(1))
            .expect("expired deadline derives");
        let first_start = origin + Duration::from_secs(1);
        let second_start = origin + Duration::from_secs(3);

        let first =
            fresh_lifecycle_deadline_at(expired, first_start).expect("first deadline derives");
        let second =
            fresh_lifecycle_deadline_at(expired, second_start).expect("second deadline derives");

        assert_eq!(first, first_start + LIFECYCLE_FINALIZATION_GRACE);
        assert_eq!(second, second_start + LIFECYCLE_FINALIZATION_GRACE);
        assert!(second > first);
    }

    #[test]
    fn repository_reads_remain_available_during_paused_index() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::BeforeCompletion,
            fail_commit: AtomicBool::new(false),
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
            "pub fn independently_readable() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([92; 16]);

        let accepted = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(operation)),
                detached: true,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
        );
        assert!(matches!(
            accepted,
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches the paused publication boundary");

        let started = Instant::now();
        let listed = execute_with_timeout(
            &daemon,
            FirstSliceIpcRequest::RepositoryList(daemon::RepositoryListRequest {
                max_results: Some(20),
                query: None,
            }),
        );
        let elapsed = started.elapsed();
        release_sender.send(()).expect("index resumes");
        wait_for_terminal_operation(&journal, operation);
        let listed = listed.expect("repository read remains available");
        assert!(
            elapsed < Duration::from_secs(1),
            "repository read was delayed for {elapsed:?}"
        );
        let FirstSliceIpcResponse::RepositoryList(listed) = listed else {
            panic!("repository list response expected");
        };
        assert!(listed.repositories.is_empty());

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
    fn operation_status_long_poll_does_not_block_cancellation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::BeforeCompletion,
            fail_commit: AtomicBool::new(false),
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
            "pub fn long_poll_target() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([47; 16]);
        let admitted = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(operation)),
                detached: true,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
        );
        assert!(matches!(
            admitted,
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches the completion boundary");
        let current = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryOperationStatus(
                daemon::RepositoryOperationStatusRequest {
                    schema_version: Some(schema_version()),
                    operation: Some(operation_to_wire(operation)),
                    action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                    wait_ms: Some(0),
                    after_revision: None,
                },
            ),
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(current) = current else {
            panic!("operation status response expected");
        };
        let revision = current.operation.expect("operation status exists").revision;
        let polling_daemon = daemon.clone();
        let (poll_started_sender, poll_started_receiver) = mpsc::sync_channel(1);
        let (poll_sender, poll_receiver) = mpsc::sync_channel(1);
        let poll = thread::spawn(move || {
            poll_started_sender
                .send(())
                .expect("poll start is observed");
            let response = execute(
                &polling_daemon,
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(operation)),
                        action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                        wait_ms: Some(5_000),
                        after_revision: Some(revision),
                    },
                ),
            );
            poll_sender
                .send(response)
                .expect("poll response is observed");
        });
        poll_started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("long poll starts");
        thread::sleep(OPERATION_STATUS_POLL_INTERVAL + Duration::from_millis(25));

        let cancellation_started = Instant::now();
        let cancelled = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryOperationStatus(
                daemon::RepositoryOperationStatusRequest {
                    schema_version: Some(schema_version()),
                    operation: Some(operation_to_wire(operation)),
                    action: daemon::RepositoryOperationAction::RepositoryOperationCancel as i32,
                    wait_ms: Some(5_000),
                    after_revision: Some(revision),
                },
            ),
        );
        assert!(
            cancellation_started.elapsed() < Duration::from_secs(1),
            "a long poll must not monopolize the control lane"
        );
        assert!(matches!(
            cancelled,
            FirstSliceIpcResponse::RepositoryOperationStatus(_)
        ));
        let polled = poll_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("revision change releases the long poll");
        let FirstSliceIpcResponse::RepositoryOperationStatus(polled) = polled else {
            panic!("long-poll status response expected");
        };
        assert!(polled.operation.expect("polled operation exists").revision > revision);

        release_sender.send(()).expect("index resumes");
        poll.join().expect("poll thread joins");
        wait_for_terminal_operation(&journal, operation);
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

    fn wait_for_terminal_operation(
        journal: &OperationJournal,
        operation: OperationId,
    ) -> OperationRecord {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let record = journal
                .status(operation)
                .expect("operation remains visible");
            if record.state.is_terminal() {
                return record;
            }
            assert!(
                Instant::now() < deadline,
                "operation did not reach a terminal state"
            );
            thread::sleep(Duration::from_millis(10));
        }
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
            fail_commit: AtomicBool::new(false),
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
                effective_budget: None,
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
                    mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
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
        let terminal = wait_for_terminal_operation(&journal, operation);
        let status = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(operation)),
                        action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                )
            },
            "terminal operation status follows committed publication",
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
            panic!("operation status response expected");
        };
        let published = status.published_generation.is_some();
        if prove_lane_reusable {
            let follow_up = execute_retrying_busy(
                &daemon,
                || {
                    FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                        schema_version: Some(schema_version()),
                        root: follow_up_root.clone(),
                        operation: Some(operation_to_wire(OperationId::from_bytes(
                            [operation_byte.wrapping_add(64); 16],
                        ))),
                        detached: true,
                        mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
                    })
                },
                "fresh index completes on the released work lane",
            );
            let FirstSliceIpcResponse::RepositoryIndex(follow_up) = follow_up else {
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

        assert!(matches!(
            response.expect("detached admission was already acknowledged"),
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
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
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
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
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
        );
        let FirstSliceIpcResponse::RepositoryIndex(first) = first else {
            panic!("index response expected");
        };
        assert!(first.published_generation.is_none());
        assert!(first.estimated_disk_bytes > 0);
        wait_for_terminal_operation(&journal, OperationId::from_bytes([1; 16]));
        let retry = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root: fixture.path().to_string_lossy().into_owned(),
                    operation: Some(operation_to_wire(OperationId::from_bytes([1; 16]))),
                    detached: true,
                    mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
                })
            },
            "terminal operation retry becomes visible",
        );
        let FirstSliceIpcResponse::RepositoryIndex(retry) = retry else {
            panic!("retry index response expected");
        };
        assert_eq!(retry.repository, first.repository);
        assert_eq!(retry.operation, first.operation);
        let repository = first.repository.clone().expect("repository is returned");
        let generation = retry
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
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
        );
        assert!(matches!(second, FirstSliceIpcResponse::RepositoryIndex(_)));
        wait_for_terminal_operation(&journal, OperationId::from_bytes([2; 16]));
        let _ = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(OperationId::from_bytes([2; 16]))),
                        action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                )
            },
            "second generation publication becomes visible",
        );
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
                page_offset: 0,
                languages: Vec::new(),
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
    fn status_cannot_observe_success_before_generation_commit() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::AfterSuccess,
            fail_commit: AtomicBool::new(false),
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
                    mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
                }),
            )
        });
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches success/commit boundary");

        let status = execute_with_timeout(
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
        )
        .expect_err("staged publication remains externally in progress");
        assert_eq!(status.code(), ErrorCode::Busy);
        assert!(status.retryable());
        assert_eq!(status.next_actions(), &[NextAction::Retry]);

        release_sender.send(()).expect("publication resumes");
        let response = index.join().expect("index thread joins");
        let FirstSliceIpcResponse::RepositoryIndex(indexed) = response else {
            panic!("repository index response expected");
        };
        assert!(indexed.published_generation.is_none());
        let repository = indexed.repository.clone().expect("repository is returned");
        wait_for_terminal_operation(&journal, operation);
        let status = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(operation)),
                        action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                )
            },
            "committed operation status becomes visible",
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
            panic!("committed operation status response expected");
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
        assert_eq!(status.files_examined, 2);
        assert!(status.bytes_examined > 0);
        let generation = status
            .published_generation
            .clone()
            .expect("generation is returned after commit");
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
                page_offset: 0,
                languages: Vec::new(),
            }),
        );
        let FirstSliceIpcResponse::CodeLocate(locate) = locate else {
            panic!("first publicly successful status must name a queryable generation");
        };
        assert_eq!(locate.hits.len(), 1);
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

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_commit_boundary_failure_is_terminal_and_publishes_nothing() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let journal = Arc::new(
            OperationJournal::open(&paths.operation_journal_path()).expect("journal opens"),
        );
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::AfterSuccess,
            fail_commit: AtomicBool::new(true),
            armed: AtomicBool::new(true),
            reached: reached_sender,
            release: release_receiver,
        };
        let (daemon, workers) = FirstSliceDaemon::start_durable_with_publication_hook(
            actor.handle(),
            paths.state_dir(),
            hook,
        )
        .expect("durable host starts");
        let fixture = TempDir::new().expect("fixture exists");
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn answer() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([45; 16]);
        let index_daemon = daemon.clone();
        let root = fixture.path().to_string_lossy().into_owned();
        let retry_root = root.clone();
        let index = thread::spawn(move || {
            execute_with_timeout(
                &index_daemon,
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root,
                    operation: Some(operation_to_wire(operation)),
                    detached: true,
                    mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
                }),
            )
        });
        reached_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("index reaches the commit boundary");
        release_sender.send(()).expect("commit attempt resumes");
        let accepted = index
            .join()
            .expect("index thread joins")
            .expect("detached admission was acknowledged");
        assert!(matches!(
            accepted,
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
        wait_for_terminal_operation(&journal, operation);

        let status = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(operation)),
                        action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                )
            },
            "failed operation status becomes visible",
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
            panic!("failed-closed operation status response expected");
        };
        let failed = status.operation.expect("failed operation is returned");
        assert_eq!(failed.state, daemon::OperationState::Failed as i32);
        let error = failed.error.expect("failed publication has a public error");
        assert_eq!(error.code, common::ErrorCode::IndexCorrupt as i32);
        assert!(status.published_generation.is_none());
        assert!(status.peak_rss_bytes > 0);
        assert!(status.written_bytes > 0);

        let follow_up = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                    schema_version: Some(schema_version()),
                    root: retry_root.clone(),
                    operation: Some(operation_to_wire(OperationId::from_bytes([46; 16]))),
                    detached: true,
                    mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
                })
            },
            "reindex after failed publication succeeds",
        );
        let FirstSliceIpcResponse::RepositoryIndex(follow_up) = follow_up else {
            panic!("reindex after failed publication succeeds");
        };
        assert!(
            follow_up.parent_generation.is_none(),
            "failed publication must not become a generation parent"
        );

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
        drop(journal);
        let reopened =
            OperationJournal::open(&paths.operation_journal_path()).expect("journal reopens");
        let persisted = reopened
            .status(operation)
            .expect("terminal failure survives restart");
        assert_eq!(persisted.state, OperationState::Failed);
        assert_eq!(persisted.stage, OperationStage::Cleanup);
        assert!(persisted.peak_rss_bytes > 0);
        assert!(persisted.written_bytes > 0);
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn durable_external_commit_reconciles_live_finalization_failure() {
        let storage = durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .expect("test runtime paths are valid");
        paths
            .prepare_owner()
            .expect("account-private runtime paths prepare");
        let journal = Arc::new(
            OperationJournal::open(&paths.operation_journal_path()).expect("journal opens"),
        );
        let actor =
            JournalActor::start(Arc::clone(&journal), 16, 16).expect("journal actor starts");
        let (reached_sender, reached_receiver) = mpsc::sync_channel(1);
        let (release_sender, release_receiver) = mpsc::sync_channel(1);
        drop(release_sender);
        let hook = PublicationBoundaryHook {
            boundary: PublicationBoundary::AfterCommit,
            fail_commit: AtomicBool::new(false),
            armed: AtomicBool::new(true),
            reached: reached_sender,
            release: release_receiver,
        };
        let (daemon, workers) = FirstSliceDaemon::start_durable_with_publication_hook(
            actor.handle(),
            paths.state_dir(),
            hook,
        )
        .expect("durable host starts");
        let fixture = durable_test_tempdir();
        fs::create_dir(fixture.path().join("src")).expect("source directory exists");
        fs::write(
            fixture.path().join("src/lib.rs"),
            "pub fn reconciled_answer() -> u32 { 42 }\n",
        )
        .expect("source writes");
        let operation = OperationId::from_bytes([49; 16]);

        let accepted = execute(
            &daemon,
            FirstSliceIpcRequest::RepositoryIndex(daemon::RepositoryIndexRequest {
                schema_version: Some(schema_version()),
                root: fixture.path().to_string_lossy().into_owned(),
                operation: Some(operation_to_wire(operation)),
                detached: true,
                mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
            }),
        );
        assert!(matches!(
            accepted,
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
        reached_receiver
            .recv_timeout(Duration::from_secs(30))
            .expect("external durable commit precedes injected finalization failure");
        wait_for_terminal_operation(&journal, operation);
        let terminal = journal.status(operation).expect("terminal status loads");
        assert_eq!(terminal.state, OperationState::Succeeded);
        assert_eq!(terminal.stage, OperationStage::Cleanup);
        assert!(terminal.peak_rss_bytes > 0);
        assert!(terminal.written_bytes > 0);

        let status = execute_retrying_busy(
            &daemon,
            || {
                FirstSliceIpcRequest::RepositoryOperationStatus(
                    daemon::RepositoryOperationStatusRequest {
                        schema_version: Some(schema_version()),
                        operation: Some(operation_to_wire(operation)),
                        action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                        wait_ms: None,
                        after_revision: None,
                    },
                )
            },
            "reconciled publication becomes visible",
        );
        let FirstSliceIpcResponse::RepositoryOperationStatus(status) = status else {
            panic!("repository operation status response expected");
        };
        assert!(status.published_generation.is_some());
        assert_eq!(status.peak_rss_bytes, terminal.peak_rss_bytes);
        assert_eq!(status.written_bytes, terminal.written_bytes);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        drop(daemon);
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(5)))
            .expect("workers stop");
        actor.join().expect("journal actor joins");
        drop(journal);
        let reopened =
            OperationJournal::open(&paths.operation_journal_path()).expect("journal reopens");
        let persisted = reopened
            .status(operation)
            .expect("reconciled success survives restart");
        assert_eq!(persisted.state, OperationState::Succeeded);
        assert_eq!(persisted.peak_rss_bytes, terminal.peak_rss_bytes);
        assert_eq!(persisted.written_bytes, terminal.written_bytes);
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
            fail_commit: AtomicBool::new(false),
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
                effective_budget: None,
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
                    mode: daemon::RepositoryIndexMode::RepositoryIndexStructural as i32,
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
        assert!(matches!(
            index
                .join()
                .expect("index thread joins")
                .expect("detached admission was acknowledged"),
            FirstSliceIpcResponse::RepositoryIndex(_)
        ));
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
    fn shutdown_cancels_refinement_registered_before_journal_activation() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let actor = JournalActor::start(journal, 4, 4).expect("journal actor starts");
        let (daemon, workers) = FirstSliceDaemon::start(actor.handle()).expect("host starts");
        let cancellation = Cancellation::new();
        register_semantic_refinement(
            &workers.semantic_refinements,
            OperationId::from_bytes([47; 16]),
            RepositoryId::from_bytes([48; 16]),
            cancellation.clone(),
        )
        .expect("refinement registers");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        runtime
            .block_on(workers.stop(tokio::time::Instant::now() + Duration::from_secs(2)))
            .expect("workers stop within the global cap");

        assert_eq!(
            cancellation.reason(),
            Some(CancellationReason::Shutdown),
            "shutdown reaches refinements that are not journal-visible yet"
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
        metadata
            .reserve(operation, 1, None)
            .expect("metadata reserves");
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
        metadata
            .reserve(failed, 1, None)
            .expect("metadata reserves");
        assert_eq!(
            retry_index_response(
                &Mutex::new(metadata),
                failed_record,
                FirstSliceIndexMode::Structural,
            )
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
                CancellationAuthority::Internal(InternalCancellationAuthority::ClientDisconnect),
            )
            .expect("cancellation persists")
            .operation;
        let mut cancelled_metadata = OperationMetadataSet::new(1);
        cancelled_metadata
            .reserve(cancelled, 1, None)
            .expect("metadata reserves");
        let cancelled_error = retry_index_response(
            &Mutex::new(cancelled_metadata),
            cancelled_record,
            FirstSliceIndexMode::Structural,
        )
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
            .reserve(interrupted, 1, None)
            .expect("metadata reserves");
        let interrupted_error = retry_index_response(
            &Mutex::new(interrupted_metadata),
            interrupted_record,
            FirstSliceIndexMode::Structural,
        )
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
        metadata
            .reserve(operation, 1, None)
            .expect("metadata reserves");
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
                effective_budget: None,
                index_admission: None,
            },
        )
        .expect_err("missing receipt fails closed");
        assert_eq!(error.code(), ErrorCode::Internal);
        actor.join().expect("actor joins");
    }

    #[test]
    fn succeeded_status_reloads_published_generation_after_metadata_eviction() {
        let journal = Arc::new(OperationJournal::open_in_memory().expect("journal opens"));
        let operation = OperationId::from_bytes([56; 16]);
        let repository = RepositoryId::from_bytes([57; 16]);
        let generation = GenerationId::from_bytes([58; 20]);
        let submission = repository_submission(operation, 56)
            .with_repository_context(
                RepositoryOperationSubmission::new(
                    repository,
                    None,
                    1_700_000_000_056,
                    64 * 1024 * 1024,
                    RepositoryOperationMode::Structural,
                )
                .expect("repository context is valid")
                .with_root_identity([59; 32]),
            )
            .expect("repository context attaches");
        journal
            .submit(submission)
            .expect("repository operation submits");
        journal
            .start_execution(operation)
            .expect("repository operation starts");
        journal
            .complete_repository_publication(operation)
            .expect("publication succeeds");
        journal
            .record_repository_publication(operation, generation)
            .expect("published generation projects durably");
        let actor = JournalActor::start(Arc::clone(&journal), 4, 4).expect("actor starts");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime builds");
        let deadline = Instant::now() + Duration::from_secs(1);

        let status = repository_operation_status(
            &actor.handle(),
            &Mutex::new(OperationMetadataSet::new(1)),
            &runtime,
            daemon::RepositoryOperationStatusRequest {
                schema_version: Some(schema_version()),
                operation: Some(operation_to_wire(operation)),
                action: daemon::RepositoryOperationAction::RepositoryOperationGet as i32,
                after_revision: None,
                wait_ms: None,
            },
            &FirstSliceIpcContext {
                client_instance_id: ClientInstanceId::from_bytes([56; 16]),
                selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
                cancellation: Cancellation::with_deadline(deadline),
                deadline,
                effective_budget: None,
                index_admission: None,
            },
        )
        .expect("durable context reconstructs evicted status");

        assert_eq!(
            parse_generation(status.published_generation.as_ref())
                .expect("published generation is retained"),
            generation
        );
        assert_eq!(status.started_unix_ms, 1_700_000_000_056);
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
        let metadata = Mutex::new(OperationMetadataSet::new(1));
        metadata
            .lock()
            .expect("metadata locks")
            .reserve(operation, 1, None)
            .expect("metadata reserves");

        finish_failed_index(
            &runtime,
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("elapsed lifecycle deadline derives"),
            &actor.handle(),
            &metadata,
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
            selected_protocol_minor: rootlight_daemon_core::PROTOCOL_MINOR,
            cancellation: rootlight_operations::Cancellation::with_deadline(deadline),
            deadline,
            effective_budget: None,
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

    fn execute_retrying_busy(
        daemon: &FirstSliceDaemon,
        mut request: impl FnMut() -> FirstSliceIpcRequest,
        expectation: &str,
    ) -> FirstSliceIpcResponse {
        // A terminal journal record can become visible immediately before the
        // public metadata lane is released. Only that documented transition is
        // retryable; every other response remains an immediate test failure.
        let retry_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match execute_with_timeout(daemon, request()) {
                Ok(response) => return response,
                Err(error)
                    if error.code() == ErrorCode::Busy && Instant::now() < retry_deadline =>
                {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{expectation}: {error:?}"),
            }
        }
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
            effective_budget: None,
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
