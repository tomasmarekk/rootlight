//! Release-process regression coverage for the MCP bridge startup SLO.

mod process_support;

use std::{
    fs,
    io::{BufRead as _, BufReader, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_client::{Client, ConnectPolicy};
use rootlight_ids::OperationId;
use rootlight_runtime::RuntimePaths;
use serde_json::Value;

const WARMUP_SAMPLES: usize = 5;
const MEASURED_SAMPLES: usize = 100;
const P50_TARGET_US: u64 = 80_000;
const P95_TARGET_US: u64 = 150_000;
const P99_TARGET_US: u64 = 300_000;
const DAEMON_P50_TARGET_US: u64 = 500_000;
const DAEMON_P95_TARGET_US: u64 = 1_500_000;
const DAEMON_P99_TARGET_US: u64 = 3_000_000;
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(10);
const EXIT_TIMEOUT: Duration = Duration::from_secs(2);
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

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
#[ignore = "runs 105 fresh release daemon processes and enforces startup percentiles"]
fn daemon_reaches_ready_within_release_startup_slo() {
    let isolated = process_support::private_process_tempdir("rl-startup-daemon-");
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");
    let repository = isolated.path().join("repository");
    prepare_valid_catalog(&state_dir, &runtime_dir, &repository);

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
    assert!(
        p50 <= DAEMON_P50_TARGET_US && p95 <= DAEMON_P95_TARGET_US && p99 <= DAEMON_P99_TARGET_US,
        "daemon startup exceeded release SLO: p50={p50}us/{DAEMON_P50_TARGET_US}us, \
         p95={p95}us/{DAEMON_P95_TARGET_US}us, \
         p99={p99}us/{DAEMON_P99_TARGET_US}us, \
         min={}us, max={}us",
        samples[0],
        samples[samples.len() - 1],
    );
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
    finish_process(&mut child);
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
        .stderr(Stdio::null())
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

fn prepare_valid_catalog(state_dir: &Path, runtime_dir: &Path, repository: &Path) {
    fs::create_dir_all(repository.join("src")).expect("fixture source directory is created");
    fs::write(
        repository.join("Cargo.toml"),
        "[package]\nname = \"startup_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        repository.join("src").join("lib.rs"),
        "pub fn startup_fixture() -> bool { true }\n",
    )
    .expect("fixture source is written");

    let (mut daemon, input, _startup, control) = start_healthy_daemon(state_dir, runtime_dir);
    let indexed = control
        .repository_index(
            &repository.to_string_lossy(),
            OperationId::from_bytes([0x52; 16]),
            false,
        )
        .expect("fixture repository creates a durable catalog generation");
    assert_eq!(
        indexed.operation,
        OperationId::from_bytes([0x52; 16]),
        "catalog preparation returns the requested operation"
    );
    stop_daemon(input, &mut daemon);
}

fn stop_daemon(input: ChildStdin, child: &mut Child) {
    drop(input);
    finish_process(child);
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

fn finish_process(child: &mut Child) {
    let deadline = Instant::now() + EXIT_TIMEOUT;
    loop {
        if let Some(status) = child.try_wait().expect("MCP process status reads") {
            assert!(status.success(), "MCP process exits successfully");
            return;
        }
        if Instant::now() >= deadline {
            child.kill().expect("timed-out MCP process terminates");
            let _status = child.wait().expect("terminated MCP process reaps");
            panic!("MCP process did not exit after stdin closed");
        }
        thread::sleep(EXIT_POLL_INTERVAL);
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let rank = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    sorted[rank]
}
