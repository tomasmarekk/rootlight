//! Rootlight command-line entry point.
//!
//! Argument parsing and JSON rendering stay at this edge; daemon and standalone
//! modes execute the same typed control and orchestration contracts.

#![forbid(unsafe_code)]

use std::{
    env, fs,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_client::{
    Client, ClientError, ConnectPolicy, DaemonLifecycle as ClientDaemonLifecycle,
    DetachedArtifactSignature, DetachedUpdateSignature, DiagnosticsQuick, FilesystemUpdateError,
    FilesystemUpdateOutcome, Health, HealthStatus as ClientHealthStatus, MAX_UPDATE_ARTIFACT_BYTES,
    MAX_UPDATE_LICENSE_BUNDLE_BYTES, MAX_UPDATE_METADATA_BYTES, MAX_UPDATE_PROVENANCE_BYTES,
    MAX_UPDATE_SBOM_BYTES, OperationKind, OperationStage, OperationStatus, PackageInstallOutcome,
    PackageUninstallOutcome, ProcessCandidateHealthCheck, RecoveryClass,
    ResourcePressure as ClientResourcePressure, SupportBundle as ClientSupportBundle,
    TrustedUpdatePolicy, UPDATE_HEALTH_STATE_DIR_ENV, UpdateContext, UpdateInputPaths,
    UpdatePublicKey, UpdateRuntimeStatus, UpdateSignatures, UpdateSupportingEvidence,
    VerifiedUpdate, apply_update_package, install_package_with_policy, recover_update,
    uninstall_package, update_runtime_status, verify_update_with_evidence,
};
use rootlight_daemon_core::{
    ControlRequest, ControlResponse, ControlService, DaemonLifecycle, DaemonLimits,
    DaemonOrchestrator, DaemonState, DiagnosticOutcome as DomainDiagnosticOutcome,
    DiagnosticsQuick as DomainDiagnosticsQuick, HealthStatus as DomainHealthStatus, JournalActor,
    OperationPreparationError, PreparedOperationSubmission,
    ResourcePressure as DomainResourcePressure, ServiceError, SupportBundle as DomainSupportBundle,
};
use rootlight_error::{ErrorCode, PublicError};
use rootlight_ids::{ContentHash, GenerationId, OperationId, RepositoryId, content_hash};
use rootlight_operations::{
    CancellationAuthority, CatalogWriterLock, ClientInstanceId, GenerationRepairCandidate,
    MAX_REPAIR_CANDIDATES, OperationJournal, OperationRecord, OperationStage as JournalStage,
    OperationState as JournalState, RecoveryClass as JournalRecoveryClass, RepairAction,
    RepairPlan, plan_catalog_repair,
};
use rootlight_runtime::{PrivateOutputFile, RuntimeError, RuntimePaths};
#[cfg(test)]
use rootlight_service::CancellationReason;
use rootlight_service::{
    Cancellation, CodeLocateResult, FirstSliceError, FirstSliceIndexReceipt, FirstSliceService,
    LocateMode, QueryResponse, RUNTIME_TRACE_SCHEMA_VERSION, RuntimeTraceLimits,
    RuntimeTraceOverlay, SharedGenerationExpectation, SharedGenerationLimits,
    SourceReadQueryResult, SymbolExplainResult,
};
use serde::{Deserialize, Serialize};

const CLI_CONTRACT_VERSION: &str = "1.0";
// Local IPC authenticates the OS account. Short-lived operation commands then
// declare one stable CLI identity so a later invocation can cancel work from an
// earlier invocation. MCP and library clients keep their independent identities.
const CLI_CLIENT_INSTANCE_ID: [u8; 16] = *b"rootlight-cli-v1";
const FIRST_SLICE_DEMO_CONTRACT_VERSION: &str = "1.0";
const HARD_MAX_CLI_JSON_BYTES: usize = 4 * 1024 * 1024;
const SHARED_GENERATION_RECEIPT_SCHEMA: &str = "rootlight.shared-generation-receipt/1";
const RUNTIME_TRACE_RECEIPT_SCHEMA: &str = "rootlight.runtime-trace-receipt/1";
const SHARED_GENERATION_RETENTION: usize = 8;
const MAX_REPAIR_INVENTORY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_UPDATE_CONTEXT_BYTES: u64 = 64 * 1024;
const MAX_UPDATE_KEY_FILE_BYTES: u64 = 256;
const MAX_UPDATE_CHECKSUM_FILE_BYTES: u64 = 512;
const UPDATE_CATALOG_SCHEMA: u32 = 3;
const UPDATE_PROTOCOL_MAJOR: u32 = 1;
const UPDATE_PROTOCOL_MINOR: u32 = 7;
const INSTALL_VERSIONS_DIRECTORY: &str = "versions";
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

    if command == "--update-health-probe" && trailing.is_empty() && !standalone {
        return execute_update_health_probe();
    }
    if command == "first-slice-demo" {
        return execute_first_slice_demo(&trailing);
    }
    if command == "repair" {
        return execute_repair(&runtime_paths()?, &trailing);
    }
    if command == "update" {
        return execute_update(&trailing);
    }
    if command == "generation-import" {
        return execute_generation_import(&trailing);
    }
    if (command == "generation-export" || command == "runtime-trace-import") && !standalone {
        return Err(CliError::Usage);
    }

    dispatch_after_command_preflight(standalone, &command, &trailing, |standalone| {
        let paths = runtime_paths()?;
        if standalone {
            execute_standalone(&paths, command.to_string_lossy().as_ref(), &trailing)
        } else {
            let client_instance_id = if command == "operation-submit"
                || command == "operation-status"
                || command == "operation-cancel"
            {
                CLI_CLIENT_INSTANCE_ID
            } else {
                let mut identity = [0_u8; 16];
                getrandom::fill(&mut identity).map_err(|_| CliError::RandomUnavailable)?;
                identity
            };
            let client = Client::connect_or_start(
                &paths,
                client_instance_id,
                ConnectPolicy::StartIfMissing,
            )?;
            execute_client(&client, command.to_string_lossy().as_ref(), &trailing)
        }
    })
}

