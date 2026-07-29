//! Production-process evidence for repository indexing and status lifecycle.

mod process_support;

use std::{
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const UNINDEXED_REPOSITORY: &str = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v";

#[test]
fn repository_generation_and_source_queries_survive_daemon_restart() {
    let fixture = process_support::private_process_tempdir("rl-restart-");
    let repository_root = fixture.path().join("repository");
    write_repository(&repository_root, 1);
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = build_default_daemon();

    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);
    let indexed = index_repository(&mut mcp, "restart-index", &repository_root);
    let repository_id = required_text(
        &indexed,
        &["result", "structuredContent", "data", "repository_id"],
    );
    let generation = required_text(
        &indexed,
        &[
            "result",
            "structuredContent",
            "data",
            "published_generation",
        ],
    );
    let operation_id = required_text(
        &indexed,
        &["result", "structuredContent", "data", "operation_id"],
    );
    mcp.finish();
    daemon.finish();

    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);

    let listed = mcp.call("restart-list", "repo.list", json!({}));
    assert_success(&listed, "repo.list");
    assert_eq!(
        listed["result"]["structuredContent"]["data"]["repositories"][0]["repository_id"],
        repository_id
    );
    assert_eq!(
        listed["result"]["structuredContent"]["data"]["repositories"][0]["active_generation"],
        generation
    );

    let status = mcp.call(
        "restart-status",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": generation,
            "include_operations": true
        }),
    );
    assert_success(&status, "repo.status");
    assert_eq!(
        status["result"]["structuredContent"]["data"]["resolved_generation"],
        generation
    );
    assert_eq!(
        status["result"]["structuredContent"]["data"]["operations"][0]["operation_id"],
        operation_id
    );
    let operation = mcp.call(
        "restart-operation",
        "operation.status",
        json!({"operation_id": operation_id}),
    );
    assert_success(&operation, "operation.status");
    assert_eq!(
        operation["result"]["structuredContent"]["data"]["published_generation"],
        generation
    );

    let located = mcp.call(
        "restart-locate",
        "code.locate",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": generation,
            "query": "answer",
            "search_modes": ["exact"],
            "response_profile": "evidence"
        }),
    );
    assert_success(&located, "code.locate");
    let source_ref =
        located["result"]["structuredContent"]["data"]["matches"][0]["source_ref"].clone();
    assert!(source_ref.is_object(), "restored match has source evidence");
    let source = mcp.call(
        "restart-source",
        "source.read",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": generation,
            "references": [{"source_ref": source_ref}],
            "encoding": "utf8_lossless_when_valid"
        }),
    );
    assert_success(&source, "source.read");
    assert!(
        source["result"]["structuredContent"]["data"]["chunks"][0]["content"]
            .as_str()
            .is_some_and(|content| content.contains("answer"))
    );

    write_repository(&repository_root, 2);
    let successor = index_repository(&mut mcp, "restart-successor", &repository_root);
    let successor_generation = required_text(
        &successor,
        &[
            "result",
            "structuredContent",
            "data",
            "published_generation",
        ],
    );
    assert_eq!(
        successor["result"]["structuredContent"]["data"]["repository_id"],
        repository_id
    );
    assert_eq!(
        successor["result"]["structuredContent"]["data"]["accepted_plan"]["parent_generation"],
        generation
    );
    assert_ne!(successor_generation, generation);

    mcp.finish();
    daemon.finish();

    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);
    let successor_status = mcp.call(
        "successor-restart-status",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "include_operations": true
        }),
    );
    assert_success(&successor_status, "repo.status");
    assert_eq!(
        successor_status["result"]["structuredContent"]["data"]["resolved_generation"],
        successor_generation
    );
    assert_eq!(
        successor_status["result"]["structuredContent"]["generation"]["parent_generation"],
        generation
    );
    assert_eq!(
        successor_status["result"]["structuredContent"]["data"]["operations"]
            .as_array()
            .map(Vec::len),
        Some(2)
    );

    mcp.finish();
    daemon.finish();
}

