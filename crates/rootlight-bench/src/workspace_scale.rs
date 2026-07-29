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
use rootlight_runtime::RuntimePaths;
use rootlight_service::{FirstSliceError, FirstSliceService};
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

use crate::{EvidenceValue, ProcessTreeMeasurement, ProcessTreeSample, ProcessTreeSampler};

#[cfg(target_os = "linux")]
use crate::LinuxProcTreeSampler;
#[cfg(not(target_os = "linux"))]
use crate::UnavailableProcessTreeSampler;

/// Wire identity for exact-run durable workspace scale evidence.
pub const WORKSPACE_SCALE_EVIDENCE_SCHEMA: &str = "rootlight.workspace-scale-evidence/1";
/// Maximum accepted encoded evidence size.
pub const WORKSPACE_SCALE_EVIDENCE_MAX_BYTES: usize = 64 * 1024;

const MAX_REPOSITORIES: usize = 100;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const RUN_DEADLINE: Duration = Duration::from_secs(600);
const LINUX_SAMPLE_INTERVAL: Duration = Duration::from_millis(1);
const MAX_TOTAL_ELAPSED_MICROS: u64 = 300_000_000;
const MAX_PEAK_RSS_BYTES: u64 = 2 * 1024 * 1024 * 1024;

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
    elapsed_within_bound: bool,
    peak_rss_within_bound: Option<bool>,
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

    let fixture = TempDir::new().map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
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
    for root in &repository_roots {
        let receipt = service
            .index_repository(root, &cancellation)
            .map_err(WorkspaceScaleEvidenceError::Service)?;
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
        generations.push((receipt.repository, receipt.generation));
    }
    let index_wall_micros = elapsed_micros(index_started)?;
    if service.list_repositories().len() != repositories {
        return Err(WorkspaceScaleEvidenceError::RepositorySet);
    }
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

    let first_root = repository_roots
        .first()
        .ok_or(WorkspaceScaleEvidenceError::RepositorySet)?;
    fs::write(
        first_root.join("src/lib.rs"),
        "pub fn value_000() -> u32 {\n    1\n}\n",
    )
    .map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
    let update_started = Instant::now();
    let updated = restored
        .index_repository(first_root, &cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    let independent_update_micros = elapsed_micros(update_started)?;
    let first_generation = generations
        .first()
        .map(|(_, generation)| *generation)
        .ok_or(WorkspaceScaleEvidenceError::RepositorySet)?;
    let independent_update_published =
        updated.repository == generations[0].0 && updated.generation != first_generation;
    let unrelated_generations_unchanged =
        generations.iter().skip(1).all(|(repository, generation)| {
            restored.active_generation_for(*repository) == Some(*generation)
        });
    if !independent_update_published || !unrelated_generations_unchanged {
        return Err(WorkspaceScaleEvidenceError::Isolation);
    }
    drop(restored);

    let second_restore_started = Instant::now();
    let restored = FirstSliceService::new_durable(2, &state_root, &cancellation)
        .map_err(WorkspaceScaleEvidenceError::Service)?;
    let second_restore_micros = elapsed_micros(second_restore_started)?;
    let updated_generation_restored = restored.active_generation_for(updated.repository)
        == Some(updated.generation)
        && restored.list_repositories().len() == repositories;
    if !updated_generation_restored {
        return Err(WorkspaceScaleEvidenceError::Restore);
    }
    drop(restored);

    let durable_state_bytes = directory_size(&state_root)?;
    let total_elapsed_micros = elapsed_micros(total_started)?;
    let measurement = sample.finish();
    let bounds = measurement_bounds(total_elapsed_micros, &measurement);
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
        bounds,
    };
    validate_evidence(&evidence, repositories, source_revision, toolchain)?;
    Ok(evidence)
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
        fs::create_dir(&repository).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::create_dir(&source_directory).map_err(|_| WorkspaceScaleEvidenceError::Filesystem)?;
        fs::write(
            source_directory.join("lib.rs"),
            format!("pub fn value_{ordinal:03}() -> u32 {{\n    {ordinal}\n}}\n"),
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
        elapsed_within_bound: total_elapsed_micros <= MAX_TOTAL_ELAPSED_MICROS,
        peak_rss_within_bound,
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
        || evidence.discovered_inputs < repository_count
        || evidence.indexed_files != repository_count
        || evidence.entities < repository_count
        || evidence.durable_state_bytes == 0
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
        || evidence.bounds.maximum_repositories != MAX_REPOSITORIES
        || evidence.bounds.maximum_total_elapsed_micros != MAX_TOTAL_ELAPSED_MICROS
        || evidence.bounds.maximum_peak_rss_bytes != MAX_PEAK_RSS_BYTES
        || !evidence.bounds.elapsed_within_bound
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