fn execute_repair(
    paths: &RuntimePaths,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    let (action, candidates) = match arguments {
        [dry_run, action] if dry_run == "--dry-run" => (parse_repair_action(action)?, Vec::new()),
        [dry_run, action, inventory_flag, inventory_path]
            if dry_run == "--dry-run" && inventory_flag == "--inventory" =>
        {
            (
                parse_repair_action(action)?,
                read_repair_inventory(Path::new(inventory_path))?,
            )
        }
        _ => return Err(CliError::Usage),
    };
    if action == RepairAction::ReconstructCatalogFromManifests && candidates.is_empty() {
        return Err(CliError::InvalidRepairInventory);
    }
    if action != RepairAction::ReconstructCatalogFromManifests && !candidates.is_empty() {
        return Err(CliError::Usage);
    }
    Ok(CommandResult::RepairPlan(plan_catalog_repair(
        &paths.operation_journal_path(),
        action,
        &candidates,
    )?))
}

fn parse_repair_action(argument: &std::ffi::OsStr) -> Result<RepairAction, CliError> {
    match argument.to_str() {
        Some("verify-catalog") => Ok(RepairAction::VerifyCatalog),
        Some("verify-generation-headers") => Ok(RepairAction::VerifyGenerationHeaders),
        Some("full-scrub") => Ok(RepairAction::FullScrub),
        Some("select-last-good-generation") => Ok(RepairAction::SelectLastGoodGeneration),
        Some("rebuild-lexical-index") => Ok(RepairAction::RebuildLexicalIndex),
        Some("rebuild-derived-overlays") => Ok(RepairAction::RebuildDerivedOverlays),
        Some("rebuild-repository") => Ok(RepairAction::RebuildRepository),
        Some("reconstruct-catalog") => Ok(RepairAction::ReconstructCatalogFromManifests),
        Some("purge-quarantine") => Ok(RepairAction::PurgeQuarantine),
        _ => Err(CliError::Usage),
    }
}

fn read_repair_inventory(path: &Path) -> Result<Vec<GenerationRepairCandidate>, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(CliError::RepairInventoryRead)?;
    if !metadata.file_type().is_file() {
        return Err(CliError::InvalidRepairInventory);
    }
    if metadata.len() > MAX_REPAIR_INVENTORY_BYTES {
        return Err(CliError::RepairInventoryTooLarge);
    }
    let mut bytes = Vec::new();
    let maximum = MAX_REPAIR_INVENTORY_BYTES
        .checked_add(1)
        .ok_or(CliError::RepairInventoryTooLarge)?;
    fs::File::open(path)
        .map_err(CliError::RepairInventoryRead)?
        .take(maximum)
        .read_to_end(&mut bytes)
        .map_err(CliError::RepairInventoryRead)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_REPAIR_INVENTORY_BYTES) {
        return Err(CliError::RepairInventoryTooLarge);
    }
    let inventory: RepairInventory =
        serde_json::from_slice(&bytes).map_err(|_| CliError::InvalidRepairInventory)?;
    if inventory.schema_version != rootlight_operations::REPAIR_SCHEMA_VERSION
        || inventory.candidates.len() > MAX_REPAIR_CANDIDATES
    {
        return Err(CliError::InvalidRepairInventory);
    }
    Ok(inventory.candidates)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RepairInventory {
    schema_version: String,
    candidates: Vec<GenerationRepairCandidate>,
}

fn execute_update(arguments: &[std::ffi::OsString]) -> Result<CommandResult, CliError> {
    match arguments {
        [
            install,
            root_flag,
            install_root,
            artifact_flag,
            artifact_path,
            checksum_flag,
            checksum_path,
            public_key_flag,
            public_key_path,
            key_id_flag,
            key_id,
            channel_flag,
            channel,
        ] if install == "install"
            && root_flag == "--root"
            && artifact_flag == "--artifact"
            && checksum_flag == "--checksum"
            && public_key_flag == "--public-key"
            && key_id_flag == "--key-id"
            && channel_flag == "--channel" =>
        {
            let artifact = PathBuf::from(artifact_path);
            let checksum = read_package_checksum(Path::new(checksum_path), artifact.as_path())?;
            let public_key =
                read_update_hex(Path::new(public_key_path), MAX_UPDATE_KEY_FILE_BYTES)?;
            let policy = TrustedUpdatePolicy::new(
                true,
                key_id
                    .to_str()
                    .ok_or(CliError::InvalidUpdateInput)?
                    .to_owned(),
                UpdatePublicKey::from_hex(&public_key)?,
                channel
                    .to_str()
                    .ok_or(CliError::InvalidUpdateInput)?
                    .to_owned(),
                UPDATE_CATALOG_SCHEMA,
                UPDATE_PROTOCOL_MAJOR,
                UPDATE_PROTOCOL_MINOR,
                0,
            )?;
            Ok(CommandResult::UpdateInstalled(install_package_with_policy(
                Path::new(install_root),
                &artifact,
                &checksum,
                &policy,
            )?))
        }
        [uninstall, root_flag, install_root]
            if uninstall == "uninstall" && root_flag == "--root" =>
        {
            Ok(CommandResult::UpdateUninstalled(uninstall_package(
                Path::new(install_root),
            )?))
        }
        [status] if status == "status" => {
            let install_root = installed_root_from_current_executable()?;
            Ok(CommandResult::UpdateStatus(update_runtime_status(
                &install_root,
            )?))
        }
        [recover] if recover == "recover" => {
            let install_root = installed_root_from_current_executable()?;
            Ok(CommandResult::UpdateRecovered(recover_update(
                &install_root,
            )?))
        }
        [
            apply,
            metadata_flag,
            metadata_path,
            metadata_signature_flag,
            metadata_signature_path,
            artifact_signature_flag,
            artifact_signature_path,
            artifact_flag,
            artifact_path,
            sbom_flag,
            sbom_path,
            provenance_flag,
            provenance_path,
            license_bundle_flag,
            license_bundle_path,
        ] if apply == "apply"
            && metadata_flag == "--metadata"
            && metadata_signature_flag == "--metadata-signature"
            && artifact_signature_flag == "--artifact-signature"
            && artifact_flag == "--artifact"
            && sbom_flag == "--sbom"
            && provenance_flag == "--provenance"
            && license_bundle_flag == "--license-bundle" =>
        {
            let install_root = installed_root_from_current_executable()?;
            let inputs = UpdateInputPaths::new(
                PathBuf::from(metadata_path),
                PathBuf::from(metadata_signature_path),
                PathBuf::from(artifact_signature_path),
                PathBuf::from(artifact_path),
                PathBuf::from(sbom_path),
                PathBuf::from(provenance_path),
                PathBuf::from(license_bundle_path),
            );
            let mut health = ProcessCandidateHealthCheck;
            let paths = runtime_paths()?;
            Ok(CommandResult::UpdateApplied(apply_update_package(
                &install_root,
                paths.state_dir(),
                &inputs,
                &mut health,
            )?))
        }
        _ => execute_update_verify(arguments),
    }
}

