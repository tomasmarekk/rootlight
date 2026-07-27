//! Release-process structural scalability, freshness, and resource evidence.

mod process_support;

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

#[cfg(target_os = "linux")]
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use rootlight_bench::{
    ColdIndexEvidence, FreshnessEvidence, MAX_STRUCTURAL_INDEX_SIZE_PPM,
    MAX_TEN_MILLION_LOC_RSS_BYTES, MIN_STRUCTURAL_LOC_PER_SECOND, SCALABILITY_EVIDENCE_SCHEMA,
    ScalabilityClass, ScalabilityDisposition, ScalabilityEnvironment, ScalabilityEvidence,
    ScalabilityFixture, StructuralFactCounts, StructuralResourceEvidence, StructuralThroughput,
    encode_scalability_evidence, scalability_distribution, sha256_hex,
    structural_durable_total_bound,
};
#[cfg(target_os = "linux")]
use rootlight_bench::{
    EvidenceValue, LinuxProcTreeSampler, ProcessTreeMeasurement, ProcessTreeSample,
    ProcessTreeSampler,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const EVIDENCE_PATH_ENV: &str = "ROOTLIGHT_SCALABILITY_EVIDENCE";
const LARGE_PHYSICAL_LOC: u64 = 1_000_000;
const TEN_MILLION_PHYSICAL_LOC: u64 = 10_000_000;
const LARGE_SOURCE_FILES: u64 = 500;
const TEN_MILLION_SOURCE_FILES: u64 = 5_000;
const STRUCTURAL_FUNCTIONS_PER_FILE: u64 = 64;
const COLD_INDEX_ATTEMPTS: usize = 5;
const FRESHNESS_SAMPLES: usize = 100;
const FRESHNESS_SOURCE_FILES: usize = 100;
const FRESHNESS_LINES_PER_FILE: usize = 100;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const PUBLICATION_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(target_os = "linux")]
const RESOURCE_POLL_INTERVAL: Duration = Duration::from_millis(2);
const TEMPORARY_FIXED_OVERHEAD_BYTES: u64 = 64 * 1024 * 1024;

#[test]
fn generated_scale_fixture_has_exact_loc_and_structural_density() {
    let fixture_root = process_support::private_process_tempdir("rl-scale-shape-");
    let fixture = write_scale_fixture(fixture_root.path(), "rust-shape-v1", 2, 1_000);
    let mut observed_loc = 0_u64;
    let mut observed_functions = 0_u64;
    for entry in
        fs::read_dir(fixture_root.path().join("src")).expect("shape fixture source directory reads")
    {
        let source = fs::read_to_string(entry.expect("shape fixture entry reads").path())
            .expect("shape fixture source reads");
        observed_loc = observed_loc
            .checked_add(u64::try_from(source.lines().count()).expect("line count fits u64"))
            .expect("fixture line count fits u64");
        observed_functions = observed_functions
            .checked_add(
                u64::try_from(source.matches("pub fn scale_").count())
                    .expect("function count fits u64"),
            )
            .expect("fixture function count fits u64");
    }
    assert_eq!(fixture.physical_loc, 1_000);
    assert_eq!(observed_loc, fixture.physical_loc);
    assert_eq!(observed_functions, 2 * STRUCTURAL_FUNCTIONS_PER_FILE);
}