#[test]
fn repository_lifecycle_is_generation_exact_and_preflights_unsupported_controls() {
    let fixture = process_support::private_process_tempdir("rl-repo-");
    let repository_root = fixture.path().join("repository");
    write_repository(&repository_root, 1);

    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = build_default_daemon();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);

    let first = index_repository(&mut mcp, "index-first", &repository_root);
    let repository_id = required_text(
        &first,
        &["result", "structuredContent", "data", "repository_id"],
    );
    let first_operation = required_text(
        &first,
        &["result", "structuredContent", "data", "operation_id"],
    );
    let first_generation = required_text(
        &first,
        &[
            "result",
            "structuredContent",
            "data",
            "published_generation",
        ],
    );
    assert_eq!(
        first["result"]["structuredContent"]["data"]["state"],
        "published"
    );

    write_repository(&repository_root, 2);
    let second = index_repository(&mut mcp, "index-second", &repository_root);
    let second_operation = required_text(
        &second,
        &["result", "structuredContent", "data", "operation_id"],
    );
    let second_generation = required_text(
        &second,
        &[
            "result",
            "structuredContent",
            "data",
            "published_generation",
        ],
    );
    assert_eq!(
        second["result"]["structuredContent"]["data"]["state"],
        "published"
    );
    assert_ne!(first_operation, second_operation);
    assert_ne!(first_generation, second_generation);
    assert_eq!(
        second["result"]["structuredContent"]["data"]["accepted_plan"]["parent_generation"],
        first_generation
    );

    let retained = mcp.call(
        "status-retained",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": first_generation,
            "coverage_detail": "language",
            "include_operations": true,
            "require_freshness": "none",
            "response_profile": "compact"
        }),
    );
    assert_success(&retained, "repo.status");
    let structured = &retained["result"]["structuredContent"];
    assert_eq!(structured["generation"]["generation_id"], first_generation);
    assert_eq!(
        structured["generation"]["structural_freshness"],
        "superseded"
    );
    assert_eq!(structured["data"]["requested_generation"], first_generation);
    assert_eq!(structured["data"]["resolved_generation"], first_generation);
    assert_eq!(
        structured["data"]["active_generation"]["generation_id"],
        second_generation
    );
    assert_eq!(structured["data"]["publication_state"], "retained");
    assert_eq!(structured["data"]["repository_state"], "ready");
    assert_eq!(
        structured["data"]["coverage"]["languages"][0]["language"],
        "rust"
    );
    assert_eq!(
        structured["data"]["recommended_actions"],
        json!(["index repository"])
    );
    assert_eq!(structured["warnings"][0]["code"], "stale_generation");
    assert!(
        structured["usage"]["json_bytes"]
            .as_u64()
            .is_some_and(|bytes| bytes > 0)
    );
    assert!(
        structured["usage"]["estimated_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(structured["usage"]["wall_time_ms"].is_u64());

    let operations = structured["data"]["operations"]
        .as_array()
        .expect("repo.status returns requested operation details");
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0]["operation_id"], second_operation);
    assert_eq!(operations[0]["state"], "published");
    assert_eq!(operations[1]["operation_id"], first_operation);
    assert_eq!(operations[1]["state"], "published");

    let stale = mcp.call(
        "status-stale",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": first_generation,
            "require_freshness": "structural"
        }),
    );
    assert_public_error(&stale, "STALE_GENERATION");
    let stale_error = &stale["result"]["structuredContent"]["error"];
    assert_eq!(stale_error["generation"], first_generation);
    assert_eq!(
        stale_error["next_actions"],
        json!([{"action": "rebuild_repository"}])
    );

    // These controls are capability-preflight rejections. Stopping the daemon
    // first proves neither request attempts the client port or transport.
    daemon.finish();
    let unsupported_profile = mcp.call(
        "status-standard",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "response_profile": "standard"
        }),
    );
    assert_public_error(&unsupported_profile, "UNSUPPORTED_CAPABILITY");
    let unsupported_budget = mcp.call(
        "status-budget",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "budget": {"max_tokens": 500}
        }),
    );
    assert_public_error(&unsupported_budget, "UNSUPPORTED_CAPABILITY");

    mcp.finish();
}

