//! Release-process regression coverage for the MCP bridge startup SLO.

mod process_support;

use std::{
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_client::{Client, ConnectPolicy, GenerationSelector, RepositoryIndexMode};
use rootlight_ids::{GenerationId, OperationId, RepositoryId};
use rootlight_runtime::RuntimePaths;
use serde_json::Value;

const WARMUP_SAMPLES: usize = 5;
const MEASURED_SAMPLES: usize = 100;
const P50_TARGET_US: u64 = 80_000;
const P95_TARGET_US: u64 = 150_000;
const P99_TARGET_US: u64 = 300_000;
// Readiness follows restoration of every retained generation, so this recovery
// budget scales with fixture cardinality while the bridge SLO remains absolute.
const DAEMON_RECOVERY_P50_PER_REPOSITORY_US: u64 = 125_000;
const DAEMON_RECOVERY_P95_PER_REPOSITORY_US: u64 = 200_000;
const DAEMON_RECOVERY_P99_PER_REPOSITORY_US: u64 = 250_000;
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_REPOSITORIES: usize = 24;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);
const FAILURE_STDERR_LIMIT: u64 = 16 * 1024;

#[test]
#[ignore = "runs 105 fresh release MCP processes and enforces startup percentiles"]
fn bridge_initializes_within_release_startup_slo() {
    let isolated = process_support::private_process_tempdir("rl-startup-bridge-");
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    fs::create_dir_all(&state_dir).expect("daemon state directory is created");
    let (mut daemon, daemon_input, _startup, control) =
        start_healthy_daemon(&state_dir, &runtime_dir);

    for _ in 0..WARMUP_SAMPLES {
        let _elapsed = initialize_process(&state_dir, &runtime_dir);
    }
    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        samples.push(initialize_process(&state_dir, &runtime_dir));
    }
    samples.sort_unstable();

    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    let p99 = percentile(&samples, 99);
    assert!(
        p50 <= P50_TARGET_US && p95 <= P95_TARGET_US && p99 <= P99_TARGET_US,
        "bridge startup exceeded release SLO: p50={p50}us/{P50_TARGET_US}us, \
         p95={p95}us/{P95_TARGET_US}us, p99={p99}us/{P99_TARGET_US}us, \
         min={}us, max={}us",
        samples[0],
        samples[samples.len() - 1],
    );
    assert!(
        control.health().is_ok_and(|health| health.ready),
        "daemon remains health-ready throughout bridge measurements"
    );
    stop_daemon(daemon_input, &mut daemon);
}

#[test]
#[ignore = "captures one first-process latency sample without enforcing a threshold"]
fn first_bridge_initialization_reports_cold_telemetry() {
    let isolated = process_support::private_process_tempdir("rl-startup-cold-");
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    fs::create_dir_all(&state_dir).expect("daemon state directory is created");
    let (mut daemon, daemon_input, _startup, control) =
        start_healthy_daemon(&state_dir, &runtime_dir);

    let elapsed_us = initialize_process(&state_dir, &runtime_dir);
    println!(
        "{}",
        serde_json::json!({
            "schema": "rootlight.mcp-first-process/1",
            "phase": "cold_first_process",
            "elapsed_us": elapsed_us,
            "gating": false
        })
    );
    assert!(elapsed_us > 0);
    assert!(
        control.health().is_ok_and(|health| health.ready),
        "daemon remains health-ready after the first bridge process"
    );
    stop_daemon(daemon_input, &mut daemon);
}