fn read_package_checksum(path: &Path, artifact: &Path) -> Result<String, CliError> {
    let bytes = read_bounded_update_input(path, MAX_UPDATE_CHECKSUM_FILE_BYTES)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| CliError::InvalidUpdateInput)?;
    let line = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text);
    if line.contains(['\r', '\n']) {
        return Err(CliError::InvalidUpdateInput);
    }
    let (digest, file_name) = line.split_once("  ").ok_or(CliError::InvalidUpdateInput)?;
    let expected_name = artifact
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(CliError::InvalidUpdateInput)?;
    if file_name != expected_name {
        return Err(CliError::InvalidUpdateInput);
    }
    Ok(digest.to_owned())
}

fn execute_update_verify(arguments: &[std::ffi::OsString]) -> Result<CommandResult, CliError> {
    let [
        verify,
        metadata_flag,
        metadata_path,
        metadata_signature_flag,
        metadata_signature_path,
        artifact_signature_flag,
        artifact_signature_path,
        artifact_flag,
        artifact_path,
        sbom_flag,
        sbom_path,
        provenance_flag,
        provenance_path,
        license_bundle_flag,
        license_bundle_path,
        public_key_flag,
        public_key_path,
        context_flag,
        context_path,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if verify != "verify"
        || metadata_flag != "--metadata"
        || metadata_signature_flag != "--metadata-signature"
        || artifact_signature_flag != "--artifact-signature"
        || artifact_flag != "--artifact"
        || sbom_flag != "--sbom"
        || provenance_flag != "--provenance"
        || license_bundle_flag != "--license-bundle"
        || public_key_flag != "--public-key"
        || context_flag != "--context"
    {
        return Err(CliError::Usage);
    }

    let metadata = read_bounded_update_input(
        Path::new(metadata_path),
        u64::try_from(MAX_UPDATE_METADATA_BYTES).map_err(|_| CliError::UpdateInputTooLarge)?,
    )?;
    let metadata_signature = read_update_hex(
        Path::new(metadata_signature_path),
        MAX_UPDATE_KEY_FILE_BYTES,
    )?;
    let artifact_signature = read_update_hex(
        Path::new(artifact_signature_path),
        MAX_UPDATE_KEY_FILE_BYTES,
    )?;
    let public_key = read_update_hex(Path::new(public_key_path), MAX_UPDATE_KEY_FILE_BYTES)?;
    let context_bytes =
        read_bounded_update_input(Path::new(context_path), MAX_UPDATE_CONTEXT_BYTES)?;
    let context: UpdateContext =
        serde_json::from_slice(&context_bytes).map_err(|_| CliError::InvalidUpdateInput)?;
    let artifact_path = Path::new(artifact_path);
    let artifact_metadata =
        fs::symlink_metadata(artifact_path).map_err(CliError::UpdateInputRead)?;
    if !artifact_metadata.file_type().is_file() {
        return Err(CliError::InvalidUpdateInput);
    }
    if artifact_metadata.len() > MAX_UPDATE_ARTIFACT_BYTES {
        return Err(CliError::UpdateInputTooLarge);
    }
    let mut artifact = fs::File::open(artifact_path).map_err(CliError::UpdateInputRead)?;
    let mut sbom = open_bounded_update_input(Path::new(sbom_path), MAX_UPDATE_SBOM_BYTES)?;
    let mut provenance =
        open_bounded_update_input(Path::new(provenance_path), MAX_UPDATE_PROVENANCE_BYTES)?;
    let mut license_bundle = open_bounded_update_input(
        Path::new(license_bundle_path),
        MAX_UPDATE_LICENSE_BUNDLE_BYTES,
    )?;
    let mut supporting =
        UpdateSupportingEvidence::new(&mut sbom, &mut provenance, &mut license_bundle);
    let verified = verify_update_with_evidence(
        &metadata,
        UpdateSignatures::new(
            DetachedUpdateSignature::from_hex(&metadata_signature)?,
            DetachedArtifactSignature::from_hex(&artifact_signature)?,
            UpdatePublicKey::from_hex(&public_key)?,
        ),
        &mut artifact,
        &mut supporting,
        &context,
    )?;
    Ok(CommandResult::UpdateVerified(verified))
}

fn installed_root_from_current_executable() -> Result<PathBuf, CliError> {
    let executable = env::current_exe().map_err(CliError::CurrentExecutable)?;
    installed_root_for_executable(&executable)
}

fn installed_root_for_executable(executable: &Path) -> Result<PathBuf, CliError> {
    let bin = executable
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
        .ok_or(CliError::InvalidInstalledLayout)?;
    let version_root = bin.parent().ok_or(CliError::InvalidInstalledLayout)?;
    let version = version_root
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or(CliError::InvalidInstalledLayout)?;
    let parsed = semver::Version::parse(version).map_err(|_| CliError::InvalidInstalledLayout)?;
    if parsed.to_string() != version {
        return Err(CliError::InvalidInstalledLayout);
    }
    let versions = version_root
        .parent()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name == INSTALL_VERSIONS_DIRECTORY)
        })
        .ok_or(CliError::InvalidInstalledLayout)?;
    versions
        .parent()
        .map(Path::to_path_buf)
        .ok_or(CliError::InvalidInstalledLayout)
}

fn execute_update_health_probe() -> Result<CommandResult, CliError> {
    let temporary = update_health_state_tempdir()?;
    let runtime = update_health_runtime_tempdir()?;
    let state_dir = env::var_os(UPDATE_HEALTH_STATE_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| temporary.path().join("state"));
    if !state_dir.is_absolute() {
        return Err(CliError::InvalidUpdateInput);
    }
    let paths = RuntimePaths::new(state_dir, runtime.path().to_path_buf())?;
    paths.prepare_owner()?;
    let mut identity = [0_u8; 16];
    getrandom::fill(&mut identity).map_err(|_| CliError::RandomUnavailable)?;
    let (client, owned) =
        Client::connect_or_start_owned(&paths, identity, ConnectPolicy::StartIfMissing)?;
    let health = client.health()?;
    if !health.ready {
        return Err(CliError::HealthProbeNotReady);
    }
    let owned = owned.ok_or(CliError::HealthProbeOwnership)?;
    drop(client);
    owned.shutdown()?;
    Ok(CommandResult::UpdateHealthProbe { ready: true })
}