#[test]
fn repository_status_distinguishes_empty_missing_failed_and_unavailable_results() {
    let fixture = process_support::private_process_tempdir("rl-repo-");
    let repository_root = fixture.path().join("empty-repository");
    fs::create_dir_all(repository_root.join("src"))
        .expect("empty repository source directory is created");
    fs::write(
        repository_root.join("Cargo.toml"),
        "[package]\nname = \"empty_repository_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("empty repository manifest is written");
    fs::write(repository_root.join("src").join("lib.rs"), [])
        .expect("empty repository source is written");

    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = build_default_daemon();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);

    let indexed = index_repository(&mut mcp, "index-empty", &repository_root);
    let repository_id = required_text(
        &indexed,
        &["result", "structuredContent", "data", "repository_id"],
    );
    assert_ne!(repository_id, UNINDEXED_REPOSITORY);

    let empty = mcp.call(
        "status-empty",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "coverage_detail": "language",
            "include_operations": true
        }),
    );
    assert_success(&empty, "repo.status");
    let empty_status = &empty["result"]["structuredContent"];
    assert_eq!(empty_status["data"]["repository_state"], "ready");
    assert_eq!(
        empty_status["data"]["coverage"]["languages"][0]["language"],
        "rust"
    );
    assert_eq!(
        empty_status["data"]["coverage"]["languages"][0]["files_indexed"],
        1
    );
    assert_eq!(
        empty_status["generation"]["structural_freshness"],
        "current"
    );
    assert_eq!(empty_status["generation"]["semantic_freshness"], "stale");
    assert_eq!(
        empty_status["data"]["recommended_actions"],
        json!(["index repository"])
    );
    assert_eq!(empty_status["warnings"][0]["code"], "stale_generation");

    let missing = mcp.call(
        "status-not-indexed",
        "repo.status",
        json!({
            "repository": {"repository_id": UNINDEXED_REPOSITORY}
        }),
    );
    assert_public_error(&missing, "NOT_FOUND");
    assert_eq!(
        missing["result"]["structuredContent"]["error"]["message"],
        "repository was not found"
    );

    fs::remove_file(repository_root.join("src").join("lib.rs"))
        .expect("indexed source is removed before the failed update");
    let failed_index = mcp.call(
        "index-failed",
        "repo.index",
        json!({
            "root": repository_root,
            "mode": "structural",
            "detached": false
        }),
    );
    assert_public_error(&failed_index, "UNSUPPORTED_CAPABILITY");

    let failed = mcp.call(
        "status-failed",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "include_operations": true
        }),
    );
    assert_success(&failed, "repo.status");
    let failed_status = &failed["result"]["structuredContent"];
    assert_eq!(failed_status["data"]["repository_state"], "ready");
    assert_eq!(failed_status["data"]["operations"][0]["state"], "failed");
    assert_eq!(
        failed_status["data"]["recommended_actions"],
        json!(["index repository", "inspect operation"])
    );
    assert_eq!(failed_status["warnings"][0]["code"], "stale_generation");

    mcp.finish();
    daemon.finish();

    let unavailable_state = fixture.path().join("unavailable-state");
    let unavailable_runtime = fixture.path().join("unavailable-runtime");
    let mut unavailable =
        McpProcess::spawn_transport_only(&unavailable_state, &unavailable_runtime);
    let response = unavailable.call(
        "status-unavailable",
        "repo.status",
        json!({
            "repository": {"repository_id": UNINDEXED_REPOSITORY}
        }),
    );
    assert_eq!(response["error"]["code"], -32_603);
    assert_eq!(response["error"]["message"], "tool transport failed");
    unavailable.finish();
}

fn write_repository(root: &Path, value: u32) {
    fs::create_dir_all(root.join("src")).expect("fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"repository_lifecycle_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        root.join("src").join("lib.rs"),
        format!("pub fn answer() -> u32 {{ {value} }}\n"),
    )
    .expect("fixture source is written");
}

fn index_repository(mcp: &mut McpProcess, id: &str, root: &Path) -> Value {
    let arguments = json!({
        "root": root,
        "mode": "structural",
        "detached": false
    });
    let response = process_support::retry_transient_busy(id, |attempt_id| {
        mcp.call(attempt_id, "repo.index", arguments.clone())
    });
    assert_success(&response, "repo.index");
    response
}

