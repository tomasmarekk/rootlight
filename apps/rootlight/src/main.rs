//! Rootlight command-line entry point.
//!
//! Argument parsing and JSON rendering stay at this edge; daemon and standalone
//! modes execute the same typed control and orchestration contracts.

#![forbid(unsafe_code)]

use std::{env, path::PathBuf, process::ExitCode, sync::Arc, time::Duration};

use rootlight_client::{
    Client, ClientError, ConnectPolicy, DaemonLifecycle as ClientDaemonLifecycle, Health,
    OperationKind, OperationStage, OperationStatus, RecoveryClass,
};
use rootlight_daemon_core::{
    ControlRequest, ControlResponse, ControlService, DaemonLifecycle, DaemonLimits,
    DaemonOrchestrator, DaemonState, JournalActor, ServiceError,
};
use rootlight_error::{ErrorCode, PublicError};
use rootlight_ids::OperationId;
use rootlight_operations::{
    CatalogWriterLock, ClientInstanceId, OperationJournal, OperationRecord,
    OperationStage as JournalStage, OperationState as JournalState, OperationSubmission, PlanHash,
    RecoveryClass as JournalRecoveryClass,
};
use rootlight_runtime::RuntimePaths;
use serde::Serialize;

const CLI_CONTRACT_VERSION: &str = "1.0";
const CONTROL_PROBE_PLAN_HASH: [u8; 32] = [0; 32];

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
            let envelope = CliEnvelope::failure(exit, error.public_error());
            match render_json(&envelope) {
                Ok(json) => eprintln!("{json}"),
                Err(()) => eprintln!("rootlight: output serialization failed"),
            }
            ExitCode::from(exit.code())
        }
    }
}

fn render_json(value: &CliEnvelope) -> Result<String, ()> {
    serde_json::to_string(value).map_err(|_| ())
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

    let paths = runtime_paths()?;
    if standalone {
        execute_standalone(&paths, command.to_string_lossy().as_ref(), &trailing)
    } else {
        let mut client_instance_id = [0_u8; 16];
        getrandom::fill(&mut client_instance_id).map_err(|_| CliError::RandomUnavailable)?;
        let client =
            Client::connect_or_start(&paths, client_instance_id, ConnectPolicy::StartIfMissing)?;
        execute_client(&client, command.to_string_lossy().as_ref(), &trailing)
    }
}