fn update_health_state_tempdir() -> Result<tempfile::TempDir, CliError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("rootlight-update-health-");
    #[cfg(target_os = "macos")]
    {
        // macOS exposes its default temporary directory through the `/var`
        // alias, which the no-follow state boundary intentionally rejects.
        builder
            .tempdir_in(Path::new("/private/tmp"))
            .map_err(CliError::HealthProbeIo)
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder.tempdir().map_err(CliError::HealthProbeIo)
    }
}

fn update_health_runtime_tempdir() -> Result<tempfile::TempDir, CliError> {
    #[cfg(unix)]
    {
        // Unix-domain socket paths have a small platform limit. A package can
        // run from an arbitrarily deep install or CI directory, so keep this
        // isolated, owner-private runtime namespace under a bounded root.
        #[cfg(target_os = "macos")]
        let root = Path::new("/private/tmp");
        #[cfg(not(target_os = "macos"))]
        let root = Path::new("/tmp");
        tempfile::Builder::new()
            .prefix("rlh-")
            .tempdir_in(root)
            .map_err(CliError::HealthProbeIo)
    }
    #[cfg(windows)]
    {
        tempfile::Builder::new()
            .prefix("rootlight-update-health-runtime-")
            .tempdir()
            .map_err(CliError::HealthProbeIo)
    }
}

fn read_update_hex(path: &Path, maximum: u64) -> Result<String, CliError> {
    let bytes = read_bounded_update_input(path, maximum)?;
    let value = std::str::from_utf8(&bytes).map_err(|_| CliError::InvalidUpdateInput)?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(CliError::InvalidUpdateInput);
    }
    Ok(value.to_owned())
}

fn read_bounded_update_input(path: &Path, maximum: u64) -> Result<Vec<u8>, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(CliError::UpdateInputRead)?;
    if !metadata.file_type().is_file() {
        return Err(CliError::InvalidUpdateInput);
    }
    if metadata.len() > maximum {
        return Err(CliError::UpdateInputTooLarge);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(CliError::UpdateInputRead)?
        .take(
            maximum
                .checked_add(1)
                .ok_or(CliError::UpdateInputTooLarge)?,
        )
        .read_to_end(&mut bytes)
        .map_err(CliError::UpdateInputRead)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(CliError::UpdateInputTooLarge);
    }
    Ok(bytes)
}

fn open_bounded_update_input(path: &Path, maximum: u64) -> Result<fs::File, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(CliError::UpdateInputRead)?;
    if !metadata.file_type().is_file() {
        return Err(CliError::InvalidUpdateInput);
    }
    if metadata.len() > maximum {
        return Err(CliError::UpdateInputTooLarge);
    }
    fs::File::open(path).map_err(CliError::UpdateInputRead)
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
        0,
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
        0,
        &cancellation,
    )?;
    let pinned_first = service.code_locate(
        first.generation,
        "answer".to_owned(),
        LocateMode::Exact,
        8,
        0,
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

fn execute_runtime_trace_import(
    paths: &RuntimePaths,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    let [
        input_flag,
        input,
        repository_flag,
        repository,
        generation_flag,
        generation,
    ] = arguments
    else {
        return Err(CliError::Usage);
    };
    if input_flag != "--input"
        || repository_flag != "--repository"
        || generation_flag != "--generation"
    {
        return Err(CliError::Usage);
    }

    let repository = parse_repository(repository)?;
    let generation = parse_generation(generation)?;
    let limits = RuntimeTraceLimits::default();
    let trace = read_runtime_trace_input(Path::new(input), limits.max_input_bytes())?;
    let cancellation = generation_transfer_cancellation()?;
    let service = FirstSliceService::new_durable(
        SHARED_GENERATION_RETENTION,
        paths.state_dir(),
        &cancellation,
    )?;
    let overlay = service.import_runtime_trace_overlay(
        repository,
        generation,
        &trace,
        limits,
        &cancellation,
    )?;
    Ok(CommandResult::RuntimeTraceImport(RuntimeTraceReceipt::new(
        &overlay,
    )?))
}

fn execute_generation_export(
    paths: &RuntimePaths,
    arguments: &[std::ffi::OsString],
) -> Result<CommandResult, CliError> {
    let (repository, generation, output) = match arguments {
        [repository_flag, repository, output_flag, output]
            if repository_flag == "--repository" && output_flag == "--output" =>
        {
            (
                parse_repository(repository)?,
                None,
                generation_output_path(output)?,
            )
        }
        [
            repository_flag,
            repository,
            generation_flag,
            generation,
            output_flag,
            output,
        ] if repository_flag == "--repository"
            && generation_flag == "--generation"
            && output_flag == "--output" =>
        {
            (
                parse_repository(repository)?,
                Some(parse_generation(generation)?),
                generation_output_path(output)?,
            )
        }
        _ => return Err(CliError::Usage),
    };
    let cancellation = generation_transfer_cancellation()?;
    let service = FirstSliceService::new_durable(
        SHARED_GENERATION_RETENTION,
        paths.state_dir(),
        &cancellation,
    )?;
    let exported = service.export_shared_generation(
        repository,
        generation,
        SharedGenerationLimits::default(),
        &cancellation,
    )?;
    write_generation_bundle(output, exported.bundle())?;
    Ok(CommandResult::GenerationExport(
        SharedGenerationReceipt::new(
            exported.repository(),
            exported.generation(),
            exported.source_set_hash(),
            exported.bundle(),
        )?,
    ))
}

fn execute_generation_import(arguments: &[std::ffi::OsString]) -> Result<CommandResult, CliError> {
    let (input, repository, source_set_hash, generation) = match arguments {
        [
            input_flag,
            input,
            repository_flag,
            repository,
            source_flag,
            source_set_hash,
        ] if input_flag == "--input"
            && repository_flag == "--repository"
            && source_flag == "--source-set-hash" =>
        {
            (
                Path::new(input),
                parse_repository(repository)?,
                parse_content_hash(source_set_hash)?,
                None,
            )
        }
        [
            input_flag,
            input,
            repository_flag,
            repository,
            source_flag,
            source_set_hash,
            generation_flag,
            generation,
        ] if input_flag == "--input"
            && repository_flag == "--repository"
            && source_flag == "--source-set-hash"
            && generation_flag == "--generation" =>
        {
            (
                Path::new(input),
                parse_repository(repository)?,
                parse_content_hash(source_set_hash)?,
                Some(parse_generation(generation)?),
            )
        }
        _ => return Err(CliError::Usage),
    };
    let limits = SharedGenerationLimits::default();
    let encoded = read_generation_bundle(input, limits.max_bundle_bytes())?;
    let cancellation = generation_transfer_cancellation()?;
    let service = FirstSliceService::new(1)?;
    let mut expectation = SharedGenerationExpectation::new(repository, source_set_hash);
    if let Some(generation) = generation {
        expectation = expectation.with_generation(generation);
    }
    let imported =
        service.import_shared_generation(&encoded, expectation, limits, &cancellation)?;
    let metadata = imported.generation().metadata();
    Ok(CommandResult::GenerationImport(
        SharedGenerationReceipt::new(
            metadata.repository(),
            metadata.generation(),
            imported.source_set_hash(),
            &encoded,
        )?,
    ))
}

fn generation_transfer_cancellation() -> Result<Cancellation, CliError> {
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(30))
        .ok_or(CliError::Clock)?;
    Ok(Cancellation::with_deadline(deadline))
}