#[test]
#[ignore = "runs 105 fresh release daemon processes and enforces startup percentiles"]
fn daemon_reaches_ready_within_release_startup_slo() {
    let isolated = process_support::private_process_tempdir("rl-startup-daemon-");
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    let repositories_root = isolated.path().join("repositories");
    let repositories = prepare_valid_catalog(&state_dir, &runtime_dir, &repositories_root);

    for _ in 0..WARMUP_SAMPLES {
        let _elapsed = start_ready_daemon(&state_dir, &runtime_dir);
    }
    let mut samples = Vec::with_capacity(MEASURED_SAMPLES);
    for _ in 0..MEASURED_SAMPLES {
        samples.push(start_ready_daemon(&state_dir, &runtime_dir));
    }
    samples.sort_unstable();

    let p50 = percentile(&samples, 50);
    let p95 = percentile(&samples, 95);
    let p99 = percentile(&samples, 99);
    let targets = daemon_recovery_targets();
    assert!(
        p50 <= targets.p50 && p95 <= targets.p95 && p99 <= targets.p99,
        "daemon retained-generation recovery exceeded release budget: \
         repositories={STARTUP_REPOSITORIES}, p50={p50}us/{}us, \
         p95={p95}us/{}us, p99={p99}us/{}us, \
         min={}us, max={}us",
        targets.p50,
        targets.p95,
        targets.p99,
        samples[0],
        samples[samples.len() - 1],
    );

    // Keep endpoint checks outside the latency samples so the percentiles
    // continue to represent daemon readiness rather than fixture cardinality.
    let (mut daemon, input, _elapsed, control) = start_healthy_daemon(&state_dir, &runtime_dir);
    let health = control
        .health()
        .expect("health remains readable after multi-repository recovery");
    assert!(health.ready);
    assert!(
        health.accepting_operations,
        "ready health must agree with write admission"
    );
    for repository in &repositories {
        let status = control
            .repository_status(
                repository.repository,
                GenerationSelector::Generation(repository.generation),
            )
            .expect("every recovered last-good generation is readable");
        assert_eq!(status.resolved_generation, repository.generation);
    }
    assert_mcp_reads_active_generation(&state_dir, &runtime_dir, &repositories[0]);
    let refreshed = control
        .repository_index_with_mode(
            &repositories[0].root.to_string_lossy(),
            startup_operation_id(STARTUP_REPOSITORIES, 0x53),
            false,
            RepositoryIndexMode::Structural,
        )
        .expect("ready health admits a repository write");
    assert_eq!(refreshed.repository, repositories[0].repository);
    stop_daemon(input, &mut daemon);
}

fn initialize_process(state_dir: &Path, runtime_dir: &Path) -> u64 {
    let mcp_binary = std::env::var_os("ROOTLIGHT_TEST_MCP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp")));
    let mut command = Command::new(mcp_binary);
    command
        .env("ROOTLIGHT_STATE_DIR", state_dir)
        .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().expect("MCP process starts");
    let mut stdin = child.stdin.take().expect("MCP stdin is piped");
    writeln!(
        stdin,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "rootlight-startup-regression",
                    "version": "1.0"
                }
            }
        })
    )
    .expect("initialize request writes");
    stdin.flush().expect("initialize request flushes");
    let stdout = child.stdout.take().expect("MCP stdout is piped");
    let mut output = BufReader::new(stdout);
    let mut line = String::new();
    output
        .read_line(&mut line)
        .expect("initialize response reads");
    let elapsed = u64::try_from(started.elapsed().as_micros()).expect("latency fits u64");
    let response: Value = serde_json::from_str(&line).expect("initialize response is JSON");
    assert_eq!(response["id"], "initialize");
    assert_eq!(response["result"]["protocolVersion"], "2025-11-25");

    drop(stdin);
    drop(output);
    finish_process(&mut child, "MCP");
    elapsed
}

fn start_ready_daemon(state_dir: &Path, runtime_dir: &Path) -> u64 {
    let (mut child, input, elapsed, _control) = start_healthy_daemon(state_dir, runtime_dir);
    stop_daemon(input, &mut child);
    elapsed
}

