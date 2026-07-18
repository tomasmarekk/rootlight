//! Rootlight command-line entry point.
//!
//! Argument parsing and JSON rendering stay at this edge; daemon and standalone
//! modes execute the same typed control and orchestration contracts.

#![forbid(unsafe_code)]

use std::{
    env, fs,
    io::Write as _,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_client::{
    Client, ClientError, ConnectPolicy, DaemonLifecycle as ClientDaemonLifecycle, DiagnosticsQuick,
    Health, HealthStatus as ClientHealthStatus, OperationKind, OperationStage, OperationStatus,
    RecoveryClass, ResourcePressure as ClientResourcePressure,
    SupportBundle as ClientSupportBundle,
};
use rootlight_daemon_core::{
    ControlRequest, ControlResponse, ControlService, DaemonLifecycle, DaemonLimits,
    DaemonOrchestrator, DaemonState, DiagnosticOutcome as DomainDiagnosticOutcome,
    DiagnosticsQuick as DomainDiagnosticsQuick, HealthStatus as DomainHealthStatus, JournalActor,
    OperationPreparationError, PreparedOperationSubmission,
    ResourcePressure as DomainResourcePressure, ServiceError, SupportBundle as DomainSupportBundle,
};
use rootlight_error::{DetailKey, ErrorCode, PublicError, PublicValue, SafeLabel};
use rootlight_ids::OperationId;
use rootlight_operations::{
    CatalogWriterLock, ClientInstanceId, OperationJournal, OperationRecord,
    OperationStage as JournalStage, OperationState as JournalState,
    RecoveryClass as JournalRecoveryClass,
};
use rootlight_runtime::{PrivateOutputFile, RuntimeError, RuntimePaths};
#[cfg(test)]
use rootlight_service::CancellationReason;
use rootlight_service::{
    Cancellation, CodeLocateResult, FirstSliceError, FirstSliceIndexReceipt, FirstSliceService,
    LocateMode, QueryResponse, SourceReadQueryResult, SymbolExplainResult,
};
use serde::Serialize;

const CLI_CONTRACT_VERSION: &str = "1.0";
const FIRST_SLICE_DEMO_CONTRACT_VERSION: &str = "1.0";
const HARD_MAX_CLI_JSON_BYTES: usize = 4 * 1024 * 1024;
const FIRST_SLICE_SOURCE_BEFORE: &str = "pub fn answer() -> u32 {\n    42\n}\n";
const FIRST_SLICE_SOURCE_AFTER: &str = "pub fn answer() -> u32 {\n    43\n}\n";

fn main() -> ExitCode {
    match run() {
        Ok(result) => match render_json(&CliEnvelope::success(result)) {
            Ok(json) => {
                println!("{json}");
                ExitCode::SUCCESS
            }
            Err(()) => {
                eprintln!("rootlight: output serialization failed");
                ExitCode::from(ExitFamily::Internal.code())
            }
        },
        Err(error) => {
            let exit = error.exit_family();
            let public_error = match error.public_error() {
                Ok(public_error) => public_error,
                Err(_) => {
                    eprintln!("rootlight: public error construction failed");
                    return ExitCode::from(ExitFamily::Internal.code());
                }
            };
            let envelope = CliEnvelope::failure(exit, public_error);
            match render_json(&envelope) {
                Ok(json) => eprintln!("{json}"),
                Err(()) => eprintln!("rootlight: output serialization failed"),
            }
            ExitCode::from(exit.code())
        }
    }
}

fn render_json(value: &CliEnvelope) -> Result<String, ()> {
    let mut output = BoundedJsonBuffer::new(HARD_MAX_CLI_JSON_BYTES);
    serde_json::to_writer(&mut output, value).map_err(|_| ())?;
    String::from_utf8(output.into_bytes()).map_err(|_| ())
}

struct BoundedJsonBuffer {
    bytes: Vec<u8>,
    maximum: usize,
}

impl BoundedJsonBuffer {
    const fn new(maximum: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum,
        }
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

impl std::io::Write for BoundedJsonBuffer {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let required = self
            .bytes
            .len()
            .checked_add(buffer.len())
            .ok_or_else(|| std::io::Error::other("CLI JSON output limit exceeded"))?;
        if required > self.maximum {
            return Err(std::io::Error::other("CLI JSON output limit exceeded"));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(|_| std::io::Error::other("CLI JSON output allocation failed"))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run() -> Result<CommandResult, CliError> {
    let mut arguments = env::args_os().skip(1);
    let first = arguments.next().ok_or(CliError::Usage)?;
    let (standalone, command) = if first == "--standalone" {
        (true, arguments.next().ok_or(CliError::Usage)?)
    } else {
        (false, first)
    };
    let trailing = arguments.collect::<Vec<_>>();

    if command == "first-slice-demo" {
        return execute_first_slice_demo(&trailing);
    }

    dispatch_after_command_preflight(standalone, &command, &trailing, |standalone| {
        let paths = runtime_paths()?;
        if standalone {
            execute_standalone(&paths, command.to_string_lossy().as_ref(), &trailing)
        } else {
            let mut client_instance_id = [0_u8; 16];
            getrandom::fill(&mut client_instance_id).map_err(|_| CliError::RandomUnavailable)?;
            let client = Client::connect_or_start(
                &paths,
                client_instance_id,
                ConnectPolicy::StartIfMissing,
            )?;
            execute_client(&client, command.to_string_lossy().as_ref(), &trailing)
        }
    })
}

fn dispatch_after_command_preflight<T>(
    standalone: bool,
    command: &std::ffi::OsStr,
    arguments: &[std::ffi::OsString],
    dispatch: impl FnOnce(bool) -> Result<T, CliError>,
) -> Result<T, CliError> {
    if matches!(
        arguments,
        [output, _] if command == "support-bundle" && output == "--output"
    ) {
        preflight_support_output()?;
    }
    dispatch(standalone)
}

fn execute_first_slice_demo(arguments: &[std::ffi::OsString]) -> Result<CommandResult, CliError> {
    if !arguments.is_empty() {
        return Err(CliError::Usage);
    }
    let started = Instant::now();
    let fixture = tempfile::Builder::new()
        .prefix("rootlight-first-slice-")
        .tempdir()
        .map_err(CliError::DemoIo)?;
    let source_directory = fixture.path().join("src");
    fs::create_dir(&source_directory).map_err(CliError::DemoIo)?;
    let source_path = source_directory.join("lib.rs");
    fs::write(&source_path, FIRST_SLICE_SOURCE_BEFORE).map_err(CliError::DemoIo)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .ok_or(CliError::Clock)?;
    let cancellation = Cancellation::with_deadline(deadline);
    let mut service = FirstSliceService::new(2)?;

    let first = service.index_rust_fixture(fixture.path(), &cancellation)?;
    let locate = service.code_locate(
        first.generation,
        "answer".to_owned(),
        LocateMode::Exact,
        8,
        &cancellation,
    )?;
    let hit = locate.data.hits.first().ok_or(CliError::DemoInvariant)?;
    let symbol = hit.symbol;
    let reference = hit.source.clone().ok_or(CliError::DemoInvariant)?;
    let explain = service.symbol_explain(first.generation, symbol, &cancellation)?;
    let source = service.source_read(first.generation, vec![reference], &cancellation)?;

    fs::write(&source_path, FIRST_SLICE_SOURCE_AFTER).map_err(CliError::DemoIo)?;
    let second = service.index_rust_fixture(fixture.path(), &cancellation)?;
    let second_locate = service.code_locate(
        second.generation,
        "answer".to_owned(),
        LocateMode::Exact,
        8,
        &cancellation,
    )?;
    let pinned_first = service.code_locate(
        first.generation,
        "answer".to_owned(),
        LocateMode::Exact,
        8,
        &cancellation,
    )?;
    let second_symbol = second_locate
        .data
        .hits
        .first()
        .map(|hit| hit.symbol)
        .ok_or(CliError::DemoInvariant)?;
    if second.parent != Some(first.generation)
        || service.active_generation() != Some(second.generation)
        || second_symbol != symbol
        || pinned_first.data != locate.data
    {
        return Err(CliError::DemoInvariant);
    }
    let measurements = FirstSliceMeasurements {
        total_wall_micros: elapsed_micros(started),
        first_index_wall_micros: first.elapsed_micros,
        second_index_wall_micros: second.elapsed_micros,
        first_oracle_allocated_bytes: first.oracle_allocated_bytes,
        second_oracle_allocated_bytes: second.oracle_allocated_bytes,
        lexical_index_bytes: None,
        lexical_index_size_status: "unavailable_in_memory_backend",
        peak_rss_bytes: None,
        peak_rss_status: "unavailable_portable_sampler",
        locate: QueryMeasurement::from_response(&locate),
        explain: QueryMeasurement::from_response(&explain),
        source: QueryMeasurement::from_response(&source),
        second_locate: QueryMeasurement::from_response(&second_locate),
        pinned_first: QueryMeasurement::from_response(&pinned_first),
    };
    Ok(CommandResult::FirstSliceDemo(Box::new(
        FirstSliceDemoResult {
            contract_version: FIRST_SLICE_DEMO_CONTRACT_VERSION,
            storage_mode: "ephemeral_sqlite_and_lexical",
            first_freshness: "active_at_query_time",
            retained_first_freshness: "retained_after_update",
            second_freshness: "active",
            first,
            locate,
            explain,
            source,
            second,
            second_locate,
            pinned_first,
            measurements,
        },
    )))
}

fn execute_client(
    client: &Client,
    command: &str,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    match (command, arguments) {
        ("health", []) => Ok(CommandResult::Health(client.health()?)),
        ("health", [json]) if json == "--json" => Ok(CommandResult::Health(client.health()?)),
        ("diagnostics", [quick]) if quick == "quick" => {
            Ok(CommandResult::DiagnosticsQuick(client.diagnostics_quick()?))
        }
        ("support-bundle", [output, path]) if output == "--output" => {
            let path = support_output_path(path)?;
            let bundle = client.support_bundle()?;
            write_support_bundle(path, &bundle.archive)?;
            Ok(CommandResult::SupportBundle(support_receipt(&bundle)?))
        }
        ("operation-submit", [operation]) => Ok(CommandResult::OperationSubmit(
            client.operation_submit(parse_operation(operation)?)?,
        )),
        ("operation-submit", [operation, flag, timeout_ms]) if flag == "--timeout-ms" => Ok(
            CommandResult::OperationSubmit(client.operation_submit_with_timeout(
                parse_operation(operation)?,
                Some(Duration::from_millis(parse_timeout_ms(timeout_ms)?)),
            )?),
        ),
        ("operation-submit", [operation, flag, deadline_unix_ms])
            if flag == "--deadline-unix-ms" =>
        {
            Ok(CommandResult::OperationSubmit(
                client.operation_submit_detached(
                    parse_operation(operation)?,
                    Some(parse_timestamp_ms(deadline_unix_ms)?),
                )?,
            ))
        }
        (
            "operation-submit",
            [
                operation,
                deadline_flag,
                deadline_unix_ms,
                lease_flag,
                lease_expires_unix_ms,
            ],
        ) if deadline_flag == "--deadline-unix-ms" && lease_flag == "--lease-expires-unix-ms" => {
            Ok(CommandResult::OperationSubmit(
                client.operation_submit_attached(
                    parse_operation(operation)?,
                    Some(parse_timestamp_ms(deadline_unix_ms)?),
                    parse_timestamp_ms(lease_expires_unix_ms)?,
                )?,
            ))
        }
        ("operation-submit", [operation, lease_flag, lease_expires_unix_ms])
            if lease_flag == "--lease-expires-unix-ms" =>
        {
            Ok(CommandResult::OperationSubmit(
                client.operation_submit_attached(
                    parse_operation(operation)?,
                    None,
                    parse_timestamp_ms(lease_expires_unix_ms)?,
                )?,
            ))
        }
        ("operation-status", [operation]) => Ok(CommandResult::OperationStatus(
            client.operation_status(parse_operation(operation)?)?,
        )),
        ("operation-cancel", [operation]) => {
            let (accepted, operation) = client.operation_cancel(parse_operation(operation)?)?;
            Ok(CommandResult::OperationCancel {
                accepted,
                operation,
            })
        }
        _ => Err(CliError::Usage),
    }
}

fn execute_standalone(
    paths: &RuntimePaths,
    command: &str,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    paths.prepare_owner()?;
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| CliError::RandomUnavailable)?;
    let _writer = CatalogWriterLock::acquire(&paths.writer_lock_path(), nonce)?;
    let catalog_path = paths.operation_journal_path();
    let journal = Arc::new(OperationJournal::open(&catalog_path)?);
    let limits = DaemonLimits::default();
    let state = Arc::new(DaemonState::starting());
    let actor = JournalActor::start(
        Arc::clone(&journal),
        limits.control_queue_limit(),
        usize::try_from(limits.operation_queue_limit()).map_err(|_| CliError::InvalidLimits)?,
    )?;
    let actor_handle = actor.handle();
    let mut orchestrator =
        DaemonOrchestrator::new(actor_handle.clone(), Arc::clone(&state), limits)?;
    let service = ControlService::with_state(journal, nonce, Arc::clone(&state), limits)
        .with_catalog_path(catalog_path);
    state.set_catalog_status(DomainHealthStatus::Healthy);
    state.set_endpoint_status(DomainHealthStatus::NotConfigured);
    state.set_lifecycle(DaemonLifecycle::Ready);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(CliError::AsyncRuntime)?;
    let result = runtime.block_on(execute_standalone_command(
        &service,
        &actor_handle,
        &mut orchestrator,
        command,
        arguments,
    ));
    let shutdown = runtime.block_on(orchestrator.shutdown());
    let joined = actor.join();
    result.and_then(|result| {
        shutdown?;
        joined?;
        Ok(result)
    })
}

async fn execute_standalone_command(
    service: &ControlService,
    actor: &rootlight_daemon_core::JournalActorHandle,
    orchestrator: &mut DaemonOrchestrator,
    command: &str,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    match (command, arguments) {
        ("health", []) => response_to_result(service.execute(ControlRequest::Health)),
        ("health", [json]) if json == "--json" => {
            response_to_result(service.execute(ControlRequest::Health))
        }
        ("diagnostics", [quick]) if quick == "quick" => {
            response_to_result(service.execute(ControlRequest::DiagnosticsQuick))
        }
        ("support-bundle", [output, path]) if output == "--output" => {
            let path = support_output_path(path)?;
            let response = control_response(service.execute(ControlRequest::SupportBundle(
                rootlight_observability::SupportBundleSchema::V2,
            )))?;
            let ControlResponse::SupportBundle(bundle) = response else {
                return Err(CliError::UnexpectedResponse);
            };
            let bundle = support_bundle_from_domain(bundle);
            write_support_bundle(path, &bundle.archive)?;
            Ok(CommandResult::SupportBundle(support_receipt(&bundle)?))
        }
        ("operation-submit", [operation]) => {
            submit_standalone(
                standalone_submission(parse_operation(operation)?, None)?,
                actor,
                orchestrator,
            )
            .await
        }
        ("operation-submit", [operation, flag, timeout_ms]) if flag == "--timeout-ms" => {
            submit_standalone(
                standalone_submission(
                    parse_operation(operation)?,
                    Some(parse_timeout_ms(timeout_ms)?),
                )?,
                actor,
                orchestrator,
            )
            .await
        }
        ("operation-submit", [operation, flag, deadline_unix_ms])
            if flag == "--deadline-unix-ms" =>
        {
            let submission = PreparedOperationSubmission::control_probe_timing(
                parse_operation(operation)?,
                ClientInstanceId::SYSTEM,
                true,
                Some(parse_timestamp_ms(deadline_unix_ms)?),
                None,
            )
            .map_err(operation_preparation_error)?;
            submit_standalone(submission, actor, orchestrator).await
        }
        (
            "operation-submit",
            [
                operation,
                deadline_flag,
                deadline_unix_ms,
                lease_flag,
                lease_expires_unix_ms,
            ],
        ) if deadline_flag == "--deadline-unix-ms" && lease_flag == "--lease-expires-unix-ms" => {
            let submission = PreparedOperationSubmission::control_probe_timing(
                parse_operation(operation)?,
                ClientInstanceId::SYSTEM,
                false,
                Some(parse_timestamp_ms(deadline_unix_ms)?),
                Some(parse_timestamp_ms(lease_expires_unix_ms)?),
            )
            .map_err(operation_preparation_error)?;
            submit_standalone(submission, actor, orchestrator).await
        }
        ("operation-submit", [operation, lease_flag, lease_expires_unix_ms])
            if lease_flag == "--lease-expires-unix-ms" =>
        {
            let submission = PreparedOperationSubmission::control_probe_timing(
                parse_operation(operation)?,
                ClientInstanceId::SYSTEM,
                false,
                None,
                Some(parse_timestamp_ms(lease_expires_unix_ms)?),
            )
            .map_err(operation_preparation_error)?;
            submit_standalone(submission, actor, orchestrator).await
        }
        ("operation-status", [operation]) => response_to_result(
            actor
                .control(ControlRequest::OperationStatus(parse_operation(operation)?))
                .await?,
        ),
        ("operation-cancel", [operation]) => response_to_result(
            actor
                .control(ControlRequest::OperationCancel(parse_operation(operation)?))
                .await?,
        ),
        _ => Err(CliError::Usage),
    }
}

async fn submit_standalone(
    submission: PreparedOperationSubmission,
    actor: &rootlight_daemon_core::JournalActorHandle,
    orchestrator: &mut DaemonOrchestrator,
) -> Result<CommandResult, CliError> {
    let admission = orchestrator.schedule(submission).await?;
    let terminal = await_standalone_terminal(actor, orchestrator, admission).await?;
    Ok(CommandResult::OperationSubmit(operation_from_domain(
        terminal,
    )))
}

async fn await_standalone_terminal(
    actor: &rootlight_daemon_core::JournalActorHandle,
    orchestrator: &mut DaemonOrchestrator,
    running: OperationRecord,
) -> Result<OperationRecord, CliError> {
    if running.state.is_terminal() {
        return Ok(running);
    }
    loop {
        let event = orchestrator.next_event().await?;
        if let Some(completed) = orchestrator.process_event(event).await?
            && completed.operation == running.operation
            && completed.state.is_terminal()
        {
            return Ok(completed);
        }
        let ControlResponse::OperationStatus(status) = actor
            .control(ControlRequest::OperationStatus(running.operation))
            .await?
        else {
            return Err(CliError::UnexpectedResponse);
        };
        if status.state.is_terminal() {
            return Ok(status);
        }
    }
}

fn standalone_submission(
    operation: OperationId,
    timeout_ms: Option<u64>,
) -> Result<PreparedOperationSubmission, CliError> {
    PreparedOperationSubmission::control_probe(
        operation,
        ClientInstanceId::SYSTEM,
        timeout_ms.map(Duration::from_millis),
    )
    .map_err(operation_preparation_error)
}

fn operation_preparation_error(error: OperationPreparationError) -> CliError {
    match error {
        OperationPreparationError::InvalidTimeout => CliError::InvalidTimeout,
        OperationPreparationError::Clock => CliError::Clock,
    }
}

fn response_to_result(response: ControlResponse) -> Result<CommandResult, CliError> {
    match response {
        ControlResponse::Health(health) => Ok(CommandResult::Health(health_from_domain(health))),
        ControlResponse::DiagnosticsQuick(diagnostics) => Ok(CommandResult::DiagnosticsQuick(
            diagnostics_from_domain(diagnostics),
        )),
        ControlResponse::SupportBundle(bundle) => Ok(CommandResult::SupportBundle(
            support_receipt(&support_bundle_from_domain(bundle))?,
        )),
        ControlResponse::OperationSubmit(operation) => Ok(CommandResult::OperationSubmit(
            operation_from_domain(operation),
        )),
        ControlResponse::OperationStatus(operation)
        | ControlResponse::OperationLeaseRenew(operation) => Ok(CommandResult::OperationStatus(
            operation_from_domain(operation),
        )),
        ControlResponse::OperationCancel {
            accepted,
            operation,
        } => Ok(CommandResult::OperationCancel {
            accepted,
            operation: operation_from_domain(operation),
        }),
        ControlResponse::Error(error) => Err(CliError::Public(Box::new(error))),
    }
}

fn control_response(response: ControlResponse) -> Result<ControlResponse, CliError> {
    match response {
        ControlResponse::Error(error) => Err(CliError::Public(Box::new(error))),
        response => Ok(response),
    }
}

fn health_from_domain(health: rootlight_daemon_core::Health) -> Health {
    Health {
        ready: health.ready,
        active_operations: health.active_operations,
        admitted_operations: health.admitted_operations,
        protocol_version: health.protocol_version.to_owned(),
        lifecycle: match health.lifecycle {
            DaemonLifecycle::Starting => ClientDaemonLifecycle::Starting,
            DaemonLifecycle::Ready => ClientDaemonLifecycle::Ready,
            DaemonLifecycle::Draining => ClientDaemonLifecycle::Draining,
            DaemonLifecycle::Faulted => ClientDaemonLifecycle::Faulted,
            DaemonLifecycle::Stopped => ClientDaemonLifecycle::Stopped,
        },
        accepting_operations: health.accepting_operations,
        active_connections: health.active_connections,
        connection_limit: health.connection_limit,
        queued_operations: health.queued_operations,
        running_operations: health.running_operations,
        operation_queue_limit: health.operation_queue_limit,
        journal_healthy: health.journal_healthy,
        catalog_status: health_status_from_domain(health.catalog_status),
        catalog_schema_version: health.catalog_schema_version,
        generation_status: health_status_from_domain(health.generation_status),
        adapter_status: health_status_from_domain(health.adapter_status),
        watcher_status: health_status_from_domain(health.watcher_status),
        resource_pressure: match health.resource_pressure {
            DomainResourcePressure::Normal => ClientResourcePressure::Normal,
            DomainResourcePressure::Elevated => ClientResourcePressure::Elevated,
            DomainResourcePressure::High => ClientResourcePressure::High,
            DomainResourcePressure::Critical => ClientResourcePressure::Critical,
            DomainResourcePressure::Unknown => ClientResourcePressure::Unknown,
        },
        endpoint_status: health_status_from_domain(health.endpoint_status),
        endpoint_schema_version: health.endpoint_schema_version,
    }
}

const fn health_status_from_domain(status: DomainHealthStatus) -> ClientHealthStatus {
    match status {
        DomainHealthStatus::Healthy => ClientHealthStatus::Healthy,
        DomainHealthStatus::Degraded => ClientHealthStatus::Degraded,
        DomainHealthStatus::Unavailable => ClientHealthStatus::Unavailable,
        DomainHealthStatus::NotConfigured => ClientHealthStatus::NotConfigured,
        DomainHealthStatus::Failed => ClientHealthStatus::Failed,
    }
}

fn diagnostics_from_domain(diagnostics: DomainDiagnosticsQuick) -> DiagnosticsQuick {
    DiagnosticsQuick {
        schema_version: diagnostics.schema_version,
        overall_status: health_status_from_domain(diagnostics.overall_status),
        catalog: rootlight_client::DiagnosticResult {
            outcome: match diagnostics.catalog.outcome {
                DomainDiagnosticOutcome::Passed => rootlight_client::DiagnosticOutcome::Passed,
                DomainDiagnosticOutcome::Failed => rootlight_client::DiagnosticOutcome::Failed,
                DomainDiagnosticOutcome::TimedOut => rootlight_client::DiagnosticOutcome::TimedOut,
                DomainDiagnosticOutcome::Unavailable => {
                    rootlight_client::DiagnosticOutcome::Unavailable
                }
            },
            duration_ms: diagnostics.catalog.duration_ms,
            error: diagnostics.catalog.error,
        },
    }
}

fn support_bundle_from_domain(bundle: DomainSupportBundle) -> ClientSupportBundle {
    ClientSupportBundle {
        schema_version: bundle.schema_version,
        archive: bundle.archive,
        sha256: bundle.sha256,
        archive_bytes: bundle.archive_bytes,
        contains_source: bundle.contains_source,
        telemetry: None,
    }
}

fn support_receipt(bundle: &ClientSupportBundle) -> Result<SupportBundleReceipt, CliError> {
    Ok(SupportBundleReceipt {
        schema_version: bundle.schema_version,
        archive_bytes: bundle.archive_bytes,
        sha256: hex_digest(bundle.sha256)?,
        contains_source: bundle.contains_source,
    })
}

fn hex_digest(digest: [u8; 32]) -> Result<String, CliError> {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").map_err(|_| CliError::DigestEncoding)?;
    }
    Ok(encoded)
}

fn write_support_bundle(path: &Path, archive: &[u8]) -> Result<(), CliError> {
    write_support_bundle_with_writer(path, archive, |file, bytes| file.write_all(bytes))
}

fn write_support_bundle_with_writer(
    path: &Path,
    archive: &[u8],
    write: impl FnOnce(&mut PrivateOutputFile, &[u8]) -> std::io::Result<()>,
) -> Result<(), CliError> {
    preflight_support_output()?;
    validate_support_output_path(path)?;
    let mut output = PrivateOutputFile::create(path).map_err(map_support_output_error)?;
    if let Err(source) = write(&mut output, archive) {
        return match output.abort() {
            Ok(()) => Err(CliError::SupportWrite(source)),
            Err(cleanup) => Err(CliError::SupportCleanup(cleanup)),
        };
    }
    output.commit().map_err(map_support_output_error)
}

fn preflight_support_output() -> Result<(), CliError> {
    PrivateOutputFile::preflight().map_err(map_support_output_error)
}

fn support_output_path(argument: &std::ffi::OsStr) -> Result<&Path, CliError> {
    let path = Path::new(argument);
    validate_support_output_path(path)?;
    Ok(path)
}

fn validate_support_output_path(path: &Path) -> Result<(), CliError> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt as _;

        let final_component = path
            .as_os_str()
            .as_bytes()
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or_default();
        if final_component.is_empty() || final_component == b"." || final_component == b".." {
            return Err(CliError::InvalidSupportPath);
        }
    }

    let parent = match path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent,
        None => Path::new("."),
    };
    if !parent.is_dir() || path.file_name().is_none() {
        return Err(CliError::InvalidSupportPath);
    }
    Ok(())
}