fn parse_repository(argument: &std::ffi::OsStr) -> Result<RepositoryId, CliError> {
    argument
        .to_str()
        .ok_or(CliError::InvalidGenerationInput)?
        .parse()
        .map_err(|_| CliError::InvalidGenerationInput)
}

fn parse_generation(argument: &std::ffi::OsStr) -> Result<GenerationId, CliError> {
    argument
        .to_str()
        .ok_or(CliError::InvalidGenerationInput)?
        .parse()
        .map_err(|_| CliError::InvalidGenerationInput)
}

fn parse_content_hash(argument: &std::ffi::OsStr) -> Result<ContentHash, CliError> {
    argument
        .to_str()
        .ok_or(CliError::InvalidGenerationInput)?
        .parse()
        .map_err(|_| CliError::InvalidGenerationInput)
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
    if command == "generation-export" {
        return execute_generation_export(paths, arguments);
    }
    if command == "runtime-trace-import" {
        return execute_runtime_trace_import(paths, arguments);
    }
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
                .control(ControlRequest::OperationCancel {
                    operation: parse_operation(operation)?,
                    authority: CancellationAuthority::Client(ClientInstanceId::SYSTEM),
                })
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
        error @ RuntimeError::PrivateOutputCleanup(_) => CliError::SupportCleanup(error),
        error => CliError::Runtime(error),
    }
}

fn generation_output_path(argument: &std::ffi::OsStr) -> Result<&Path, CliError> {
    let path = Path::new(argument);
    validate_support_output_path(path).map_err(|_| CliError::InvalidGenerationPath)?;
    Ok(path)
}

fn write_generation_bundle(path: &Path, bundle: &[u8]) -> Result<(), CliError> {
    PrivateOutputFile::preflight().map_err(map_generation_output_error)?;
    validate_support_output_path(path).map_err(|_| CliError::InvalidGenerationPath)?;
    let mut output = PrivateOutputFile::create(path).map_err(map_generation_output_error)?;
    if let Err(source) = output.write_all(bundle) {
        return match output.abort() {
            Ok(()) => Err(CliError::GenerationWrite(source)),
            Err(cleanup) => Err(CliError::GenerationCleanup(cleanup)),
        };
    }
    output.commit().map_err(map_generation_output_error)
}

fn map_generation_output_error(error: RuntimeError) -> CliError {
    match error {
        RuntimeError::Io(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
            CliError::GenerationOutputExists
        }
        RuntimeError::Io(source) => CliError::GenerationWrite(source),
        error @ RuntimeError::PrivateOutputCleanup(_) => CliError::GenerationCleanup(error),
        error => CliError::Runtime(error),
    }
}

fn read_generation_bundle(path: &Path, maximum: usize) -> Result<Vec<u8>, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(CliError::GenerationRead)?;
    if !metadata.file_type().is_file() {
        return Err(CliError::InvalidGenerationInput);
    }
    let maximum_u64 = u64::try_from(maximum).map_err(|_| CliError::GenerationInputTooLarge)?;
    if metadata.len() > maximum_u64 {
        return Err(CliError::GenerationInputTooLarge);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(CliError::GenerationRead)?
        .take(
            maximum_u64
                .checked_add(1)
                .ok_or(CliError::GenerationInputTooLarge)?,
        )
        .read_to_end(&mut bytes)
        .map_err(CliError::GenerationRead)?;
    if bytes.len() > maximum {
        return Err(CliError::GenerationInputTooLarge);
    }
    Ok(bytes)
}