fn start_healthy_daemon(state_dir: &Path, runtime_dir: &Path) -> (Child, ChildStdin, u64, Client) {
    let discovery = runtime_dir.join("daemon.json");
    if discovery.exists() {
        fs::remove_file(&discovery).expect("stale discovery record is removed");
    }
    let started = Instant::now();
    let mut child = Command::new(daemon_binary())
        .arg("--supervised-stdio")
        .env("ROOTLIGHT_STATE_DIR", state_dir)
        .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon process starts");
    let input = child.stdin.take().expect("daemon stdin is piped");
    let deadline = started + DAEMON_READY_TIMEOUT;
    let paths = RuntimePaths::new(state_dir.to_path_buf(), runtime_dir.to_path_buf())
        .expect("isolated runtime paths are valid");
    while Instant::now() < deadline {
        if discovery.is_file()
            && let Ok(client) =
                Client::connect_or_start(&paths, [0x51; 16], ConnectPolicy::ExistingOnly)
            && client.health().is_ok_and(|health| health.ready)
        {
            let elapsed = u64::try_from(started.elapsed().as_micros()).expect("latency fits u64");
            return (child, input, elapsed, client);
        }
        assert!(
            child
                .try_wait()
                .expect("daemon process status reads")
                .is_none(),
            "daemon exited before reaching ready"
        );
        thread::sleep(EXIT_POLL_INTERVAL);
    }
    child.kill().expect("timed-out daemon terminates");
    let _status = child.wait().expect("timed-out daemon reaps");
    panic!("daemon did not reach ready within the startup timeout");
}

fn prepare_valid_catalog(
    state_dir: &Path,
    runtime_dir: &Path,
    repositories_root: &Path,
) -> Vec<PreparedRepository> {
    fs::create_dir_all(repositories_root).expect("fixture repository root is created");
    let (mut daemon, input, _startup, control) = start_healthy_daemon(state_dir, runtime_dir);
    let mut prepared = Vec::with_capacity(STARTUP_REPOSITORIES);
    for index in 0..STARTUP_REPOSITORIES {
        let repository = repositories_root.join(format!("repository-{index:03}"));
        let symbol = format!("startup_fixture_{index:03}");
        fs::create_dir_all(repository.join("src")).expect("fixture source directory is created");
        fs::write(
            repository.join("Cargo.toml"),
            format!(
                "[package]\nname = \"startup_fixture_{index:03}\"\n\
                 version = \"0.1.0\"\nedition = \"2024\"\n"
            ),
        )
        .expect("fixture manifest is written");
        fs::write(
            repository.join("src").join("lib.rs"),
            format!("pub fn {symbol}() -> bool {{ true }}\n"),
        )
        .expect("fixture source is written");

        let operation = startup_operation_id(index, 0x52);
        let indexed = control
            .repository_index_with_mode(
                &repository.to_string_lossy(),
                operation,
                false,
                RepositoryIndexMode::Structural,
            )
            .expect("fixture repository creates a durable catalog generation");
        assert_eq!(
            indexed.operation, operation,
            "catalog preparation returns the requested operation"
        );
        prepared.push(PreparedRepository {
            root: repository,
            repository: indexed.repository,
            generation: indexed
                .published_generation
                .expect("structural fixture publishes a generation"),
            symbol,
        });
    }
    stop_daemon(input, &mut daemon);
    prepared
}