#[test]
#[ignore = "runs candidate-bound 1M/10M-LOC indexing and 300 freshness publications"]
fn release_candidate_satisfies_structural_scalability_contract() {
    assert!(
        Path::new("/proc/self/stat").is_file(),
        "structural scalability evidence requires Linux /proc accounting"
    );
    let fixture_root = process_support::private_process_tempdir("rl-scale-fixtures-");
    let large_root = fixture_root.path().join("large-1m");
    let ten_million_root = fixture_root.path().join("large-10m");
    let large_fixture = write_scale_fixture(
        &large_root,
        "rust-large-1m-v2",
        LARGE_SOURCE_FILES,
        LARGE_PHYSICAL_LOC,
    );
    let ten_million_fixture = write_scale_fixture(
        &ten_million_root,
        "rust-large-10m-v2",
        TEN_MILLION_SOURCE_FILES,
        TEN_MILLION_PHYSICAL_LOC,
    );
    let daemon_binary = daemon_binary();
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let mut cold_elapsed = Vec::with_capacity(COLD_INDEX_ATTEMPTS);
    let mut cold_facts = None;
    for attempt in 0..COLD_INDEX_ATTEMPTS {
        let measured = measure_cold_index(
            &large_root,
            &daemon_binary,
            &mcp_binary,
            &format!("large-cold-{attempt:02}"),
        );
        if let Some(expected) = cold_facts {
            assert_eq!(
                measured.facts, expected,
                "independent cold indexes retain identical fact counts"
            );
        } else {
            cold_facts = Some(measured.facts);
        }
        cold_elapsed.push(measured.elapsed_ns);
    }
    let cold_facts = cold_facts.expect("cold indexing retains fact counts");
    let cold_throughput = throughput(&cold_elapsed, &large_fixture, cold_facts);
    let cold_index = ColdIndexEvidence {
        fixture: large_fixture,
        elapsed_ns: scalability_distribution(&cold_elapsed)
            .expect("cold-index distribution is available"),
        throughput: cold_throughput,
        elapsed_ns_samples: cold_elapsed,
        facts: cold_facts,
    };
    assert!(
        cold_index.throughput.physical_loc_per_second.p50 >= MIN_STRUCTURAL_LOC_PER_SECOND,
        "cold structural indexing missed its fixed throughput target: {:#?}",
        cold_index.throughput.physical_loc_per_second
    );

    let resource_measurement =
        measure_resource_scale(&ten_million_root, &daemon_binary, &mcp_binary);
    let temporary_bound_bytes = ten_million_fixture
        .source_bytes
        .checked_mul(2)
        .and_then(|value| value.checked_add(TEMPORARY_FIXED_OVERHEAD_BYTES))
        .expect("temporary-space bound fits u64");
    let structural_index_size_ppm = ratio_ppm(
        resource_measurement.structural_index_bytes,
        ten_million_fixture.source_bytes,
    );
    let durable_total_bound_bytes = structural_durable_total_bound(
        ten_million_fixture.source_bytes,
        resource_measurement.durable_snapshot_bytes,
    )
    .expect("durable total-space bound fits u64");
    let resources = StructuralResourceEvidence {
        fixture: ten_million_fixture,
        elapsed_ns: resource_measurement.elapsed_ns,
        facts: resource_measurement.facts,
        process_tree_peak_rss_bytes: resource_measurement.peak_rss_bytes,
        rss_limit_bytes: MAX_TEN_MILLION_LOC_RSS_BYTES,
        structural_index_bytes: resource_measurement.structural_index_bytes,
        structural_index_size_ppm,
        structural_index_size_limit_ppm: MAX_STRUCTURAL_INDEX_SIZE_PPM,
        durable_snapshot_bytes: resource_measurement.durable_snapshot_bytes,
        durable_total_bytes: resource_measurement.durable_total_bytes,
        durable_total_bound_bytes,
        temporary_peak_bytes: resource_measurement.temporary_peak_bytes,
        temporary_bound_bytes,
        temporary_residual_bytes: resource_measurement.temporary_residual_bytes,
        temporary_reclaimed: resource_measurement.temporary_residual_bytes == 0,
    };
    assert!(
        resources.process_tree_peak_rss_bytes <= resources.rss_limit_bytes,
        "ten-million-LOC process tree exceeded its RSS limit: {resources:#?}"
    );
    assert!(
        resources.structural_index_size_ppm <= resources.structural_index_size_limit_ppm,
        "ten-million-LOC structural index exceeded its source ratio: {resources:#?}"
    );
    assert!(
        resources.durable_total_bytes <= resources.durable_total_bound_bytes,
        "ten-million-LOC durable state exceeded its source-derived bound: {resources:#?}"
    );
    assert!(
        resources.temporary_peak_bytes <= resources.temporary_bound_bytes
            && resources.temporary_reclaimed,
        "temporary publication space violated its fixed bound: {resources:#?}"
    );

    let freshness = measure_freshness(&daemon_binary, &mcp_binary);
    let evidence = ScalabilityEvidence {
        schema: SCALABILITY_EVIDENCE_SCHEMA.to_owned(),
        environment: environment(&daemon_binary, &mcp_binary),
        cold_index,
        freshness,
        ten_million_loc_resources: resources,
        disposition: ScalabilityDisposition::Pass,
    };
    evidence
        .validate()
        .expect("candidate scalability evidence passes every fixed threshold");
    if let Some(path) = std::env::var_os(EVIDENCE_PATH_ENV) {
        let encoded = encode_scalability_evidence(&evidence).expect("scalability evidence encodes");
        fs::write(PathBuf::from(path), encoded).expect("scalability evidence writes");
    }
}