fn map_support_output_error(error: RuntimeError) -> CliError {
    match error {
        RuntimeError::Io(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            CliError::SupportOutputExists
        }
        RuntimeError::Io(source) => CliError::SupportWrite(source),
        RuntimeError::PrivateOutputSecurityPolicy(Some(source))
            if source.kind() == std::io::ErrorKind::Unsupported =>
        {
            CliError::MacosSupportOutputUnavailable
        }
        error @ RuntimeError::PrivateOutputCleanup(_) => CliError::SupportCleanup(error),
        error => CliError::Runtime(error),
    }
}

fn operation_from_domain(operation: OperationRecord) -> OperationStatus {
    OperationStatus {
        operation: operation.operation,
        state: match operation.state {
            JournalState::Queued => rootlight_client::OperationState::Queued,
            JournalState::Running => rootlight_client::OperationState::Running,
            JournalState::Cancelling => rootlight_client::OperationState::Cancelling,
            JournalState::Succeeded => rootlight_client::OperationState::Succeeded,
            JournalState::Failed => rootlight_client::OperationState::Failed,
            JournalState::Cancelled => rootlight_client::OperationState::Cancelled,
            JournalState::Interrupted => rootlight_client::OperationState::Interrupted,
        },
        revision: operation.revision,
        completed_units: operation.progress.completed,
        total_units: operation.progress.total,
        error: operation.error,
        kind: match operation.kind {
            rootlight_operations::OperationKind::ControlProbe => OperationKind::ControlProbe,
            rootlight_operations::OperationKind::RepositoryIndex => OperationKind::RepositoryIndex,
        },
        stage: match operation.stage {
            JournalStage::Accepted => OperationStage::Accepted,
            JournalStage::Executing => OperationStage::Executing,
            JournalStage::Cleanup => OperationStage::Cleanup,
        },
        plan_hash: operation.plan_hash.as_bytes(),
        detached: operation.detached,
        cancellation_requested: operation.cancellation_requested,
        deadline_unix_ms: operation.deadline_unix_ms,
        lease_expires_unix_ms: operation.lease_expires_unix_ms,
        recovery_class: match operation.recovery_class {
            JournalRecoveryClass::NotApplicable => RecoveryClass::NotApplicable,
            JournalRecoveryClass::InterruptedByRestart => RecoveryClass::InterruptedByRestart,
            JournalRecoveryClass::DeadlineElapsed => RecoveryClass::DeadlineElapsed,
            JournalRecoveryClass::LeaseExpired => RecoveryClass::LeaseExpired,
        },
    }
}