fn execute_client(
    client: &Client,
    command: &str,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    match (command, arguments) {
        ("health", []) => Ok(CommandResult::Health(client.health()?)),
        ("operation-submit", [operation]) => Ok(CommandResult::OperationSubmit(
            client.operation_submit(parse_operation(operation)?)?,
        )),
        ("operation-submit", [operation, flag, timeout_ms]) if flag == "--timeout-ms" => Ok(
            CommandResult::OperationSubmit(client.operation_submit_with_timeout(
                parse_operation(operation)?,
                Some(Duration::from_millis(parse_timeout_ms(timeout_ms)?)),
            )?),
        ),
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
    let journal = Arc::new(OperationJournal::open(&paths.operation_journal_path())?);
    let limits = DaemonLimits::default();
    let state = Arc::new(DaemonState::starting());
    let actor = JournalActor::start(
        Arc::clone(&journal),
        limits.control_queue_limit,
        usize::try_from(limits.operation_queue_limit).map_err(|_| CliError::InvalidLimits)?,
    )?;
    let mut orchestrator = DaemonOrchestrator::new(actor.handle(), Arc::clone(&state), limits)?;
    let service = ControlService::with_state(journal, nonce, Arc::clone(&state), limits);
    state.set_lifecycle(DaemonLifecycle::Ready);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(CliError::AsyncRuntime)?;
    let result = runtime.block_on(execute_standalone_command(
        &service,
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
    orchestrator: &mut DaemonOrchestrator,
    command: &str,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    match (command, arguments) {
        ("health", []) => response_to_result(service.execute(ControlRequest::Health)),
        ("operation-submit", [operation]) => {
            let submission = standalone_submission(parse_operation(operation)?, None)?;
            let admission = orchestrator.schedule(submission).await?;
            await_standalone_terminal(service, orchestrator, admission.clone()).await?;
            Ok(CommandResult::OperationSubmit(operation_from_domain(
                admission,
            )))
        }
        ("operation-submit", [operation, flag, timeout_ms]) if flag == "--timeout-ms" => {
            let submission = standalone_submission(
                parse_operation(operation)?,
                Some(parse_timeout_ms(timeout_ms)?),
            )?;
            let admission = orchestrator.schedule(submission).await?;
            await_standalone_terminal(service, orchestrator, admission.clone()).await?;
            Ok(CommandResult::OperationSubmit(operation_from_domain(
                admission,
            )))
        }
        ("operation-status", [operation]) => response_to_result(
            service.execute(ControlRequest::OperationStatus(parse_operation(operation)?)),
        ),
        ("operation-cancel", [operation]) => response_to_result(
            service.execute(ControlRequest::OperationCancel(parse_operation(operation)?)),
        ),
        _ => Err(CliError::Usage),
    }
}

async fn await_standalone_terminal(
    service: &ControlService,
    orchestrator: &mut DaemonOrchestrator,
    running: OperationRecord,
) -> Result<OperationRecord, CliError> {
    if running.state.is_terminal() {
        return Ok(running);
    }
    loop {
        let maintenance = tokio::time::sleep(service.limits().maintenance_interval);
        tokio::pin!(maintenance);
        tokio::select! {
            completion = orchestrator.complete_next() => {
                return completion?.ok_or(CliError::MissingCompletion);
            }
            () = &mut maintenance => {
                orchestrator.maintain().await?;
                let status = control_response(service.execute(
                    ControlRequest::OperationStatus(running.operation),
                ))?;
                let ControlResponse::OperationStatus(status) = status else {
                    return Err(CliError::UnexpectedResponse);
                };
                if status.state.is_terminal() {
                    return Ok(status);
                }
            }
        }
    }
}

fn standalone_submission(
    operation: OperationId,
    timeout_ms: Option<u64>,
) -> Result<OperationSubmission, CliError> {
    OperationSubmission::new(
        operation,
        rootlight_operations::OperationKind::ControlProbe,
        PlanHash::from_bytes(CONTROL_PROBE_PLAN_HASH),
        ClientInstanceId::SYSTEM,
        true,
        timeout_ms.map(operation_deadline).transpose()?,
        None,
    )
    .map_err(|_| CliError::InvalidTimeout)
}

fn response_to_result(response: ControlResponse) -> Result<CommandResult, CliError> {
    match control_response(response)? {
        ControlResponse::Health(health) => Ok(CommandResult::Health(health_from_domain(health))),
        ControlResponse::OperationSubmit(operation) => Ok(CommandResult::OperationSubmit(
            operation_from_domain(operation),
        )),
        ControlResponse::OperationStatus(operation) => Ok(CommandResult::OperationStatus(
            operation_from_domain(operation),
        )),
        ControlResponse::OperationCancel {
            accepted,
            operation,
        } => Ok(CommandResult::OperationCancel {
            accepted,
            operation: operation_from_domain(operation),
        }),
        ControlResponse::Error(_) => unreachable!("control_response rejects public errors"),
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
    let milliseconds = value
        .to_str()
        .ok_or(CliError::InvalidTimeout)?
        .parse::<u64>()
        .map_err(|_| CliError::InvalidTimeout)?;
    if milliseconds == 0 || u32::try_from(milliseconds).is_err() {
        return Err(CliError::InvalidTimeout);
    }
    Ok(milliseconds)
}

fn operation_deadline(timeout_ms: u64) -> Result<u64, CliError> {
    unix_time_ms()?
        .checked_add(timeout_ms)
        .ok_or(CliError::InvalidTimeout)
}

fn unix_time_ms() -> Result<u64, CliError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CliError::Clock)?;
    u64::try_from(elapsed.as_millis()).map_err(|_| CliError::Clock)
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum CommandResult {
    Health(Health),
    OperationSubmit(OperationStatus),
    OperationStatus(OperationStatus),
    OperationCancel {
        accepted: bool,
        operation: OperationStatus,
    },
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
        "usage: rootlight [--standalone] health|operation-submit <id> [--timeout-ms <ms>]|operation-status <id>|operation-cancel <id>"
    )]
    Usage,
    #[error("daemon path overrides must provide both state and runtime directories")]
    IncompletePathOverride,
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
    #[error("standalone operation completion is missing")]
    MissingCompletion,
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
}

impl CliError {
    fn exit_family(&self) -> ExitFamily {
        if let Some(error) = self.embedded_public_error() {
            return exit_family_for_code(error.code());
        }
        match self {
            Self::Usage
            | Self::IncompletePathOverride
            | Self::InvalidOperation
            | Self::InvalidTimeout => ExitFamily::Usage,
            Self::Runtime(rootlight_runtime::RuntimeError::InsecureDirectory)
            | Self::Runtime(rootlight_runtime::RuntimeError::InvalidDiscovery)
            | Self::Runtime(rootlight_runtime::RuntimeError::InsecureEndpointArtifact)
            | Self::Runtime(rootlight_runtime::RuntimeError::InsecureLockFile)
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
            | Self::Operations(rootlight_operations::OperationError::WriterBusy) => {
                ExitFamily::Unavailable
            }
            Self::Client(ClientError::ProtocolMismatch)
            | Self::Client(ClientError::MissingProtocol)
            | Self::Operations(rootlight_operations::OperationError::CorruptState)
            | Self::Operations(rootlight_operations::OperationError::CorruptSchema)
            | Self::Operations(rootlight_operations::OperationError::UnsupportedLegacySchema)
            | Self::Operations(rootlight_operations::OperationError::UnsupportedSchemaVersion {
                ..
            })
            | Self::Operations(rootlight_operations::OperationError::UnsupportedSqlite {
                ..
            })
            | Self::Operations(
                rootlight_operations::OperationError::UnsupportedSqliteCompileOptions,
            ) => ExitFamily::RepairRequired,
            _ => ExitFamily::Internal,
        }
    }

    fn public_error(&self) -> PublicError {
        if let Some(error) = self.embedded_public_error() {
            return error.clone();
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
        builder
            .build()
            .unwrap_or_else(|_| unreachable!("closed CLI error templates are statically bounded"))
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

    #[test]
    fn public_failures_use_the_versioned_error_envelope() {
        let error = CliError::InvalidOperation;
        let envelope = CliEnvelope::failure(error.exit_family(), error.public_error());
        let json = serde_json::to_value(envelope).expect("CLI envelope serializes");

        assert_eq!(json["contract_version"], "1.0");
        assert_eq!(json["ok"], false);
        assert_eq!(json["exit_family"], "usage");
        assert_eq!(json["error"]["code"], "INVALID_ARGUMENT");
        assert!(json.get("result").is_none());
    }
}