#[derive(Debug, Clone)]
struct MeasuredIndex {
    elapsed_ns: u64,
    facts: StructuralFactCounts,
    #[cfg(target_os = "linux")]
    repository_id: String,
    #[cfg(target_os = "linux")]
    generation_id: String,
}

#[derive(Debug, Clone, Copy)]
struct ResourceScaleMeasurement {
    elapsed_ns: u64,
    facts: StructuralFactCounts,
    peak_rss_bytes: u64,
    structural_index_bytes: u64,
    durable_snapshot_bytes: u64,
    durable_total_bytes: u64,
    temporary_peak_bytes: u64,
    temporary_residual_bytes: u64,
}

fn measure_cold_index(
    repository_root: &Path,
    daemon_binary: &Path,
    mcp_binary: &Path,
    id: &str,
) -> MeasuredIndex {
    let isolated = process_support::private_process_tempdir("rl-scale-cold-");
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    let mut daemon = DaemonProcess::spawn(daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(mcp_binary, &state_dir, &runtime_dir);
    let measured = index_repository(&mut mcp, repository_root, &state_dir, id);
    mcp.finish();
    daemon.finish();
    measured
}

#[cfg(target_os = "linux")]
fn measure_resource_scale(
    repository_root: &Path,
    daemon_binary: &Path,
    mcp_binary: &Path,
) -> ResourceScaleMeasurement {
    let isolated = process_support::private_process_tempdir("rl-scale-resource-");
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    let mut daemon = DaemonProcess::spawn(daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(mcp_binary, &state_dir, &runtime_dir);
    let daemon_sample = LinuxProcTreeSampler::new(daemon.pid(), RESOURCE_POLL_INTERVAL)
        .expect("daemon process-tree sampler initializes")
        .begin();
    let mcp_sample = LinuxProcTreeSampler::new(mcp.pid(), RESOURCE_POLL_INTERVAL)
        .expect("MCP process-tree sampler initializes")
        .begin();
    let temporary_monitor = TemporarySpaceMonitor::start(state_dir.clone());
    let measured = index_repository(
        &mut mcp,
        repository_root,
        &state_dir,
        "ten-million-resource",
    );
    let temporary_peak_bytes = temporary_monitor.finish();
    let peak_rss_bytes = sum_peak_rss(daemon_sample.finish(), mcp_sample.finish());
    let generation_root =
        generation_root(&state_dir, &measured.repository_id, &measured.generation_id);
    let oracle_path = generation_root.join("oracle.sqlite3");
    let structural_index_bytes = fs::metadata(&oracle_path)
        .expect("sealed oracle metadata reads")
        .len();
    let durable_snapshot_bytes = tree_bytes(&generation_root.join("sources"));
    let durable_total_bytes = tree_bytes(&state_dir);
    let temporary_residual_bytes = staging_bytes(&state_dir);
    mcp.finish();
    daemon.finish();
    ResourceScaleMeasurement {
        elapsed_ns: measured.elapsed_ns,
        facts: measured.facts,
        peak_rss_bytes,
        structural_index_bytes,
        durable_snapshot_bytes,
        durable_total_bytes,
        temporary_peak_bytes,
        temporary_residual_bytes,
    }
}

#[cfg(not(target_os = "linux"))]
fn measure_resource_scale(
    _repository_root: &Path,
    _daemon_binary: &Path,
    _mcp_binary: &Path,
) -> ResourceScaleMeasurement {
    panic!("structural resource evidence requires Linux /proc accounting")
}

fn measure_freshness(daemon_binary: &Path, mcp_binary: &Path) -> Vec<FreshnessEvidence> {
    let isolated = process_support::private_process_tempdir("rl-scale-freshness-");
    let repository_root = isolated.path().join("repository");
    write_freshness_fixture(&repository_root);
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    let mut daemon = DaemonProcess::spawn(daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(mcp_binary, &state_dir, &runtime_dir);
    let _initial = index_repository(&mut mcp, &repository_root, &state_dir, "freshness-initial");
    let mut output = Vec::with_capacity(3);
    let mut edit_generation = 0_u64;
    for (changed_files, p95_limit_ns) in [
        (1_u32, 500_000_000_u64),
        (10, 2_000_000_000),
        (100, 10_000_000_000),
    ] {
        let mut samples = Vec::with_capacity(FRESHNESS_SAMPLES);
        for ordinal in 0..FRESHNESS_SAMPLES {
            edit_generation = edit_generation.saturating_add(1);
            edit_fixture_files(
                &repository_root,
                usize::try_from(changed_files).expect("changed-file count fits usize"),
                edit_generation,
            );
            let measured = index_repository(
                &mut mcp,
                &repository_root,
                &state_dir,
                &format!("freshness-{changed_files}-{ordinal:03}"),
            );
            samples.push(measured.elapsed_ns);
        }
        let elapsed_ns =
            scalability_distribution(&samples).expect("freshness distribution is available");
        assert!(
            elapsed_ns.p95 <= p95_limit_ns,
            "{changed_files}-file freshness exceeded p95 limit: {elapsed_ns:#?}"
        );
        output.push(FreshnessEvidence {
            changed_files,
            elapsed_ns_samples: samples,
            elapsed_ns,
            p95_limit_ns,
        });
    }
    mcp.finish();
    daemon.finish();
    output
}

fn index_repository(
    mcp: &mut McpProcess,
    root: &Path,
    state_dir: &Path,
    id: &str,
) -> MeasuredIndex {
    let started = Instant::now();
    let response = mcp.call(
        id,
        "repo.index",
        json!({"root": root, "mode": "auto", "detached": true}),
    );
    assert_success(&response, "repo.index");
    let data = &response["result"]["structuredContent"]["data"];
    let repository_id = required_string(&data["repository_id"], "repository identity");
    let operation_id = required_string(&data["operation_id"], "operation identity");
    let generation_id = if data["state"] == "published" {
        required_string(&data["published_generation"], "published generation")
    } else {
        wait_for_publication(mcp, &operation_id)
    };
    let elapsed_ns = duration_ns(started.elapsed());
    let facts = read_fact_counts(&generation_root(state_dir, &repository_id, &generation_id));
    MeasuredIndex {
        elapsed_ns,
        facts,
        #[cfg(target_os = "linux")]
        repository_id,
        #[cfg(target_os = "linux")]
        generation_id,
    }
}

fn wait_for_publication(mcp: &mut McpProcess, operation_id: &str) -> String {
    let deadline = Instant::now() + PUBLICATION_TIMEOUT;
    let mut attempt = 0_u64;
    while Instant::now() < deadline {
        let response = mcp.call(
            &format!("publication-{operation_id}-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 1_000}),
        );
        assert_success(&response, "operation.status");
        let data = &response["result"]["structuredContent"]["data"];
        match data["operation"]["state"].as_str() {
            Some("published") => {
                return required_string(&data["published_generation"], "published generation");
            }
            Some("failed" | "cancelled") => {
                panic!("scalability indexing terminated without publication: {response:#}");
            }
            _ => {}
        }
        attempt = attempt.saturating_add(1);
    }
    panic!("scalability indexing did not publish within the fixed timeout");
}

fn generation_root(state_dir: &Path, repository_id: &str, generation_id: &str) -> PathBuf {
    state_dir
        .join("first-slice")
        .join("repositories")
        .join(repository_id)
        .join(generation_id)
}

fn read_fact_counts(generation_root: &Path) -> StructuralFactCounts {
    let path = generation_root.join("oracle.sqlite3");
    let connection = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|error| panic!("sealed oracle {path:?} opens read-only: {error}"));
    let (files, entities, occurrences, relations) = connection
        .query_row(
            "SELECT file_count, entity_count, occurrence_count, relation_count
             FROM generation_meta WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .expect("sealed oracle fact counts read");
    let files = u64::try_from(files).expect("sealed file count is non-negative");
    let entities = u64::try_from(entities).expect("sealed entity count is non-negative");
    let occurrences = u64::try_from(occurrences).expect("sealed occurrence count is non-negative");
    let relations = u64::try_from(relations).expect("sealed relation count is non-negative");
    let total_facts = entities
        .checked_add(occurrences)
        .and_then(|value| value.checked_add(relations))
        .expect("fact count fits u64");
    StructuralFactCounts {
        files,
        entities,
        occurrences,
        relations,
        total_facts,
    }
}

fn throughput(
    elapsed_ns: &[u64],
    fixture: &ScalabilityFixture,
    facts: StructuralFactCounts,
) -> StructuralThroughput {
    StructuralThroughput {
        physical_loc_per_second: rate_distribution(fixture.physical_loc, elapsed_ns),
        source_bytes_per_second: rate_distribution(fixture.source_bytes, elapsed_ns),
        files_per_second: rate_distribution(facts.files, elapsed_ns),
        entities_per_second: rate_distribution(facts.entities, elapsed_ns),
        occurrences_per_second: rate_distribution(facts.occurrences, elapsed_ns),
        relations_per_second: rate_distribution(facts.relations, elapsed_ns),
        total_facts_per_second: rate_distribution(facts.total_facts, elapsed_ns),
    }
}

fn rate_distribution(numerator: u64, elapsed_ns: &[u64]) -> rootlight_bench::ObservedDistribution {
    let samples = elapsed_ns
        .iter()
        .map(|elapsed| {
            numerator
                .checked_mul(1_000_000_000)
                .expect("throughput numerator fits u64")
                / elapsed
        })
        .collect::<Vec<_>>();
    scalability_distribution(&samples).expect("throughput distribution is available")
}

fn write_scale_fixture(
    root: &Path,
    fixture_id: &str,
    source_files: u64,
    physical_loc: u64,
) -> ScalabilityFixture {
    assert_eq!(physical_loc % source_files, 0);
    fs::create_dir_all(root.join("src")).expect("scale fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"rootlight_scale_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("scale fixture manifest is written");
    let lines_per_file = physical_loc / source_files;
    let fixed_structural_lines = 1_u64
        .checked_add(
            STRUCTURAL_FUNCTIONS_PER_FILE
                .checked_mul(5)
                .expect("function-line count fits u64"),
        )
        .and_then(|lines| lines.checked_add(STRUCTURAL_FUNCTIONS_PER_FILE + 4))
        .and_then(|lines| lines.checked_add(2))
        .expect("fixed structural line count fits u64");
    assert!(
        lines_per_file > fixed_structural_lines,
        "scale fixture needs room for parsed data declarations"
    );
    for file_index in 0..source_files {
        let path = root.join("src").join(format!("scale_{file_index:04}.rs"));
        let file = fs::File::create(&path).expect("scale source file creates");
        let mut writer = std::io::BufWriter::with_capacity(1024 * 1024, file);
        writeln!(writer, "use std::hint::black_box;").expect("scale import writes");
        for function_index in 0..STRUCTURAL_FUNCTIONS_PER_FILE {
            let rotation = u32::try_from(function_index % 63 + 1).expect("rotation fits u32");
            let salt = file_index
                .checked_mul(STRUCTURAL_FUNCTIONS_PER_FILE)
                .and_then(|value| value.checked_add(function_index + 1))
                .expect("scale salt fits u64");
            writeln!(writer, "#[inline]").expect("scale attribute writes");
            writeln!(
                writer,
                "pub fn scale_{file_index:04}_{function_index:02}(value: u64) -> u64 {{"
            )
            .expect("scale function signature writes");
            writeln!(
                writer,
                "    let rotated = black_box(value).rotate_left({rotation});"
            )
            .expect("scale function body writes");
            writeln!(writer, "    rotated.wrapping_add({salt})")
                .expect("scale function result writes");
            writeln!(writer, "}}").expect("scale function closes");
        }
        writeln!(
            writer,
            "pub fn dispatch_{file_index:04}(value: u64) -> u64 {{"
        )
        .expect("scale dispatcher signature writes");
        writeln!(writer, "    let mut current = value;").expect("scale dispatcher seed writes");
        for function_index in 0..STRUCTURAL_FUNCTIONS_PER_FILE {
            writeln!(
                writer,
                "    current = scale_{file_index:04}_{function_index:02}(current);"
            )
            .expect("scale call edge writes");
        }
        writeln!(writer, "    current").expect("scale dispatcher result writes");
        writeln!(writer, "}}").expect("scale dispatcher closes");
        writeln!(writer, "pub const SCALE_DATA_{file_index:04}: &[u64] = &[")
            .expect("scale data declaration writes");
        for item in 0..(lines_per_file - fixed_structural_lines) {
            let value = file_index
                .checked_mul(lines_per_file)
                .and_then(|base| base.checked_add(item))
                .expect("scale data value fits u64");
            writeln!(writer, "    {value},").expect("parsed scale data entry writes");
        }
        writeln!(writer, "];").expect("scale data declaration closes");
        writer.flush().expect("scale source flushes");
    }
    fixture_manifest(root, fixture_id, source_files, physical_loc)
}

fn fixture_manifest(
    root: &Path,
    fixture_id: &str,
    source_files: u64,
    physical_loc: u64,
) -> ScalabilityFixture {
    let mut paths = fs::read_dir(root.join("src"))
        .expect("scale source directory reads")
        .collect::<Result<Vec<_>, _>>()
        .expect("scale source entries read");
    paths.sort_by_key(fs::DirEntry::file_name);
    let mut hasher = Sha256::new();
    let mut source_bytes = 0_u64;
    for entry in paths {
        let path = entry.path();
        let file_name = entry.file_name();
        let metadata = fs::symlink_metadata(&path).expect("scale source metadata reads");
        assert!(metadata.file_type().is_file());
        source_bytes = source_bytes
            .checked_add(metadata.len())
            .expect("source bytes fit u64");
        hash_length_prefixed(&mut hasher, file_name.as_encoded_bytes());
        let mut file = fs::File::open(path).expect("scale source file opens");
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).expect("scale source reads");
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    let mut language_loc = BTreeMap::new();
    language_loc.insert("rust".to_owned(), physical_loc);
    ScalabilityFixture {
        fixture_id: fixture_id.to_owned(),
        scale_class: ScalabilityClass::Large,
        fixture_sha256: hex_digest(hasher.finalize()),
        source_files,
        source_bytes,
        physical_loc,
        generated_bytes: 0,
        excluded_bytes: 0,
        language_loc,
    }
}

fn write_freshness_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("freshness source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"rootlight_freshness_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("freshness manifest is written");
    for file_index in 0..FRESHNESS_SOURCE_FILES {
        write_freshness_file(root, file_index, 0);
    }
}

fn edit_fixture_files(root: &Path, changed_files: usize, generation: u64) {
    for file_index in 0..changed_files {
        write_freshness_file(root, file_index, generation);
    }
}

fn write_freshness_file(root: &Path, file_index: usize, generation: u64) {
    let path = root.join("src").join(format!("unit_{file_index:03}.rs"));
    let mut source = format!(
        "pub fn unit_{file_index:03}(value: u64) -> u64 {{ value.saturating_add({}) }}\n",
        generation % 2
    );
    for _ in 1..FRESHNESS_LINES_PER_FILE {
        source.push_str("// ordinary body-edit fixture line\n");
    }
    fs::write(path, source).expect("freshness source writes");
}

fn environment(daemon_binary: &Path, mcp_binary: &Path) -> ScalabilityEnvironment {
    let rustc_verbose = command_output("rustc", &["-Vv"]);
    let target_triple = rustc_verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc reports its host target")
        .to_owned();
    let cpu_model = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                line.split_once(':')
                    .map(|(_, value)| value.trim().to_owned())
            })
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unavailable".to_owned());
    let memory_kib = fs::read_to_string("/proc/meminfo")
        .expect("Linux memory information reads")
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|value| value.split_ascii_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
        })
        .expect("Linux memory information reports total memory");
    let mut binary_sha256 = BTreeMap::new();
    binary_sha256.insert("rootlight-daemon".to_owned(), sha256_file(daemon_binary));
    binary_sha256.insert("rootlight-mcp".to_owned(), sha256_file(mcp_binary));
    ScalabilityEnvironment {
        source_revision: source_revision(),
        target_triple,
        operating_system: "linux".to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        cpu_model: normalize_identifier(&cpu_model),
        cpu_count: u32::try_from(
            thread::available_parallelism()
                .expect("available parallelism is reported")
                .get(),
        )
        .expect("CPU count fits u32"),
        memory_bytes: memory_kib.saturating_mul(1_024),
        build_profile: "release".to_owned(),
        binary_sha256,
    }
}

fn source_revision() -> String {
    let revision = std::env::var("SOURCE_REVISION").unwrap_or_else(|_| {
        command_output("git", &["rev-parse", "HEAD"])
            .trim()
            .to_owned()
    });
    assert!(
        revision.len() == 40
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "source revision is an exact lowercase Git object ID"
    );
    revision
}

#[cfg(target_os = "linux")]
struct TemporarySpaceMonitor {
    stop: Arc<AtomicBool>,
    peak: Arc<AtomicU64>,
    worker: JoinHandle<()>,
}

#[cfg(target_os = "linux")]
impl TemporarySpaceMonitor {
    fn start(state_dir: PathBuf) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicU64::new(0));
        let worker_stop = Arc::clone(&stop);
        let worker_peak = Arc::clone(&peak);
        let worker = thread::spawn(move || {
            while !worker_stop.load(Ordering::Acquire) {
                worker_peak.fetch_max(staging_bytes(&state_dir), Ordering::AcqRel);
                thread::sleep(RESOURCE_POLL_INTERVAL);
            }
            worker_peak.fetch_max(staging_bytes(&state_dir), Ordering::AcqRel);
        });
        Self { stop, peak, worker }
    }

    fn finish(self) -> u64 {
        self.stop.store(true, Ordering::Release);
        self.worker.join().expect("temporary-space monitor joins");
        self.peak.load(Ordering::Acquire)
    }
}