fn parse_operation(value: &std::ffi::OsString) -> Result<OperationId, CliError> {
    value
        .to_str()
        .ok_or(CliError::InvalidOperation)?
        .parse()
        .map_err(|_| CliError::InvalidOperation)
}

fn parse_timeout_ms(value: &std::ffi::OsString) -> Result<u64, CliError> {
    let milliseconds = parse_timestamp_ms(value)?;
    if u32::try_from(milliseconds).is_err() {
        return Err(CliError::InvalidTimeout);
    }
    Ok(milliseconds)
}

fn parse_timestamp_ms(value: &std::ffi::OsString) -> Result<u64, CliError> {
    let milliseconds = value
        .to_str()
        .ok_or(CliError::InvalidTimeout)?
        .parse::<u64>()
        .map_err(|_| CliError::InvalidTimeout)?;
    if milliseconds == 0 {
        return Err(CliError::InvalidTimeout);
    }
    Ok(milliseconds)
}

fn elapsed_micros(started: Instant) -> u64 {
    match u64::try_from(started.elapsed().as_micros()) {
        Ok(elapsed) => elapsed,
        Err(_) => u64::MAX,
    }
}

fn runtime_paths() -> Result<RuntimePaths, CliError> {
    match (
        env::var_os("ROOTLIGHT_STATE_DIR"),
        env::var_os("ROOTLIGHT_RUNTIME_DIR"),
    ) {
        (None, None) => RuntimePaths::resolve().map_err(CliError::Runtime),
        (Some(state), Some(runtime)) if !state.is_empty() && !runtime.is_empty() => {
            RuntimePaths::new(PathBuf::from(state), PathBuf::from(runtime))
                .map_err(CliError::Runtime)
        }
        _ => Err(CliError::IncompletePathOverride),
    }
}