fn required_text(response: &Value, path: &[&str]) -> String {
    path.iter()
        .fold(response, |value, segment| &value[*segment])
        .as_str()
        .unwrap_or_else(|| panic!("response is missing string at {path:?}: {response:#}"))
        .to_owned()
}

fn assert_success(response: &Value, tool: &str) {
    assert_ne!(
        response["result"]["isError"], true,
        "{tool} returned a public error: {response:#}"
    );
    assert!(
        response["result"]["structuredContent"].is_object(),
        "{tool} did not return structured content: {response:#}"
    );
}

fn assert_public_error(response: &Value, expected: &str) {
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"], expected,
        "unexpected process error: {response:#}"
    );
}

fn build_default_daemon() -> PathBuf {
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let profile_dir = mcp_binary
        .parent()
        .expect("MCP binary has a profile directory");
    let target_dir = profile_dir
        .parent()
        .expect("profile directory belongs to a Cargo target directory");
    let daemon = profile_dir.join(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX));
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(&workspace)
        .args([
            OsStr::new("build"),
            OsStr::new("--locked"),
            OsStr::new("-p"),
            OsStr::new("rootlight-daemon"),
            OsStr::new("--bin"),
            OsStr::new("rootlight-daemon"),
            OsStr::new("--no-default-features"),
            OsStr::new("--target-dir"),
        ])
        .arg(target_dir)
        .output()
        .expect("default daemon build starts");
    assert!(
        output.status.success(),
        "default daemon build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.is_file(), "daemon build did not produce {daemon:?}");
    daemon
}

struct DaemonProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
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
        Self {
            child: Some(child),
            input: Some(input),
        }
    }

    fn wait_until_ready(&mut self, runtime_dir: &Path) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let discovery = runtime_dir.join("daemon.json");
        while Instant::now() < deadline {
            if discovery.is_file() {
                return;
            }
            if self
                .child
                .as_mut()
                .expect("daemon child is retained")
                .try_wait()
                .expect("daemon status is readable")
                .is_some()
            {
                let mut stderr = String::new();
                self.child
                    .as_mut()
                    .expect("daemon child is retained")
                    .stderr
                    .take()
                    .expect("daemon stderr is piped")
                    .read_to_string(&mut stderr)
                    .expect("daemon stderr is readable");
                panic!("daemon exited before publishing discovery: {stderr}");
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon did not publish discovery within the startup bound");
    }

    fn finish(&mut self) {
        self.input.take();
        let status = wait_for_exit(
            self.child.as_mut().expect("daemon child is retained"),
            SHUTDOWN_TIMEOUT,
        );
        assert!(status.success(), "daemon process exits successfully");
        self.child.take();
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
    }
}

struct McpProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    responses: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl McpProcess {
    fn spawn(state_dir: &Path, runtime_dir: &Path) -> Self {
        Self::spawn_with_mode(state_dir, runtime_dir, false)
    }

    fn spawn_transport_only(state_dir: &Path, runtime_dir: &Path) -> Self {
        Self::spawn_with_mode(state_dir, runtime_dir, true)
    }

    fn spawn_with_mode(state_dir: &Path, runtime_dir: &Path, transport_only: bool) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"));
        if transport_only {
            command.arg("--transport-only");
        }
        let mut child = command
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env("ROOTLIGHT_MCP_PROFILE", "developer")
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP fixture process starts");
        let output = child.stdout.take().expect("MCP stdout is piped");
        let (responses_tx, responses) = mpsc::sync_channel(16);
        let reader = thread::spawn(move || {
            let output = std::io::BufReader::new(output);
            for line in std::io::BufRead::lines(output) {
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
                "clientInfo": {"name": "repository-lifecycle-process", "version": "1.0"},
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

    fn call(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }));
        let response = self.read();
        assert_eq!(response["id"], id);
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
            .expect("MCP reader thread joins");
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
        thread::sleep(Duration::from_millis(25));
    }
    child.kill().expect("timed-out child is terminated");
    child.wait().expect("terminated child is reaped")
}

fn terminate(child: &mut Option<Child>) {
    if let Some(mut child) = child.take() {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}