#[cfg(target_os = "linux")]
fn staging_bytes(root: &Path) -> u64 {
    fn walk(directory: &Path, in_staging: bool) -> u64 {
        let Ok(entries) = fs::read_dir(directory) else {
            return 0;
        };
        let mut total = 0_u64;
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let name_is_staging = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with("stage-"));
            let include = in_staging || name_is_staging;
            if metadata.is_dir() {
                total = total.saturating_add(walk(&path, include));
            } else if metadata.is_file() && include {
                total = total.saturating_add(metadata.len());
            }
        }
        total
    }
    walk(root, false)
}

#[cfg(target_os = "linux")]
fn tree_bytes(root: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return 0;
    };
    if metadata.file_type().is_symlink() {
        return 0;
    }
    if metadata.is_file() {
        return metadata.len();
    }
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| tree_bytes(&entry.path()))
        .fold(0, u64::saturating_add)
}

#[cfg(target_os = "linux")]
fn sum_peak_rss(daemon: ProcessTreeMeasurement, mcp: ProcessTreeMeasurement) -> u64 {
    observed(daemon.peak_rss_bytes).saturating_add(observed(mcp.peak_rss_bytes))
}

#[cfg(target_os = "linux")]
fn observed(value: EvidenceValue<u64>) -> u64 {
    match value {
        EvidenceValue::Observed { value } => value,
        EvidenceValue::Target { .. } => {
            panic!("mandatory Linux resource measurement cannot be a target")
        }
        EvidenceValue::Unavailable { reason_code } => {
            panic!("mandatory Linux resource measurement is unavailable: {reason_code}")
        }
    }
}