#[derive(Debug, Serialize)]
struct CliEnvelope {
    contract_version: &'static str,
    ok: bool,
    exit_family: ExitFamily,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<CommandResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<PublicError>,
}

impl CliEnvelope {
    fn success(result: CommandResult) -> Self {
        Self {
            contract_version: CLI_CONTRACT_VERSION,
            ok: true,
            exit_family: ExitFamily::Success,
            result: Some(result),
            error: None,
        }
    }

    fn failure(exit_family: ExitFamily, error: PublicError) -> Self {
        Self {
            contract_version: CLI_CONTRACT_VERSION,
            ok: false,
            exit_family,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum CommandResult {
    Health(Health),
    DiagnosticsQuick(DiagnosticsQuick),
    SupportBundle(SupportBundleReceipt),
    OperationSubmit(OperationStatus),
    OperationStatus(OperationStatus),
    OperationCancel {
        accepted: bool,
        operation: OperationStatus,
    },
    FirstSliceDemo(Box<FirstSliceDemoResult>),
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct FirstSliceDemoResult {
    contract_version: &'static str,
    storage_mode: &'static str,
    first_freshness: &'static str,
    retained_first_freshness: &'static str,
    second_freshness: &'static str,
    first: FirstSliceIndexReceipt,
    locate: QueryResponse<CodeLocateResult>,
    explain: QueryResponse<SymbolExplainResult>,
    source: QueryResponse<SourceReadQueryResult>,
    second: FirstSliceIndexReceipt,
    second_locate: QueryResponse<CodeLocateResult>,
    pinned_first: QueryResponse<CodeLocateResult>,
    measurements: FirstSliceMeasurements,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct FirstSliceMeasurements {
    total_wall_micros: u64,
    first_index_wall_micros: u64,
    second_index_wall_micros: u64,
    first_oracle_allocated_bytes: u64,
    second_oracle_allocated_bytes: u64,
    lexical_index_bytes: Option<u64>,
    lexical_index_size_status: &'static str,
    peak_rss_bytes: Option<u64>,
    peak_rss_status: &'static str,
    locate: QueryMeasurement,
    explain: QueryMeasurement,
    source: QueryMeasurement,
    second_locate: QueryMeasurement,
    pinned_first: QueryMeasurement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
struct QueryMeasurement {
    elapsed_micros: u64,
    response_json_bytes: u64,
    estimated_tokens: u64,
}

impl QueryMeasurement {
    fn from_response<T>(response: &QueryResponse<T>) -> Self {
        Self {
            elapsed_micros: response.usage.elapsed_micros,
            response_json_bytes: response.usage.json_bytes,
            estimated_tokens: response.usage.estimated_tokens,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SupportBundleReceipt {
    schema_version: u32,
    archive_bytes: u64,
    sha256: String,
    contains_source: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ExitFamily {
    Success,
    Usage,
    Unavailable,
    Degraded,
    RepairRequired,
    SecurityPolicy,
    Internal,
}

impl ExitFamily {
    const fn code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Usage => 2,
            Self::Unavailable => 3,
            Self::Degraded => 4,
            Self::RepairRequired => 5,
            Self::SecurityPolicy => 6,
            Self::Internal => 70,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(
        "usage: rootlight [--standalone] first-slice-demo|health [--json]|diagnostics quick|support-bundle --output <file>|operation-submit <id> [--timeout-ms <ms>|--deadline-unix-ms <ms> [--lease-expires-unix-ms <ms>]|--lease-expires-unix-ms <ms>]|operation-status <id>|operation-cancel <id>"
    )]
    Usage,
    #[error("daemon path overrides must provide both state and runtime directories")]
    IncompletePathOverride,
    #[error("support bundle output path is invalid")]
    InvalidSupportPath,
    #[error("support-bundle output is unavailable on macOS")]
    MacosSupportOutputUnavailable,
    #[error("support bundle output already exists")]
    SupportOutputExists,
    #[error("support bundle output failed")]
    SupportWrite(#[source] std::io::Error),
    #[error("support bundle digest encoding failed")]
    DigestEncoding,
    #[error("support bundle staging cleanup failed")]
    SupportCleanup(#[source] RuntimeError),
    #[error("secure random source is unavailable")]
    RandomUnavailable,
    #[error("daemon runtime setup failed")]
    Runtime(#[from] rootlight_runtime::RuntimeError),
    #[error("operation identifier is invalid")]
    InvalidOperation,
    #[error("operation timeout is invalid")]
    InvalidTimeout,
    #[error("daemon resource limits are invalid")]
    InvalidLimits,
    #[error("standalone service returned an unexpected response")]
    UnexpectedResponse,
    #[error("system clock is before the supported epoch")]
    Clock,
    #[error("standalone async runtime setup failed")]
    AsyncRuntime(#[source] std::io::Error),
    #[error("daemon request failed")]
    Public(Box<rootlight_error::PublicError>),
    #[error("daemon client failed")]
    Client(#[from] ClientError),
    #[error("daemon orchestration failed")]
    Service(#[from] ServiceError),
    #[error("operation journal failed")]
    Operations(#[from] rootlight_operations::OperationError),
    #[error("first-slice demo failed")]
    FirstSlice(#[from] FirstSliceError),
    #[error("first-slice demo filesystem setup failed")]
    DemoIo(#[source] std::io::Error),
    #[error("first-slice demo invariant failed")]
    DemoInvariant,
}

impl CliError {
    fn exit_family(&self) -> ExitFamily {
        if let Some(error) = self.embedded_public_error() {
            return exit_family_for_code(error.code());
        }
        match self {
            Self::Usage
            | Self::IncompletePathOverride
            | Self::InvalidSupportPath
            | Self::SupportOutputExists
            | Self::InvalidOperation
            | Self::InvalidTimeout => ExitFamily::Usage,
            Self::MacosSupportOutputUnavailable => ExitFamily::Degraded,
            Self::Runtime(rootlight_runtime::RuntimeError::InsecureDirectory)
            | Self::Runtime(rootlight_runtime::RuntimeError::InvalidDiscovery)
            | Self::Runtime(rootlight_runtime::RuntimeError::InsecureEndpointArtifact)
            | Self::Runtime(rootlight_runtime::RuntimeError::InsecureLockFile)
            | Self::Runtime(rootlight_runtime::RuntimeError::InsecureOutputFile)
            | Self::Runtime(rootlight_runtime::RuntimeError::PrivateOutputSecurityPolicy(_))
            | Self::Runtime(rootlight_runtime::RuntimeError::WindowsSecurityPolicy)
            | Self::Runtime(rootlight_runtime::RuntimeError::InvalidEndpoint(_))
            | Self::Operations(rootlight_operations::OperationError::InsecureLockFile)
            | Self::Operations(rootlight_operations::OperationError::WindowsSecurityPolicy) => {
                ExitFamily::SecurityPolicy
            }
            Self::Client(ClientError::DaemonUnavailable)
            | Self::Client(ClientError::DaemonExecutableMissing)
            | Self::Client(ClientError::DaemonLaunchFailed)
            | Self::Client(ClientError::DaemonStartTimedOut)
            | Self::Operations(
                rootlight_operations::OperationError::WriterBusy
                | rootlight_operations::OperationError::Busy,
            ) => ExitFamily::Unavailable,
            Self::Client(ClientError::ProtocolMismatch)
            | Self::Client(ClientError::MissingProtocol)
            | Self::Operations(rootlight_operations::OperationError::CorruptState)
            | Self::Operations(rootlight_operations::OperationError::CorruptSchema)
            | Self::Operations(rootlight_operations::OperationError::ForeignCatalog)
            | Self::Operations(rootlight_operations::OperationError::MigrationChecksumMismatch)
            | Self::Operations(rootlight_operations::OperationError::UnsupportedLegacySchema)
            | Self::Operations(rootlight_operations::OperationError::UnsupportedSchemaVersion {
                ..
            })
            | Self::Operations(rootlight_operations::OperationError::UnsupportedSqlite {
                ..
            })
            | Self::Operations(
                rootlight_operations::OperationError::UnsupportedSqliteCompileOptions
                | rootlight_operations::OperationError::UnsupportedSqliteConfiguration,
            ) => ExitFamily::RepairRequired,
            _ => ExitFamily::Internal,
        }
    }

    fn public_error(&self) -> Result<PublicError, rootlight_error::PublicErrorBuildError> {
        if matches!(self, Self::MacosSupportOutputUnavailable) {
            return macos_support_output_public_error();
        }
        if let Some(error) = self.embedded_public_error() {
            return Ok(error.clone());
        }
        if matches!(self, Self::FirstSlice(FirstSliceError::Cancelled(_))) {
            return PublicError::builder(ErrorCode::Cancelled, "operation was cancelled").build();
        }
        let (code, message, retryable) = match self.exit_family() {
            ExitFamily::Success => (ErrorCode::Internal, "internal operation failed", false),
            ExitFamily::Usage => (
                ErrorCode::InvalidArgument,
                "command arguments are invalid",
                false,
            ),
            ExitFamily::Unavailable => (ErrorCode::Busy, "daemon is unavailable", true),
            ExitFamily::Degraded => (ErrorCode::IncompleteCoverage, "service is degraded", false),
            ExitFamily::RepairRequired => (
                ErrorCode::MigrationRequired,
                "stored state requires repair",
                false,
            ),
            ExitFamily::SecurityPolicy => (
                ErrorCode::PermissionDenied,
                "security policy denied operation",
                false,
            ),
            ExitFamily::Internal => (ErrorCode::Internal, "internal operation failed", false),
        };
        let builder = PublicError::builder(code, message);
        let builder = if retryable {
            builder.retryable()
        } else {
            builder
        };
        builder.build()
    }

    fn embedded_public_error(&self) -> Option<&PublicError> {
        match self {
            Self::Public(error) => Some(error),
            Self::Client(error) => error.as_public_error(),
            Self::Service(ServiceError::Public(error)) => Some(error),
            _ => None,
        }
    }
}

fn macos_support_output_public_error() -> Result<PublicError, rootlight_error::PublicErrorBuildError>
{
    let capability = DetailKey::parse("capability")?;
    let capability_name = SafeLabel::parse("support_bundle_output")?;
    let platform = DetailKey::parse("platform")?;
    let platform_name = SafeLabel::parse("macos")?;

    PublicError::builder(
        ErrorCode::UnsupportedCapability,
        "support-bundle output is unavailable on macos",
    )
    .detail(capability, PublicValue::Label(capability_name))
    .detail(platform, PublicValue::Label(platform_name))
    .build()
}

const fn exit_family_for_code(code: ErrorCode) -> ExitFamily {
    match code {
        ErrorCode::InvalidArgument => ExitFamily::Usage,
        ErrorCode::IncompleteCoverage | ErrorCode::UnsupportedCapability => ExitFamily::Degraded,
        ErrorCode::IndexCorrupt | ErrorCode::MigrationRequired => ExitFamily::RepairRequired,
        ErrorCode::PermissionDenied => ExitFamily::SecurityPolicy,
        ErrorCode::Busy | ErrorCode::ResourceExhausted | ErrorCode::ProtocolMismatch => {
            ExitFamily::Unavailable
        }
        ErrorCode::Internal
        | ErrorCode::NotFound
        | ErrorCode::Conflict
        | ErrorCode::StaleGeneration
        | ErrorCode::BudgetExceeded
        | ErrorCode::Cancelled
        | ErrorCode::AdapterFailed => ExitFamily::Internal,
        _ => ExitFamily::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SupportTempdir {
        _owner: tempfile::TempDir,
        path: PathBuf,
    }

    impl SupportTempdir {
        fn path(&self) -> &Path {
            &self.path
        }
    }

    fn support_tempdir() -> SupportTempdir {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
                .expect("temporary directory becomes private");
        }
        #[cfg(target_os = "macos")]
        let path =
            std::fs::canonicalize(temporary.path()).expect("temporary directory canonicalizes");
        #[cfg(not(target_os = "macos"))]
        let path = temporary.path().to_path_buf();
        SupportTempdir {
            _owner: temporary,
            path,
        }
    }

    fn operation_status() -> OperationStatus {
        OperationStatus {
            operation: OperationId::from_bytes([7; 16]),
            state: rootlight_client::OperationState::Running,
            revision: 3,
            completed_units: 0,
            total_units: 0,
            error: None,
            kind: OperationKind::ControlProbe,
            stage: OperationStage::Executing,
            plan_hash: [0; 32],
            detached: true,
            cancellation_requested: false,
            deadline_unix_ms: None,
            lease_expires_unix_ms: None,
            recovery_class: RecoveryClass::NotApplicable,
        }
    }

    #[test]
    fn operation_result_discriminator_does_not_collide_with_operation_kind() {
        let envelope = CliEnvelope::success(CommandResult::OperationStatus(operation_status()));
        let json = serde_json::to_value(envelope).expect("CLI envelope serializes");

        assert_eq!(json["contract_version"], "1.0");
        assert_eq!(json["result"]["type"], "operation_status");
        assert_eq!(json["result"]["data"]["kind"], "control_probe");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn support_command_preflight_preserves_supported_dispatch() {
        let arguments = [
            std::ffi::OsString::from("--output"),
            std::ffi::OsString::from("support.zip"),
        ];
        let mut dispatched = false;

        dispatch_after_command_preflight(
            false,
            std::ffi::OsStr::new("support-bundle"),
            &arguments,
            |_| {
                dispatched = true;
                Ok(())
            },
        )
        .expect("supported platform dispatches");

        assert!(dispatched);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn support_bundle_write_is_private_and_refuses_overwrite() {
        let temporary = support_tempdir();
        let output = temporary.path().join("support.zip");
        write_support_bundle(&output, b"bundle").expect("bundle writes");
        assert_eq!(std::fs::read(&output).expect("bundle reads"), b"bundle");
        let replacement = write_support_bundle(&output, b"replacement");
        assert!(matches!(replacement, Err(CliError::SupportOutputExists)));
        assert_eq!(
            std::fs::read(&output).expect("bundle still reads"),
            b"bundle"
        );

        let raced = temporary.path().join("raced.zip");
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let writers = [
            (b"first".as_slice(), Arc::clone(&barrier)),
            (b"second".as_slice(), Arc::clone(&barrier)),
        ]
        .into_iter()
        .map(|(contents, barrier)| {
            let raced = raced.clone();
            std::thread::spawn(move || {
                barrier.wait();
                write_support_bundle(&raced, contents)
            })
        })
        .collect::<Vec<_>>();
        barrier.wait();
        let results = writers
            .into_iter()
            .map(|writer| writer.join().expect("support writer joins"))
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(CliError::SupportOutputExists)))
                .count(),
            1
        );
        let raced_contents = std::fs::read(&raced).expect("winning bundle reads");
        assert!(matches!(raced_contents.as_slice(), b"first" | b"second"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            let mode = std::fs::metadata(&output)
                .expect("bundle metadata reads")
                .mode();
            assert_eq!(mode & 0o077, 0);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn support_output_argument_rejects_raw_directory_aliases() {
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let temporary = support_tempdir();
        for suffix in [
            b"trailing/".as_slice(),
            b"current/.".as_slice(),
            b"parent/..".as_slice(),
        ] {
            let mut raw = temporary.path().as_os_str().as_bytes().to_vec();
            raw.push(b'/');
            raw.extend_from_slice(suffix);
            let argument = std::ffi::OsString::from_vec(raw);
            assert!(argument.as_os_str().as_bytes().ends_with(suffix));

            let error =
                support_output_path(&argument).expect_err("raw directory alias is rejected");
            assert!(matches!(error, CliError::InvalidSupportPath));
            let envelope = CliEnvelope::failure(
                error.exit_family(),
                error
                    .public_error()
                    .expect("closed invalid-path template is valid"),
            );
            let json = serde_json::to_value(envelope).expect("CLI envelope serializes");
            assert_eq!(json["exit_family"], "usage");
            assert_eq!(json["error"]["code"], "INVALID_ARGUMENT");
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn support_bundle_write_failure_leaves_private_reserved_output() {
        let temporary = support_tempdir();
        let output = temporary.path().join("partial.zip");
        let error = write_support_bundle_with_writer(&output, b"complete", |file, _| {
            file.write_all(b"partial")?;
            Err(std::io::Error::other("injected support write failure"))
        })
        .expect_err("injected write fails");
        assert!(matches!(error, CliError::SupportWrite(_)));
        assert_eq!(
            std::fs::read(&output).expect("reserved output reads"),
            b"partial"
        );
        assert!(matches!(
            write_support_bundle(&output, b"replacement"),
            Err(CliError::SupportOutputExists)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn support_command_preflight_stops_all_downstream_effects() {
        #[derive(Debug, Default, PartialEq, Eq)]
        struct ObservedEffects {
            runtime_resolution: bool,
            random_generation: bool,
            client_connection: bool,
            standalone_preparation: bool,
            service_generation: bool,
            writer_callback: bool,
        }

        let arguments = [
            std::ffi::OsString::from("--output"),
            std::ffi::OsString::from("path-that-must-not-be-inspected/support.zip"),
        ];
        let expected = unsupported_support_output_signature(
            &preflight_support_output().expect_err("macOS support output preflight fails closed"),
        );

        for standalone in [false, true] {
            let mut effects = ObservedEffects::default();
            let error = dispatch_after_command_preflight(
                standalone,
                std::ffi::OsStr::new("support-bundle"),
                &arguments,
                |standalone| {
                    effects.runtime_resolution = true;
                    effects.random_generation = true;
                    if standalone {
                        effects.standalone_preparation = true;
                    } else {
                        effects.client_connection = true;
                    }
                    effects.service_generation = true;
                    effects.writer_callback = true;
                    Ok(())
                },
            )
            .expect_err("macOS support command fails before dispatch");

            assert_eq!(effects, ObservedEffects::default());
            assert_eq!(unsupported_support_output_signature(&error), expected);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn support_writer_preflight_does_not_inspect_any_path_class() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let temporary = support_tempdir();
        let existing = temporary.path().join("existing.zip");
        std::fs::write(&existing, b"existing").expect("existing output writes");
        let symlinked = temporary.path().join("symlinked.zip");
        symlink(&existing, &symlinked).expect("output symlink creates");
        let unreadable_parent = temporary.path().join("unreadable");
        std::fs::create_dir(&unreadable_parent).expect("unreadable parent creates");
        let unreadable = unreadable_parent.join("support.zip");
        std::fs::set_permissions(&unreadable_parent, std::fs::Permissions::from_mode(0o000))
            .expect("unreadable parent permissions apply");
        let nonexistent = temporary
            .path()
            .join("nonexistent-parent")
            .join("support.zip");
        let invalid = PathBuf::new();
        let paths = [&invalid, &nonexistent, &unreadable, &symlinked, &existing];
        let expected = unsupported_support_output_signature(
            &preflight_support_output().expect_err("macOS support output preflight fails closed"),
        );
        assert_eq!(expected.0, ExitFamily::Degraded);
        assert_eq!(expected.1, ErrorCode::UnsupportedCapability);
        assert!(
            !expected
                .2
                .contains(temporary.path().to_string_lossy().as_ref())
        );

        for path in paths {
            let mut writer_called = false;
            let error = write_support_bundle_with_writer(path, b"bundle", |_, _| {
                writer_called = true;
                Ok(())
            })
            .expect_err("macOS support publication fails before path validation");

            assert!(!writer_called);
            assert_eq!(unsupported_support_output_signature(&error), expected);
        }
        std::fs::set_permissions(&unreadable_parent, std::fs::Permissions::from_mode(0o700))
            .expect("unreadable parent permissions restore");
        assert_eq!(
            std::fs::read(&existing).expect("existing output reads"),
            b"existing"
        );
        assert!(
            std::fs::symlink_metadata(&symlinked)
                .expect("output symlink metadata reads")
                .file_type()
                .is_symlink()
        );
        assert!(!nonexistent.exists());
        assert!(!unreadable.exists());
    }

    #[cfg(target_os = "macos")]
    fn unsupported_support_output_signature(error: &CliError) -> (ExitFamily, ErrorCode, String) {
        match error {
            CliError::MacosSupportOutputUnavailable => {
                let public = error
                    .public_error()
                    .expect("closed macOS capability template is valid");
                (
                    error.exit_family(),
                    public.code(),
                    serde_json::to_string(&public).expect("public error serializes"),
                )
            }
            other => panic!("unexpected macOS support error: {other:?}"),
        }
    }

    #[test]
    fn macos_support_output_error_is_stable_capability_specific_and_path_redacted() {
        let path_marker = "private/secret/support.zip";
        let error = map_support_output_error(RuntimeError::PrivateOutputSecurityPolicy(Some(
            std::io::Error::new(std::io::ErrorKind::Unsupported, path_marker),
        )));
        assert!(matches!(error, CliError::MacosSupportOutputUnavailable));
        let envelope = CliEnvelope::failure(
            error.exit_family(),
            error
                .public_error()
                .expect("closed macOS capability template is valid"),
        );
        let json = serde_json::to_value(envelope).expect("CLI envelope serializes");
        let encoded = serde_json::to_string(&json).expect("CLI envelope renders");

        assert_eq!(json["contract_version"], "1.0");
        assert_eq!(json["ok"], false);
        assert_eq!(json["exit_family"], "degraded");
        assert_eq!(json["error"]["code"], "UNSUPPORTED_CAPABILITY");
        assert_eq!(
            json["error"]["message"],
            "support-bundle output is unavailable on macos"
        );
        assert_eq!(json["error"]["retryable"], false);
        assert_eq!(json["error"]["details"]["capability"]["type"], "label");
        assert_eq!(
            json["error"]["details"]["capability"]["value"],
            "support_bundle_output"
        );
        assert_eq!(json["error"]["details"]["platform"]["type"], "label");
        assert_eq!(json["error"]["details"]["platform"]["value"], "macos");
        assert!(!encoded.contains(path_marker));
        assert!(json.get("result").is_none());
    }

    #[test]
    fn public_failures_use_the_versioned_error_envelope() {
        let error = CliError::InvalidOperation;
        let envelope = CliEnvelope::failure(
            error.exit_family(),
            error
                .public_error()
                .expect("closed public error template is valid"),
        );
        let json = serde_json::to_value(envelope).expect("CLI envelope serializes");

        assert_eq!(json["contract_version"], "1.0");
        assert_eq!(json["ok"], false);
        assert_eq!(json["exit_family"], "usage");
        assert_eq!(json["error"]["code"], "INVALID_ARGUMENT");
        assert!(json.get("result").is_none());
    }

    #[test]
    fn first_slice_cancellation_preserves_the_public_error_family() {
        let error = CliError::FirstSlice(FirstSliceError::Cancelled(
            CancellationReason::DeadlineExceeded,
        ));

        assert_eq!(
            error
                .public_error()
                .expect("closed cancellation template is valid")
                .code(),
            ErrorCode::Cancelled
        );
    }

    #[test]
    fn cli_json_buffer_enforces_the_complete_output_limit() {
        let mut buffer = BoundedJsonBuffer::new(3);
        buffer.write_all(b"abc").expect("exact limit fits");
        assert!(buffer.write_all(b"d").is_err());
        assert_eq!(buffer.into_bytes(), b"abc");
    }

    #[test]
    fn support_cleanup_failure_is_reported_as_a_distinct_internal_error() {
        let error = map_support_output_error(RuntimeError::PrivateOutputCleanup(
            std::io::Error::other("injected support cleanup failure"),
        ));

        assert!(matches!(error, CliError::SupportCleanup(_)));
        let envelope = CliEnvelope::failure(
            error.exit_family(),
            error
                .public_error()
                .expect("closed cleanup error template is valid"),
        );
        let json = serde_json::to_value(envelope).expect("CLI envelope serializes");
        assert_eq!(json["exit_family"], "internal");
        assert_eq!(json["error"]["code"], "INTERNAL");
    }
}