fn read_runtime_trace_input(path: &Path, maximum: usize) -> Result<Vec<u8>, CliError> {
    let metadata = fs::symlink_metadata(path).map_err(CliError::RuntimeTraceRead)?;
    if !metadata.file_type().is_file() {
        return Err(CliError::InvalidRuntimeTraceInput);
    }
    let maximum_u64 = u64::try_from(maximum).map_err(|_| CliError::RuntimeTraceInputTooLarge)?;
    if metadata.len() > maximum_u64 {
        return Err(CliError::RuntimeTraceInputTooLarge);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(CliError::RuntimeTraceRead)?
        .take(
            maximum_u64
                .checked_add(1)
                .ok_or(CliError::RuntimeTraceInputTooLarge)?,
        )
        .read_to_end(&mut bytes)
        .map_err(CliError::RuntimeTraceRead)?;
    if bytes.len() > maximum {
        return Err(CliError::RuntimeTraceInputTooLarge);
    }
    Ok(bytes)
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
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
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
    RepairPlan(RepairPlan),
    UpdateVerified(VerifiedUpdate),
    UpdateInstalled(PackageInstallOutcome),
    UpdateUninstalled(PackageUninstallOutcome),
    UpdateApplied(FilesystemUpdateOutcome),
    UpdateRecovered(UpdateRuntimeStatus),
    UpdateStatus(UpdateRuntimeStatus),
    UpdateHealthProbe {
        ready: bool,
    },
    OperationSubmit(OperationStatus),
    OperationStatus(OperationStatus),
    OperationCancel {
        accepted: bool,
        operation: OperationStatus,
    },
    GenerationExport(SharedGenerationReceipt),
    GenerationImport(SharedGenerationReceipt),
    RuntimeTraceImport(RuntimeTraceReceipt),
    FirstSliceDemo(Box<FirstSliceDemoResult>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct SharedGenerationReceipt {
    schema_version: &'static str,
    repository: RepositoryId,
    generation: GenerationId,
    source_set_hash: ContentHash,
    bundle_hash: ContentHash,
    bundle_bytes: u64,
    read_only: bool,
    activated: bool,
}

impl SharedGenerationReceipt {
    fn new(
        repository: RepositoryId,
        generation: GenerationId,
        source_set_hash: ContentHash,
        bundle: &[u8],
    ) -> Result<Self, CliError> {
        Ok(Self {
            schema_version: SHARED_GENERATION_RECEIPT_SCHEMA,
            repository,
            generation,
            source_set_hash,
            bundle_hash: content_hash(bundle),
            bundle_bytes: u64::try_from(bundle.len())
                .map_err(|_| CliError::GenerationInputTooLarge)?,
            read_only: true,
            activated: false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct RuntimeTraceReceipt {
    schema_version: &'static str,
    trace_schema_version: &'static str,
    repository: RepositoryId,
    generation: GenerationId,
    trace_hash: ContentHash,
    relation_records: u64,
    total_observations: u64,
    producer_kind: &'static str,
    importer_version: &'static str,
    read_only: bool,
    persisted: bool,
    static_generation_mutated: bool,
}

impl RuntimeTraceReceipt {
    fn new(overlay: &RuntimeTraceOverlay) -> Result<Self, CliError> {
        Ok(Self {
            schema_version: RUNTIME_TRACE_RECEIPT_SCHEMA,
            trace_schema_version: RUNTIME_TRACE_SCHEMA_VERSION,
            repository: overlay.repository(),
            generation: overlay.generation(),
            trace_hash: overlay.provenance().trace_hash(),
            relation_records: u64::try_from(overlay.relations().len())
                .map_err(|_| CliError::RuntimeTraceInputTooLarge)?,
            total_observations: overlay.total_observations(),
            producer_kind: "runtime_trace",
            importer_version: overlay.provenance().importer_version(),
            read_only: true,
            persisted: false,
            static_generation_mutated: false,
        })
    }
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
        "usage: rootlight [--standalone] first-slice-demo|health [--json]|diagnostics quick|support-bundle --output <file>|--standalone generation-export --repository <id> [--generation <id>] --output <file>|generation-import --input <file> --repository <id> --source-set-hash <hash> [--generation <id>]|--standalone runtime-trace-import --input <file> --repository <id> --generation <id>|repair --dry-run <verify-catalog|verify-generation-headers|full-scrub|select-last-good-generation|rebuild-lexical-index|rebuild-derived-overlays|rebuild-repository|reconstruct-catalog|purge-quarantine> [--inventory <file>]|update install --root <dir> --artifact <file> --checksum <file> --public-key <file> --key-id <id> --channel <channel>|update uninstall --root <dir>|update apply --metadata <file> --metadata-signature <file> --artifact-signature <file> --artifact <file> --sbom <file> --provenance <file> --license-bundle <file>|update recover|update status|update verify --metadata <file> --metadata-signature <file> --artifact-signature <file> --artifact <file> --sbom <file> --provenance <file> --license-bundle <file> --public-key <file> --context <file>|operation-submit <id> [--timeout-ms <ms>|--deadline-unix-ms <ms> [--lease-expires-unix-ms <ms>]|--lease-expires-unix-ms <ms>]|operation-status <id>|operation-cancel <id>"
    )]
    Usage,
    #[error("daemon path overrides must provide both state and runtime directories")]
    IncompletePathOverride,
    #[error("support bundle output path is invalid")]
    InvalidSupportPath,
    #[error("support bundle output already exists")]
    SupportOutputExists,
    #[error("support bundle output failed")]
    SupportWrite(#[source] std::io::Error),
    #[error("support bundle digest encoding failed")]
    DigestEncoding,
    #[error("support bundle staging cleanup failed")]
    SupportCleanup(#[source] RuntimeError),
    #[error("runtime trace input is invalid")]
    InvalidRuntimeTraceInput,
    #[error("runtime trace input exceeds its byte limit")]
    RuntimeTraceInputTooLarge,
    #[error("runtime trace input could not be read")]
    RuntimeTraceRead(#[source] std::io::Error),
    #[error("shared generation input is invalid")]
    InvalidGenerationInput,
    #[error("shared generation input or output path is invalid")]
    InvalidGenerationPath,
    #[error("shared generation input exceeds its byte limit")]
    GenerationInputTooLarge,
    #[error("shared generation input could not be read")]
    GenerationRead(#[source] std::io::Error),
    #[error("shared generation output already exists")]
    GenerationOutputExists,
    #[error("shared generation output failed")]
    GenerationWrite(#[source] std::io::Error),
    #[error("shared generation staging cleanup failed")]
    GenerationCleanup(#[source] RuntimeError),
    #[error("repair inventory is invalid")]
    InvalidRepairInventory,
    #[error("repair inventory exceeds its byte limit")]
    RepairInventoryTooLarge,
    #[error("repair inventory could not be read")]
    RepairInventoryRead(#[source] std::io::Error),
    #[error("repair planning failed")]
    Repair(#[from] rootlight_operations::RepairError),
    #[error("update input is invalid")]
    InvalidUpdateInput,
    #[error("update input exceeds its byte limit")]
    UpdateInputTooLarge,
    #[error("update input could not be read")]
    UpdateInputRead(#[source] std::io::Error),
    #[error("update verification failed")]
    Update(#[from] rootlight_client::UpdateError),
    #[error("update installation operation failed")]
    FilesystemUpdate(#[from] FilesystemUpdateError),
    #[error("current executable is unavailable")]
    CurrentExecutable(#[source] std::io::Error),
    #[error("current executable is outside an installed version")]
    InvalidInstalledLayout,
    #[error("candidate health probe filesystem setup failed")]
    HealthProbeIo(#[source] std::io::Error),
    #[error("candidate health probe did not reach ready state")]
    HealthProbeNotReady,
    #[error("candidate health probe did not own its isolated daemon")]
    HealthProbeOwnership,
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
            | Self::InvalidRuntimeTraceInput
            | Self::RuntimeTraceInputTooLarge
            | Self::RuntimeTraceRead(_)
            | Self::InvalidGenerationInput
            | Self::InvalidGenerationPath
            | Self::GenerationInputTooLarge
            | Self::GenerationRead(_)
            | Self::GenerationOutputExists
            | Self::InvalidRepairInventory
            | Self::RepairInventoryTooLarge
            | Self::InvalidUpdateInput
            | Self::UpdateInputTooLarge
            | Self::FilesystemUpdate(
                FilesystemUpdateError::InvalidInput
                | FilesystemUpdateError::InputTooLarge
                | FilesystemUpdateError::InvalidSignature,
            )
            | Self::InvalidOperation
            | Self::InvalidTimeout
            | Self::FirstSlice(FirstSliceError::Sharing | FirstSliceError::RuntimeTrace(_)) => {
                ExitFamily::Usage
            }
            Self::FilesystemUpdate(
                FilesystemUpdateError::Health(_) | FilesystemUpdateError::UnsupportedPlatform,
            )
            | Self::HealthProbeNotReady => ExitFamily::Degraded,
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
            Self::FilesystemUpdate(FilesystemUpdateError::Busy) => ExitFamily::Unavailable,
            Self::Update(_)
            | Self::FilesystemUpdate(_)
            | Self::InvalidInstalledLayout
            | Self::HealthProbeOwnership => ExitFamily::SecurityPolicy,
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
        #[cfg(unix)]
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

    #[test]
    fn generation_commands_export_and_verify_a_read_only_bundle() {
        let temporary = support_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::create_dir(repository_root.join("src")).expect("source directory creates");
        fs::write(
            repository_root.join("src/lib.rs"),
            FIRST_SLICE_SOURCE_BEFORE,
        )
        .expect("source writes");
        let cancellation = generation_transfer_cancellation().expect("deadline is representable");
        let indexed = {
            let mut service = FirstSliceService::new_durable(
                SHARED_GENERATION_RETENTION,
                paths.state_dir(),
                &cancellation,
            )
            .expect("durable service initializes");
            service
                .index_rust_fixture(&repository_root, &cancellation)
                .expect("fixture indexes")
        };

        let bundle = temporary.path().join("generation.rlshare");
        let export_arguments = [
            std::ffi::OsString::from("--repository"),
            std::ffi::OsString::from(indexed.repository.to_string()),
            std::ffi::OsString::from("--generation"),
            std::ffi::OsString::from(indexed.generation.to_string()),
            std::ffi::OsString::from("--output"),
            bundle.as_os_str().to_owned(),
        ];
        let CommandResult::GenerationExport(exported) =
            execute_generation_export(&paths, &export_arguments).expect("generation exports")
        else {
            panic!("generation export returned the wrong result");
        };
        assert!(bundle.is_file());
        assert!(exported.read_only);
        assert!(!exported.activated);

        let import_arguments = [
            std::ffi::OsString::from("--input"),
            bundle.as_os_str().to_owned(),
            std::ffi::OsString::from("--repository"),
            std::ffi::OsString::from(exported.repository.to_string()),
            std::ffi::OsString::from("--source-set-hash"),
            std::ffi::OsString::from(exported.source_set_hash.to_string()),
            std::ffi::OsString::from("--generation"),
            std::ffi::OsString::from(exported.generation.to_string()),
        ];
        let CommandResult::GenerationImport(imported) =
            execute_generation_import(&import_arguments).expect("generation imports")
        else {
            panic!("generation import returned the wrong result");
        };
        assert_eq!(imported, exported);
    }

    #[test]
    fn runtime_trace_command_imports_a_read_only_generation_overlay() {
        let temporary = support_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        paths.prepare_owner().expect("runtime paths are private");
        let repository_root = temporary.path().join("repository");
        fs::create_dir(&repository_root).expect("repository root creates");
        fs::create_dir(repository_root.join("src")).expect("source directory creates");
        fs::write(
            repository_root.join("src/lib.rs"),
            FIRST_SLICE_SOURCE_BEFORE,
        )
        .expect("source writes");
        let cancellation = generation_transfer_cancellation().expect("deadline is representable");
        let (indexed, symbol) = {
            let mut service = FirstSliceService::new_durable(
                SHARED_GENERATION_RETENTION,
                paths.state_dir(),
                &cancellation,
            )
            .expect("durable service initializes");
            let indexed = service
                .index_rust_fixture(&repository_root, &cancellation)
                .expect("fixture indexes");
            let located = service
                .code_locate(
                    indexed.generation,
                    "answer".to_owned(),
                    LocateMode::Exact,
                    1,
                    0,
                    &cancellation,
                )
                .expect("fixture symbol locates");
            (indexed, located.data.hits[0].symbol)
        };
        let trace = temporary.path().join("runtime-trace.json");
        fs::write(
            &trace,
            serde_json::to_vec(&serde_json::json!({
                "schema": RUNTIME_TRACE_SCHEMA_VERSION,
                "repository": indexed.repository,
                "generation": indexed.generation,
                "producer": {
                    "name": "cli-test-tracer",
                    "version": "1.0.0",
                    "configuration_hash": content_hash(b"cli-test-tracer-config"),
                    "binary_digest": content_hash(b"cli-test-tracer-binary"),
                },
                "records": [{
                    "kind": "calls",
                    "subject": symbol,
                    "object": symbol,
                    "count": 4,
                }],
            }))
            .expect("trace JSON encodes"),
        )
        .expect("trace input writes");
        let arguments = [
            std::ffi::OsString::from("--input"),
            trace.as_os_str().to_owned(),
            std::ffi::OsString::from("--repository"),
            std::ffi::OsString::from(indexed.repository.to_string()),
            std::ffi::OsString::from("--generation"),
            std::ffi::OsString::from(indexed.generation.to_string()),
        ];

        let CommandResult::RuntimeTraceImport(receipt) =
            execute_runtime_trace_import(&paths, &arguments).expect("runtime trace imports")
        else {
            panic!("runtime trace import returned the wrong result");
        };

        assert_eq!(receipt.schema_version, RUNTIME_TRACE_RECEIPT_SCHEMA);
        assert_eq!(receipt.trace_schema_version, RUNTIME_TRACE_SCHEMA_VERSION);
        assert_eq!(receipt.repository, indexed.repository);
        assert_eq!(receipt.generation, indexed.generation);
        assert_eq!(receipt.relation_records, 1);
        assert_eq!(receipt.total_observations, 4);
        assert!(receipt.read_only);
        assert!(!receipt.persisted);
        assert!(!receipt.static_generation_mutated);
        let restored = FirstSliceService::new_durable(
            SHARED_GENERATION_RETENTION,
            paths.state_dir(),
            &cancellation,
        )
        .expect("durable state restores");
        assert_eq!(
            restored.active_generation_for(indexed.repository),
            Some(indexed.generation)
        );
    }

    #[test]
    fn update_health_runtime_supports_a_nonce_specific_endpoint() {
        let state =
            update_health_state_tempdir().expect("bounded health state directory is available");
        let runtime =
            update_health_runtime_tempdir().expect("bounded health runtime directory is available");
        let paths = RuntimePaths::new(state.path().join("state"), runtime.path().to_path_buf())
            .expect("health runtime paths are valid");
        paths
            .prepare_owner()
            .expect("health runtime paths become owner-private");

        paths
            .endpoint([7; 16])
            .expect("health runtime endpoint is representable");
    }

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

    #[test]
    fn repair_reconstruction_is_a_source_free_non_mutating_plan() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let state = temporary.path().join("state");
        let runtime = temporary.path().join("runtime");
        let paths = RuntimePaths::new(state.clone(), runtime).expect("runtime paths are valid");
        let inventory_path = temporary.path().join("inventory.json");
        let inventory = serde_json::json!({
            "schema_version": "1.0",
            "candidates": [{
                "generation_id": "generation-01",
                "manifest_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "complete": true,
                "verified": true,
                "required_bytes": 4096
            }]
        });
        fs::write(
            &inventory_path,
            serde_json::to_vec(&inventory).expect("inventory serializes"),
        )
        .expect("inventory writes");
        let arguments = [
            std::ffi::OsString::from("--dry-run"),
            std::ffi::OsString::from("reconstruct-catalog"),
            std::ffi::OsString::from("--inventory"),
            inventory_path.into_os_string(),
        ];

        let result = execute_repair(&paths, &arguments).expect("repair plan builds");
        let encoded = serde_json::to_string(&result).expect("result serializes");

        let CommandResult::RepairPlan(plan) = result else {
            panic!("repair returns its typed plan");
        };
        assert_eq!(plan.status, rootlight_operations::RepairPlanStatus::Ready);
        assert!(!state.exists());
        assert!(!encoded.contains(temporary.path().to_string_lossy().as_ref()));
    }

    #[test]
    fn repair_reconstruction_requires_an_explicit_inventory() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("runtime paths are valid");
        let arguments = [
            std::ffi::OsString::from("--dry-run"),
            std::ffi::OsString::from("reconstruct-catalog"),
        ];

        assert!(matches!(
            execute_repair(&paths, &arguments),
            Err(CliError::InvalidRepairInventory)
        ));
    }

    #[test]
    fn update_verification_rejects_unsigned_inputs_without_runtime_dispatch() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let metadata = temporary.path().join("update.json");
        let metadata_signature = temporary.path().join("update.sig");
        let artifact_signature = temporary.path().join("artifact.sig");
        let artifact = temporary.path().join("rootlight.zip");
        let sbom = temporary.path().join("rootlight.cdx.json");
        let provenance = temporary.path().join("rootlight.intoto.jsonl");
        let license_bundle = temporary.path().join("rootlight.licenses.zip");
        let public_key = temporary.path().join("release.pub");
        let context = temporary.path().join("context.json");
        fs::write(&metadata, b"{}").expect("metadata writes");
        fs::write(&metadata_signature, "0".repeat(128)).expect("metadata signature writes");
        fs::write(&artifact_signature, "0".repeat(128)).expect("artifact signature writes");
        fs::write(&artifact, b"untrusted artifact").expect("artifact writes");
        fs::write(&sbom, b"{}").expect("SBOM writes");
        fs::write(&provenance, b"{}").expect("provenance writes");
        fs::write(&license_bundle, b"not a zip").expect("license bundle writes");
        fs::write(&public_key, "0".repeat(64)).expect("public key writes");
        fs::write(
            &context,
            serde_json::to_vec(&serde_json::json!({
                "updates_enabled": true,
                "current_version": "1.0.0",
                "last_good_version": "1.0.0",
                "channel": "stable",
                "platform": "windows",
                "architecture": "x86_64",
                "now_unix_seconds": 2000,
                "catalog_schema": 3,
                "protocol_major": 1,
                "protocol_minor": 7,
                "available_disk_bytes": 67108864,
                "rollout_bucket": 0
            }))
            .expect("context serializes"),
        )
        .expect("context writes");
        let arguments = [
            std::ffi::OsString::from("verify"),
            std::ffi::OsString::from("--metadata"),
            metadata.into_os_string(),
            std::ffi::OsString::from("--metadata-signature"),
            metadata_signature.into_os_string(),
            std::ffi::OsString::from("--artifact-signature"),
            artifact_signature.into_os_string(),
            std::ffi::OsString::from("--artifact"),
            artifact.into_os_string(),
            std::ffi::OsString::from("--sbom"),
            sbom.into_os_string(),
            std::ffi::OsString::from("--provenance"),
            provenance.into_os_string(),
            std::ffi::OsString::from("--license-bundle"),
            license_bundle.into_os_string(),
            std::ffi::OsString::from("--public-key"),
            public_key.into_os_string(),
            std::ffi::OsString::from("--context"),
            context.into_os_string(),
        ];

        assert!(matches!(
            execute_update(&arguments),
            Err(CliError::Update(
                rootlight_client::UpdateError::InvalidSignature
            ))
        ));
    }

    #[test]
    fn installed_update_root_requires_the_versioned_payload_layout() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let executable = root.join("versions/1.2.3/bin").join(if cfg!(windows) {
            "rootlight.exe"
        } else {
            "rootlight"
        });

        assert_eq!(
            installed_root_for_executable(&executable).expect("installed layout resolves"),
            root
        );
    }

    #[test]
    fn installed_update_root_rejects_launcher_and_noncanonical_paths() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let launcher = root.join("current/bin/rootlight");
        let noncanonical = root.join("versions/1.2.3.0/bin/rootlight");

        assert!(matches!(
            installed_root_for_executable(&launcher),
            Err(CliError::InvalidInstalledLayout)
        ));
        assert!(matches!(
            installed_root_for_executable(&noncanonical),
            Err(CliError::InvalidInstalledLayout)
        ));
    }
}