fn ratio_ppm(numerator: u64, denominator: u64) -> u64 {
    numerator
        .checked_mul(1_000_000)
        .expect("ratio numerator fits u64")
        / denominator
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos())
        .unwrap_or(u64::MAX)
        .max(1)
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(
        u64::try_from(bytes.len())
            .expect("fixture component length fits u64")
            .to_le_bytes(),
    );
    hasher.update(bytes);
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn sha256_file(path: &Path) -> String {
    sha256_hex(&fs::read(path).unwrap_or_else(|error| panic!("{path:?} reads: {error}")))
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("{program} starts: {error}"));
    assert!(
        output.status.success(),
        "{program} succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("command output is UTF-8")
}

fn normalize_identifier(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len().min(128));
    for character in value.chars() {
        if normalized.len() >= 128 {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':') {
            normalized.push(character.to_ascii_lowercase());
        } else if !normalized.ends_with('_') {
            normalized.push('_');
        }
    }
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "unavailable".to_owned()
    } else {
        normalized.to_owned()
    }
}

fn assert_success(response: &Value, tool: &str) {
    assert_ne!(
        response["result"]["isError"], true,
        "{tool} returned a public error: {response:#}"
    );
    assert!(
        response["result"]["structuredContent"].is_object(),
        "{tool} omitted structured content: {response:#}"
    );
}

