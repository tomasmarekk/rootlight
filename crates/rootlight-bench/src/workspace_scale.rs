//! Candidate-bound durable workspace scale measurements.
//!
//! Unlike the deterministic language/workspace contract artifact, this module
//! records wall-clock and process telemetry from one exact run. The benchmark
//! exercises the production durable service so scale evidence includes
//! discovery, parsing, publication, restart recovery, and independent
//! repository advancement instead of an in-memory catalog approximation.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_query::LocateMode;
use rootlight_runtime::RuntimePaths;
use rootlight_service::{FirstSliceError, FirstSliceIndexCommit, FirstSliceService};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::{EvidenceValue, ProcessTreeMeasurement, ProcessTreeSample, ProcessTreeSampler};

#[cfg(target_os = "linux")]
use crate::LinuxProcTreeSampler;
#[cfg(not(target_os = "linux"))]
use crate::UnavailableProcessTreeSampler;

/// Wire identity for exact-run durable workspace scale evidence.
pub const WORKSPACE_SCALE_EVIDENCE_SCHEMA: &str = "rootlight.workspace-scale-evidence/2";
/// Maximum accepted encoded evidence size.
pub const WORKSPACE_SCALE_EVIDENCE_MAX_BYTES: usize = 64 * 1024;

const MAX_REPOSITORIES: usize = 100;
const FILES_PER_REPOSITORY: u64 = 6;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const RUN_DEADLINE: Duration = Duration::from_secs(600);
const LINUX_SAMPLE_INTERVAL: Duration = Duration::from_millis(1);
const MAX_TOTAL_ELAPSED_MICROS: u64 = 300_000_000;
const MAX_PEAK_RSS_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_RETAINED_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// One exact-run measurement of the production durable multi-repository path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceScaleEvidence {
    schema: String,
    source_revision: String,
    environment: WorkspaceScaleEnvironment,
    repositories_requested: usize,
    repositories_indexed: usize,
    repositories_restored: usize,
    discovered_inputs: u64,
    indexed_files: u64,
    entities: u64,
    durable_state_bytes: u64,
    retained_memory_bytes: u64,
    initial_newly_written_bytes: u64,
    initial_referenced_bytes: u64,
    independent_update_newly_written_bytes: u64,
    independent_update_referenced_bytes: u64,
    independent_update_reserved_memory_bytes: u64,
    independent_update_owned_memory_bytes: u64,
    service_open_micros: u64,
    index_wall_micros: u64,
    index_receipt_micros: u64,
    first_restore_micros: u64,
    independent_update_micros: u64,
    second_restore_micros: u64,
    total_elapsed_micros: u64,
    process_tree_cpu_ns: EvidenceValue<u64>,
    process_tree_peak_rss_bytes: EvidenceValue<u64>,
    durable_publication: bool,
    exact_generations_restored: bool,
    independent_update_published: bool,
    unrelated_generations_unchanged: bool,
    updated_generation_restored: bool,
    first_restore_query_parity: bool,
    second_restore_query_parity: bool,
    bounds: WorkspaceScaleBounds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceScaleEnvironment {
    operating_system: String,
    architecture: String,
    toolchain: String,
    build_profile: String,
    process_tree_measurement: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceScaleBounds {
    maximum_repositories: usize,
    maximum_total_elapsed_micros: u64,
    maximum_peak_rss_bytes: u64,
    maximum_retained_memory_bytes: u64,
    elapsed_within_bound: bool,
    peak_rss_within_bound: Option<bool>,
    retained_memory_within_bound: bool,
}

/// Runs one bounded production-service scale observation.
///
/// # Errors
///
/// Returns a source-redacted error when arguments, fixture construction,
/// durable indexing, restart verification, telemetry, or evidence validation
/// fails.
pub fn build_workspace_scale_evidence(
    repositories: usize,
    source_revision: &str,
    toolchain: &str,
) -> Result<WorkspaceScaleEvidence, WorkspaceScaleEvidenceError> {
    validate_revision(source_revision)?;
    validate_toolchain(toolchain)?;
    if repositories == 0 || repositories > MAX_REPOSITORIES {
        return Err(WorkspaceScaleEvidenceError::InvalidRepositoryCount);
    }

    let fixture = observation_tempdir()?;
    let runtime_paths =
        RuntimePaths::new(fixture.path().join("state"), fixture.path().join("runtime"))
            .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
    runtime_paths
        .prepare_owner()
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
    let state_root = runtime_paths.state_dir().to_path_buf();
    let repositories_root = fixture.path().join("repositories");
    fs::create_dir(&repositories_root).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
    let repository_roots = create_repository_fixtures(&repositories_root, repositories)?;
    let cancellation = deadline()?;
    let sample = begin_process_sample()?;
    let total_started = Instant::now();

    let open_started = Instant::now();
    let mut service = FirstSliceService::new_durable(2, &state_root, &cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    let service_open_micros = elapsed_micros(open_started)?;
    if !service.uses_durable_publication() {
        return Err(WorkspaceScaleEvidenceError::Durability);
    }

    let index_started = Instant::now();
    let mut generations = Vec::new();
    generations
        .try_reserve_exact(repositories)
        .map_err(|_| WorkspaceScaleEvidenceError::ResourceLimit)?;
    let mut discovered_inputs = 0_u64;
    let mut indexed_files = 0_u64;
    let mut entities = 0_u64;
    let mut index_receipt_micros = 0_u64;
    let mut retained_memory_bytes = 0_u64;
    let mut initial_newly_written_bytes = 0_u64;
    let mut initial_referenced_bytes = 0_u64;
    for root in &repository_roots {
        let commit = index_with_evidence(&mut service, root, &cancellation)?;
        let receipt = commit.receipt();
        discovered_inputs = discovered_inputs
            .checked_add(receipt.discovered_inputs)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        indexed_files = indexed_files
            .checked_add(receipt.indexed_files)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        entities = entities
            .checked_add(receipt.entities)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        index_receipt_micros = index_receipt_micros
            .checked_add(receipt.elapsed_micros)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        retained_memory_bytes = retained_memory_bytes
            .checked_add(commit.evidence().owned_memory_bytes)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        initial_newly_written_bytes = initial_newly_written_bytes
            .checked_add(commit.evidence().newly_written_bytes)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        initial_referenced_bytes = initial_referenced_bytes
            .checked_add(commit.evidence().referenced_bytes)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        generations.push((receipt.repository, receipt.generation));
    }
    let index_wall_micros = elapsed_micros(index_started)?;
    if service.list_repositories().len() != repositories {
        return Err(WorkspaceScaleEvidenceError::RepositorySet);
    }
    let (first_repository, first_generation) = generations
        .first()
        .copied()
        .ok_or(WorkspaceScaleEvidenceError::RepositorySet)?;
    let initial_query = query_signature(
        &service,
        first_generation,
        "repository_000_anchor",
        &cancellation,
    )?;
    drop(service);

    let first_restore_started = Instant::now();
    let mut restored = FirstSliceService::new_durable(2, &state_root, &cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    let first_restore_micros = elapsed_micros(first_restore_started)?;
    let repositories_restored = restored.list_repositories().len();
    let exact_generations_restored = repositories_restored == repositories
        && generations.iter().all(|(repository, generation)| {
            restored.active_generation_for(*repository) == Some(*generation)
        });
    if !exact_generations_restored {
        return Err(WorkspaceScaleEvidenceError::Restore);
    }
    let first_restore_query_parity = query_signature(
        &restored,
        first_generation,
        "repository_000_anchor",
        &cancellation,
    )? == initial_query;
    if !first_restore_query_parity {
        return Err(WorkspaceScaleEvidenceError::QueryParity);
    }

    let first_root = repository_roots
        .first()
        .ok_or(WorkspaceScaleEvidenceError::RepositorySet)?;
    fs::write(
        first_root.join("src/lib.rs"),
        "mod worker;\npub fn repository_000_anchor() -> u32 {\n    worker::value() + 1\n}\n",
    )
    .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
    let update_started = Instant::now();
    let updated = index_with_evidence(&mut restored, first_root, &cancellation)?;
    let independent_update_micros = elapsed_micros(update_started)?;
    let independent_update_published = updated.receipt().repository == first_repository
        && updated.receipt().generation != first_generation;
    let unrelated_generations_unchanged =
        generations.iter().skip(1).all(|(repository, generation)| {
            restored.active_generation_for(*repository) == Some(*generation)
        });
    if !independent_update_published || !unrelated_generations_unchanged {
        return Err(WorkspaceScaleEvidenceError::Isolation);
    }
    if updated.evidence().referenced_bytes == 0
        || updated.evidence().newly_written_bytes == 0
        || updated.evidence().reserved_memory_bytes == 0
        || updated.evidence().owned_memory_bytes == 0
    {
        return Err(WorkspaceScaleEvidenceError::IncrementalEvidence);
    }
    let updated_query = query_signature(
        &restored,
        updated.receipt().generation,
        "repository_000_anchor",
        &cancellation,
    )?;
    drop(restored);

    let second_restore_started = Instant::now();
    let restored = FirstSliceService::new_durable(2, &state_root, &cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    let second_restore_micros = elapsed_micros(second_restore_started)?;
    let updated_generation_restored = restored.active_generation_for(updated.receipt().repository)
        == Some(updated.receipt().generation)
        && restored.list_repositories().len() == repositories;
    if !updated_generation_restored {
        return Err(WorkspaceScaleEvidenceError::Restore);
    }
    let second_restore_query_parity = query_signature(
        &restored,
        updated.receipt().generation,
        "repository_000_anchor",
        &cancellation,
    )? == updated_query;
    if !second_restore_query_parity {
        return Err(WorkspaceScaleEvidenceError::QueryParity);
    }
    drop(restored);

    let durable_state_bytes = directory_size(&state_root)?;
    let total_elapsed_micros = elapsed_micros(total_started)?;
    let measurement = sample.finish();
    let bounds = measurement_bounds(total_elapsed_micros, retained_memory_bytes, &measurement);
    let evidence = WorkspaceScaleEvidence {
        schema: WORKSPACE_SCALE_EVIDENCE_SCHEMA.to_owned(),
        source_revision: source_revision.to_owned(),
        environment: WorkspaceScaleEnvironment {
            operating_system: std::env::consts::OS.to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
            toolchain: toolchain.to_owned(),
            build_profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .to_owned(),
            process_tree_measurement: if cfg!(target_os = "linux") {
                "linux_proc_tree_sampled"
            } else {
                "unavailable_on_this_platform"
            }
            .to_owned(),
        },
        repositories_requested: repositories,
        repositories_indexed: generations.len(),
        repositories_restored,
        discovered_inputs,
        indexed_files,
        entities,
        durable_state_bytes,
        retained_memory_bytes,
        initial_newly_written_bytes,
        initial_referenced_bytes,
        independent_update_newly_written_bytes: updated.evidence().newly_written_bytes,
        independent_update_referenced_bytes: updated.evidence().referenced_bytes,
        independent_update_reserved_memory_bytes: updated.evidence().reserved_memory_bytes,
        independent_update_owned_memory_bytes: updated.evidence().owned_memory_bytes,
        service_open_micros,
        index_wall_micros,
        index_receipt_micros,
        first_restore_micros,
        independent_update_micros,
        second_restore_micros,
        total_elapsed_micros,
        process_tree_cpu_ns: measurement.cpu_ns,
        process_tree_peak_rss_bytes: measurement.peak_rss_bytes,
        durable_publication: true,
        exact_generations_restored,
        independent_update_published,
        unrelated_generations_unchanged,
        updated_generation_restored,
        first_restore_query_parity,
        second_restore_query_parity,
        bounds,
    };
    validate_evidence(&evidence, repositories, source_revision, toolchain)?;
    Ok(evidence)
}

fn index_with_evidence(
    service: &mut FirstSliceService,
    root: &Path,
    cancellation: &Cancellation,
) -> Result<FirstSliceIndexCommit, WorkspaceScaleEvidenceError> {
    let prepared = service
        .prepare_repository(root, cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    service
        .publish_prepared_with_metrics(prepared, cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)
}

fn query_signature(
    service: &FirstSliceService,
    generation: rootlight_ids::GenerationId,
    query: &str,
    cancellation: &Cancellation,
) -> Result<Vec<String>, WorkspaceScaleEvidenceError> {
    let response = service
        .code_locate(
            generation,
            query.to_owned(),
            LocateMode::Exact,
            8,
            0,
            cancellation,
        )
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    if response.data.hits.is_empty() {
        return Err(WorkspaceScaleEvidenceError::QueryParity);
    }
    Ok(response
        .data
        .hits
        .into_iter()
        .map(|hit| {
            format!(
                "{}\u{1f}{}\u{1f}{}\u{1f}{}",
                hit.symbol, hit.identifier, hit.path, hit.kind
            )
        })
        .collect())
}

/// Encodes one validated workspace scale observation.
///
/// # Errors
///
/// Returns an error when validation, serialization, size, or privacy checks
/// fail.
pub fn encode_workspace_scale_evidence(
    evidence: &WorkspaceScaleEvidence,
) -> Result<Vec<u8>, WorkspaceScaleEvidenceError> {
    validate_evidence(
        evidence,
        evidence.repositories_requested,
        &evidence.source_revision,
        &evidence.environment.toolchain,
    )?;
    let encoded =
        serde_json::to_vec(evidence).map_err(|_| WorkspaceScaleEvidenceError::Encoding)?;
    if encoded.len() > WORKSPACE_SCALE_EVIDENCE_MAX_BYTES {
        return Err(WorkspaceScaleEvidenceError::ResourceLimit);
    }
    privacy_scan(&encoded)?;
    Ok(encoded)
}

/// Verifies an encoded observation against its exact candidate inputs.
///
/// # Errors
///
/// Returns an error when decoding, canonical encoding, validation, size, or
/// privacy checks fail.
pub fn verify_workspace_scale_evidence(
    encoded: &[u8],
    repositories: usize,
    source_revision: &str,
    toolchain: &str,
) -> Result<(), WorkspaceScaleEvidenceError> {
    if encoded.len() > WORKSPACE_SCALE_EVIDENCE_MAX_BYTES {
        return Err(WorkspaceScaleEvidenceError::ResourceLimit);
    }
    privacy_scan(encoded)?;
    let canonical_input = encoded.strip_suffix(b"\n").unwrap_or(encoded);
    let evidence: WorkspaceScaleEvidence = serde_json::from_slice(canonical_input)
        .map_err(|_| WorkspaceScaleEvidenceError::Encoding)?;
    validate_evidence(&evidence, repositories, source_revision, toolchain)?;
    if serde_json::to_vec(&evidence).map_err(|_| WorkspaceScaleEvidenceError::Encoding)?
        != canonical_input
    {
        return Err(WorkspaceScaleEvidenceError::Encoding);
    }
    Ok(())
}

fn observation_tempdir() -> Result<TempDir, WorkspaceScaleEvidenceError> {
    #[cfg(target_os = "macos")]
    {
        // macOS exposes its default temporary directory through the `/var`
        // alias, which the no-follow SQLite boundary correctly rejects.
        tempfile::Builder::new()
            .prefix("rl-workspace-scale-")
            .tempdir_in("/private/tmp")
            .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)
    }
    #[cfg(not(target_os = "macos"))]
    {
        TempDir::new().map_err(|_| WorkspaceScaleEvidenceError::Filesystem)
    }
}

fn create_repository_fixtures(
    root: &Path,
    repositories: usize,
) -> Result<Vec<PathBuf>, WorkspaceScaleEvidenceError> {
    let mut roots = Vec::new();
    roots
        .try_reserve_exact(repositories)
        .map_err(|_| WorkspaceScaleEvidenceError::ResourceLimit)?;
    for ordinal in 0..repositories {
        let repository = root.join(format!("repository-{ordinal:03}"));
        let source_directory = repository.join("src");
        let web_directory = repository.join("web");
        let scripts_directory = repository.join("scripts");
        let command_directory = repository.join("cmd");
        fs::create_dir(&repository).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        for directory in [
            &source_directory,
            &web_directory,
            &scripts_directory,
            &command_directory,
        ] {
            fs::create_dir(directory).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        }
        fs::write(
            source_directory.join("lib.rs"),
            format!(
                "mod worker;\npub fn repository_{ordinal:03}_anchor() -> u32 {{\n    worker::value()\n}}\n"
            ),
        )
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::write(
            source_directory.join("worker.rs"),
            format!("pub fn value() -> u32 {{ {ordinal} }}\n"),
        )
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::write(
            source_directory.join("worker_test.rs"),
            format!(
                "#[test]\nfn repository_{ordinal:03}_worker_is_stable() {{\n    assert_eq!({ordinal}, {ordinal});\n}}\n"
            ),
        )
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::write(
            web_directory.join("api.ts"),
            format!(
                "export function repository{ordinal:03}Api(value: number): number {{ return value + {ordinal}; }}\n"
            ),
        )
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::write(
            scripts_directory.join("task.py"),
            format!("def repository_{ordinal:03}_task(value: int) -> int:\n    return value + {ordinal}\n"),
        )
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::write(
            command_directory.join("main.go"),
            format!(
                "package main\nfunc repository{ordinal:03}Command(value int) int {{ return value + {ordinal} }}\n"
            ),
        )
        .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        roots.push(repository);
    }
    Ok(roots)
}

fn deadline() -> Result<Cancellation, WorkspaceScaleEvidenceError> {
    let deadline = Instant::now()
        .checked_add(RUN_DEADLINE)
        .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
    Ok(Cancellation::with_deadline(deadline))
}

#[cfg(target_os = "linux")]
fn begin_process_sample() -> Result<crate::LinuxProcTreeSample, WorkspaceScaleEvidenceError> {
    let sampler = LinuxProcTreeSampler::new(std::process::id(), LINUX_SAMPLE_INTERVAL)
        .map_err(|_| WorkspaceScaleEvidenceError::Telemetry)?;
    Ok(sampler.begin())
}

#[cfg(not(target_os = "linux"))]
fn begin_process_sample() -> Result<crate::UnavailableProcessTreeSample, WorkspaceScaleEvidenceError>
{
    let _ = LINUX_SAMPLE_INTERVAL;
    Ok(UnavailableProcessTreeSampler.begin())
}

fn elapsed_micros(started: Instant) -> Result<u64, WorkspaceScaleEvidenceError> {
    u64::try_from(started.elapsed().as_micros())
        .map_err(|_| WorkspaceScaleEvidenceError::ResourceLimit)
}

fn measurement_bounds(
    total_elapsed_micros: u64,
    retained_memory_bytes: u64,
    measurement: &ProcessTreeMeasurement,
) -> WorkspaceScaleBounds {
    let peak_rss_within_bound = match measurement.peak_rss_bytes {
        EvidenceValue::Observed { value } => Some(value <= MAX_PEAK_RSS_BYTES),
        EvidenceValue::Target { .. } | EvidenceValue::Unavailable { .. } => None,
    };
    WorkspaceScaleBounds {
        maximum_repositories: MAX_REPOSITORIES,
        maximum_total_elapsed_micros: MAX_TOTAL_ELAPSED_MICROS,
        maximum_peak_rss_bytes: MAX_PEAK_RSS_BYTES,
        maximum_retained_memory_bytes: MAX_RETAINED_MEMORY_BYTES,
        elapsed_within_bound: total_elapsed_micros <= MAX_TOTAL_ELAPSED_MICROS,
        peak_rss_within_bound,
        retained_memory_within_bound: retained_memory_bytes <= MAX_RETAINED_MEMORY_BYTES,
    }
}

fn directory_size(root: &Path) -> Result<u64, WorkspaceScaleEvidenceError> {
    let mut pending = vec![root.to_path_buf()];
    let mut visited = 0_usize;
    let mut bytes = 0_u64;
    while let Some(path) = pending.pop() {
        visited = visited
            .checked_add(1)
            .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
        if visited > MAX_DIRECTORY_ENTRIES {
            return Err(WorkspaceScaleEvidenceError::ResourceLimit);
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        if metadata.file_type().is_symlink() {
            return Err(WorkspaceScaleEvidenceError::Filesystem);
        }
        if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
            continue;
        }
        if !metadata.is_dir() {
            return Err(WorkspaceScaleEvidenceError::Filesystem);
        }
        for entry in fs::read_dir(&path).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)? {
            pending.push(
                entry
                    .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?
                    .path(),
            );
        }
    }
    Ok(bytes)
}

fn validate_evidence(
    evidence: &WorkspaceScaleEvidence,
    repositories: usize,
    source_revision: &str,
    toolchain: &str,
) -> Result<(), WorkspaceScaleEvidenceError> {
    validate_revision(source_revision)?;
    validate_toolchain(toolchain)?;
    let repository_count =
        u64::try_from(repositories).map_err(|_| WorkspaceScaleEvidenceError::ResourceLimit)?;
    let expected_files = repository_count
        .checked_mul(FILES_PER_REPOSITORY)
        .ok_or(WorkspaceScaleEvidenceError::ResourceLimit)?;
    let cpu_available = matches!(evidence.process_tree_cpu_ns, EvidenceValue::Observed { .. });
    let rss_available = matches!(
        evidence.process_tree_peak_rss_bytes,
        EvidenceValue::Observed { .. }
    );
    let expected_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    if repositories == 0
        || repositories > MAX_REPOSITORIES
        || evidence.schema != WORKSPACE_SCALE_EVIDENCE_SCHEMA
        || evidence.source_revision != source_revision
        || evidence.environment.operating_system != std::env::consts::OS
        || evidence.environment.architecture != std::env::consts::ARCH
        || evidence.environment.toolchain != toolchain
        || evidence.environment.build_profile != expected_profile
        || evidence.repositories_requested != repositories
        || evidence.repositories_indexed != repositories
        || evidence.repositories_restored != repositories
        || evidence.discovered_inputs < expected_files
        || evidence.indexed_files != expected_files
        || evidence.entities < repository_count
        || evidence.durable_state_bytes == 0
        || evidence.retained_memory_bytes == 0
        || evidence.initial_newly_written_bytes == 0
        || evidence.independent_update_newly_written_bytes == 0
        || evidence.independent_update_referenced_bytes == 0
        || evidence.independent_update_reserved_memory_bytes == 0
        || evidence.independent_update_owned_memory_bytes == 0
        || evidence.independent_update_owned_memory_bytes
            > evidence.independent_update_reserved_memory_bytes
        || evidence.service_open_micros == 0
        || evidence.index_wall_micros == 0
        || evidence.index_receipt_micros == 0
        || evidence.first_restore_micros == 0
        || evidence.independent_update_micros == 0
        || evidence.second_restore_micros == 0
        || evidence.total_elapsed_micros == 0
        || !evidence.durable_publication
        || !evidence.exact_generations_restored
        || !evidence.independent_update_published
        || !evidence.unrelated_generations_unchanged
        || !evidence.updated_generation_restored
        || !evidence.first_restore_query_parity
        || !evidence.second_restore_query_parity
        || evidence.bounds.maximum_repositories != MAX_REPOSITORIES
        || evidence.bounds.maximum_total_elapsed_micros != MAX_TOTAL_ELAPSED_MICROS
        || evidence.bounds.maximum_peak_rss_bytes != MAX_PEAK_RSS_BYTES
        || evidence.bounds.maximum_retained_memory_bytes != MAX_RETAINED_MEMORY_BYTES
        || !evidence.bounds.elapsed_within_bound
        || !evidence.bounds.retained_memory_within_bound
        || evidence.bounds.peak_rss_within_bound == Some(false)
        || (cfg!(target_os = "linux")
            && (!cpu_available
                || !rss_available
                || evidence.bounds.peak_rss_within_bound != Some(true)
                || evidence.environment.process_tree_measurement != "linux_proc_tree_sampled"))
        || (!cfg!(target_os = "linux")
            && (cpu_available
                || rss_available
                || evidence.bounds.peak_rss_within_bound.is_some()
                || evidence.environment.process_tree_measurement != "unavailable_on_this_platform"))
    {
        return Err(WorkspaceScaleEvidenceError::InvalidEvidence);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), WorkspaceScaleEvidenceError> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(WorkspaceScaleEvidenceError::InvalidRevision);
    }
    Ok(())
}

fn validate_toolchain(value: &str) -> Result<(), WorkspaceScaleEvidenceError> {
    if value.is_empty()
        || value.len() > 128
        || value
            .bytes()
            .any(|byte| !byte.is_ascii_graphic() && byte != b' ')
        || value.contains(['/', '\\'])
    {
        return Err(WorkspaceScaleEvidenceError::InvalidToolchain);
    }
    Ok(())
}

fn privacy_scan(encoded: &[u8]) -> Result<(), WorkspaceScaleEvidenceError> {
    const MARKERS: [&[u8]; 5] = [
        b"C:\\Users\\",
        b"C:\\\\Users\\\\",
        b"/home/",
        b"/Users/",
        b"file://",
    ];
    if MARKERS.iter().any(|marker| {
        encoded
            .windows(marker.len())
            .any(|window| window == *marker)
    }) {
        return Err(WorkspaceScaleEvidenceError::PrivacyBoundary);
    }
    Ok(())
}

/// Invalid, incomplete, or irreproducible workspace scale evidence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WorkspaceScaleEvidenceError {
    /// Repository count is outside the bounded candidate range.
    #[error("workspace scale repository count is invalid")]
    InvalidRepositoryCount,
    /// Source revision is not a canonical full identifier.
    #[error("workspace scale source revision is invalid")]
    InvalidRevision,
    /// Toolchain identity is absent or path-shaped.
    #[error("workspace scale toolchain identity is invalid")]
    InvalidToolchain,
    /// Temporary fixture or durable state I/O failed.
    #[error("workspace scale filesystem observation failed")]
    Filesystem,
    /// The production durable service failed a bounded operation.
    #[error("workspace scale service observation failed")]
    Service(#[source] FirstSliceError),
    /// The indexed or restored repository set differed.
    #[error("workspace scale repository set differs")]
    RepositorySet,
    /// Durable publication was unavailable.
    #[error("workspace scale durable publication is unavailable")]
    Durability,
    /// Exact generation recovery failed.
    #[error("workspace scale restart recovery differs")]
    Restore,
    /// Updating one repository affected another failure domain.
    #[error("workspace scale repository isolation differs")]
    Isolation,
    /// A restart changed the bounded query result for a pinned generation.
    #[error("workspace scale query parity differs")]
    QueryParity,
    /// Incremental publication omitted required reuse, write, or memory evidence.
    #[error("workspace scale incremental evidence is incomplete")]
    IncrementalEvidence,
    /// Process telemetry could not initialize.
    #[error("workspace scale process telemetry failed")]
    Telemetry,
    /// A bounded counter, collection, directory walk, or encoding overflowed.
    #[error("workspace scale resource limit was exceeded")]
    ResourceLimit,
    /// JSON encoding, decoding, or canonical form failed.
    #[error("workspace scale evidence encoding failed")]
    Encoding,
    /// Encoded evidence contained a private host path.
    #[error("workspace scale evidence crossed the privacy boundary")]
    PrivacyBoundary,
    /// Evidence fields do not satisfy the candidate contract.
    #[error("workspace scale evidence is invalid")]
    InvalidEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
    const TOOLCHAIN: &str = "rustc 1.90.0";

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_observation_path_is_canonical_and_private() {
        let fixture = observation_tempdir().expect("macOS observation directory exists");

        assert!(fixture.path().starts_with("/private/tmp"));
        assert_eq!(
            fs::canonicalize(fixture.path()).expect("observation directory canonicalizes"),
            fixture.path()
        );
    }

    #[test]
    #[ignore = "run serially by the platform and workspace-scale evidence gates"]
    fn one_repository_exercises_durable_restart_and_isolation() {
        let evidence = build_workspace_scale_evidence(1, REVISION, TOOLCHAIN)
            .expect("one-repository durable observation succeeds");
        let encoded =
            encode_workspace_scale_evidence(&evidence).expect("workspace evidence encodes");
        verify_workspace_scale_evidence(&encoded, 1, REVISION, TOOLCHAIN)
            .expect("workspace evidence verifies");
    }

    #[test]
    fn invalid_counts_and_private_paths_are_rejected() {
        assert!(privacy_scan(br#"{"path":"C:\\Users\\private"}"#).is_err());
        assert!(build_workspace_scale_evidence(0, REVISION, TOOLCHAIN).is_err());
        assert!(build_workspace_scale_evidence(MAX_REPOSITORIES + 1, REVISION, TOOLCHAIN).is_err());
    }
}