fn assert_mcp_reads_active_generation(
    state_dir: &Path,
    runtime_dir: &Path,
    repository: &PreparedRepository,
) {
    let mcp_binary = std::env::var_os("ROOTLIGHT_TEST_MCP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp")));
    let mut child = Command::new(mcp_binary)
        .env("ROOTLIGHT_STATE_DIR", state_dir)
        .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
        .env("ROOTLIGHT_MCP_PROFILE", "developer")
        .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("MCP process starts");
    let mut input = child.stdin.take().expect("MCP stdin is piped");
    let output = child.stdout.take().expect("MCP stdout is piped");
    let mut output = BufReader::new(output);
    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {
                    "name": "rootlight-startup-recovery",
                    "version": "1.0"
                },
                "initializationOptions": {
                    "rootlight_exposure_profile": "developer"
                }
            }
        })
    )
    .expect("initialize request writes");
    input.flush().expect("initialize request flushes");
    let initialized = read_json_line(&mut output, "initialize");
    assert_eq!(initialized["id"], "initialize");

    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        })
    )
    .expect("initialized notification writes");
    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": "recovered-read",
            "method": "tools/call",
            "params": {
                "name": "code.locate",
                "arguments": {
                    "repository": {
                        "repository_id": repository.repository.to_string()
                    },
                    "generation": "active",
                    "query": repository.symbol,
                    "search_modes": ["exact"],
                    "max_results": 8,
                    "response_profile": "compact"
                }
            }
        })
    )
    .expect("MCP read request writes");
    input.flush().expect("MCP read request flushes");
    let response = read_json_line(&mut output, "recovered read");
    assert_eq!(response["id"], "recovered-read");
    assert_ne!(
        response["result"]["isError"], true,
        "ready health must agree with MCP read admission: {response:#}"
    );
    let matches = response["result"]["structuredContent"]["data"]["matches"]
        .as_array()
        .expect("code.locate returns matches");
    assert!(
        matches
            .iter()
            .any(|candidate| candidate["display_name"] == repository.symbol),
        "MCP reads the recovered active generation: {response:#}"
    );

    drop(input);
    drop(output);
    finish_process(&mut child, "MCP");
}

fn read_json_line(output: &mut BufReader<impl std::io::Read>, label: &str) -> Value {
    let mut line = String::new();
    output
        .read_line(&mut line)
        .unwrap_or_else(|error| panic!("{label} response reads: {error}"));
    serde_json::from_str(&line).unwrap_or_else(|error| panic!("{label} response is JSON: {error}"))
}

fn startup_operation_id(index: usize, discriminator: u8) -> OperationId {
    let ordinal = u16::try_from(index).expect("fixture ordinal fits u16");
    let mut bytes = [0_u8; 16];
    bytes[0] = discriminator;
    bytes[14..].copy_from_slice(&ordinal.to_be_bytes());
    OperationId::from_bytes(bytes)
}

struct PreparedRepository {
    root: PathBuf,
    repository: RepositoryId,
    generation: GenerationId,
    symbol: String,
}

fn stop_daemon(input: ChildStdin, child: &mut Child) {
    drop(input);
    finish_process(child, "daemon");
}

fn daemon_binary() -> PathBuf {
    let mut binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    binary.set_file_name(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.is_file(),
        "release daemon binary is present beside MCP: {binary:?}"
    );
    binary
}

fn finish_process(child: &mut Child, process_name: &str) {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("process status reads") {
            if !status.success() {
                let mut diagnostic = String::new();
                if let Some(stderr) = child.stderr.take() {
                    let _read_result = stderr
                        .take(FAILURE_STDERR_LIMIT)
                        .read_to_string(&mut diagnostic);
                }
                panic!(
                    "{process_name} process exits successfully: status={status}, stderr={diagnostic}"
                );
            }
            return;
        }
        if Instant::now() >= deadline {
            child
                .kill()
                .unwrap_or_else(|error| panic!("timed-out {process_name} terminates: {error}"));
            let _status = child
                .wait()
                .unwrap_or_else(|error| panic!("terminated {process_name} reaps: {error}"));
            panic!("{process_name} process did not exit after stdin closed");
        }
        thread::sleep(EXIT_POLL_INTERVAL);
    }
}

fn daemon_recovery_targets() -> RecoveryTargets {
    let repositories =
        u64::try_from(STARTUP_REPOSITORIES).expect("fixture repository count fits u64");
    RecoveryTargets {
        p50: DAEMON_RECOVERY_P50_PER_REPOSITORY_US
            .checked_mul(repositories)
            .expect("p50 recovery budget fits u64"),
        p95: DAEMON_RECOVERY_P95_PER_REPOSITORY_US
            .checked_mul(repositories)
            .expect("p95 recovery budget fits u64"),
        p99: DAEMON_RECOVERY_P99_PER_REPOSITORY_US
            .checked_mul(repositories)
            .expect("p99 recovery budget fits u64"),
    }
}

struct RecoveryTargets {
    p50: u64,
    p95: u64,
    p99: u64,
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    sorted[rank]
}