fn required_string(value: &Value, field: &str) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{field} is absent: {value:#}"))
        .to_owned()
}

fn daemon_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"))
        .parent()
        .expect("MCP binary has a profile directory")
        .join(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX))
}

struct DaemonProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    stderr_reader: Option<JoinHandle<String>>,
}

impl DaemonProcess {
    fn spawn(binary: &Path, state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut child = Command::new(binary)
            .arg("--supervised-stdio")
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("isolated daemon process starts");
        let input = child.stdin.take().expect("daemon stdin is piped");
        let stderr = child.stderr.take().expect("daemon stderr is piped");
        let stderr_reader = thread::spawn(move || {
            let mut output = String::new();
            BufReader::new(stderr)
                .read_to_string(&mut output)
                .expect("daemon stderr reads");
            output
        });
        Self {
            child: Some(child),
            input: Some(input),
            stderr_reader: Some(stderr_reader),
        }
    }

    #[cfg(target_os = "linux")]
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("daemon child is retained").id()
    }

    fn wait_until_ready(&mut self, runtime_dir: &Path) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let discovery = runtime_dir.join("daemon.json");
        while Instant::now() < deadline {
            if discovery.is_file() {
                return;
            }
            assert!(
                self.child
                    .as_mut()
                    .expect("daemon child is retained")
                    .try_wait()
                    .expect("daemon status is readable")
                    .is_none(),
                "daemon exited before publishing discovery"
            );
            thread::sleep(POLL_INTERVAL);
        }
        panic!("daemon did not publish discovery within the startup bound");
    }

    fn finish(&mut self) {
        self.input.take();
        let child = self.child.as_mut().expect("daemon child is retained");
        let status = wait_for_exit(child, SHUTDOWN_TIMEOUT);
        self.child.take();
        let stderr = self
            .stderr_reader
            .take()
            .expect("daemon stderr reader is retained")
            .join()
            .expect("daemon stderr reader joins");
        assert!(
            status.success(),
            "daemon process exits successfully: {stderr}"
        );
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

struct McpProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    responses: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl McpProcess {
    fn spawn(binary: &Path, state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut child = Command::new(binary)
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env("ROOTLIGHT_MCP_PROFILE", "developer")
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP scalability process starts");
        let output = child.stdout.take().expect("MCP stdout is piped");
        let (responses_tx, responses) = mpsc::sync_channel(64);
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    return;
                };
                if responses_tx.send(line).is_err() {
                    return;
                }
            }
        });
        let mut process = Self {
            input: child.stdin.take(),
            child: Some(child),
            responses,
            reader: Some(reader),
        };
        process.write(&json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "scalability-process", "version": "1.0"},
                "initializationOptions": {"rootlight_exposure_profile": "developer"}
            }
        }));
        assert_eq!(process.read()["id"], "initialize");
        process.write(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));
        process
    }

    #[cfg(target_os = "linux")]
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("MCP child is retained").id()
    }

    fn call(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }));
        let response = self.read();
        assert_eq!(response["id"], id, "MCP response identity differs");
        response
    }

    fn write(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("MCP stdin is retained");
        serde_json::to_writer(&mut *input, message).expect("MCP request serializes");
        input.write_all(b"\n").expect("MCP request terminates");
        input.flush().expect("MCP request flushes");
    }

    fn read(&self) -> Value {
        let line = self
            .responses
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("MCP response arrives within the bound");
        serde_json::from_str(&line).expect("MCP response is valid JSON")
    }

    fn finish(&mut self) {
        self.input.take();
        let child = self.child.as_mut().expect("MCP child is retained");
        let status = wait_for_exit(child, SHUTDOWN_TIMEOUT);
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("MCP stderr is piped")
            .read_to_string(&mut stderr)
            .expect("MCP stderr reads");
        assert!(status.success(), "MCP process exits successfully: {stderr}");
        assert!(stderr.is_empty(), "MCP process wrote stderr: {stderr}");
        self.child.take();
        self.reader
            .take()
            .expect("MCP reader is retained")
            .join()
            .expect("MCP reader joins");
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("child status is readable") {
            return status;
        }
        thread::sleep(POLL_INTERVAL);
    }
    child.kill().expect("timed-out child terminates");
    child.wait().expect("terminated child reaps")
}

fn terminate(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}
