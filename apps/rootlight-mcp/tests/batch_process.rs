//! Production-process evidence for the public `query.batch` adapter registry.

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_ids::{RepositoryId, SymbolId};
use rootlight_mcp_contract::{batch::BATCH_TOOL_REGISTRY, context::BatchTool};
use serde_json::{Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn every_advertised_batch_subtool_reaches_its_production_adapter() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    let repository_root = fixture.path().join("repository");
    fs::create_dir_all(repository_root.join("src")).expect("fixture source directory is created");
    fs::write(
        repository_root.join("Cargo.toml"),
        "[package]\nname = \"batch_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        repository_root.join("src").join("lib.rs"),
        "pub fn batch_process_fixture() -> usize { 12 }\n",
    )
    .expect("fixture source is written");

    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = ensure_daemon_binary();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(false, &state_dir, &runtime_dir, "developer");

    let index = mcp.call(
        "index",
        "repo.index",
        json!({
            "root": repository_root,
            "mode": "auto",
            "detached": false
        }),
    );
    assert_success(&index, "repo.index");
    let repository_id = index["result"]["structuredContent"]["data"]["repository_id"]
        .as_str()
        .expect("repo.index returns a repository identity")
        .to_owned();
    let operation_id = index["result"]["structuredContent"]["data"]["operation_id"]
        .as_str()
        .expect("repo.index returns an operation identity")
        .to_owned();
    wait_for_publication(&mut mcp, &index, &operation_id);

    let locate = mcp.call(
        "locate-fixture",
        "code.locate",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "query": "batch_process_fixture",
            "search_modes": ["exact"]
        }),
    );
    assert_success(&locate, "code.locate");
    let matched = &locate["result"]["structuredContent"]["data"]["matches"][0];
    let symbol = matched["symbol_id"].clone();
    assert!(
        symbol.is_string(),
        "fixture locate returns a symbol identity"
    );
    let source_ref = matched["source_ref"].clone();
    assert!(
        source_ref.is_object(),
        "fixture locate returns a generation-pinned source reference"
    );
    assert_eq!(BATCH_TOOL_REGISTRY.len(), BatchTool::ALL.len());
    let mut failures = Vec::new();
    for (index, descriptor) in BATCH_TOOL_REGISTRY.iter().enumerate() {
        let response = mcp.call(
            &format!("batch-adapter-{index}"),
            "query.batch",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": "active",
                "operations": [{
                    "id": format!("adapter_{index}"),
                    "tool": descriptor.batch_tool.name(),
                    "arguments": dispatch_arguments(
                        descriptor.batch_tool,
                        &symbol,
                        &source_ref
                    )
                }],
                "failure_policy": "continue_independent"
            }),
        );
        if response["result"]["isError"] == true {
            failures.push(format!(
                "{}: top-level {}",
                descriptor.batch_tool.name(),
                response["result"]["structuredContent"]["error"]["code"]
            ));
            continue;
        }
        let results = response["result"]["structuredContent"]["data"]["operation_results"]
            .as_array()
            .expect("query.batch returns operation results");
        assert_eq!(results.len(), 1);
        let result = &results[0];
        assert_eq!(result["id"], format!("adapter_{index}"));
        assert_eq!(result["tool"], descriptor.batch_tool.name());
        if result["status"] == "ok" {
            assert!(result["data"].is_object());
        } else if matches!(
            descriptor.batch_tool,
            BatchTool::SymbolExplain | BatchTool::ContextPack
        ) && result["error"]["code"] == "NOT_FOUND"
        {
            assert!(
                result["error"]["details"].is_object(),
                "stable domain errors retain structured details"
            );
        } else {
            failures.push(format!(
                "{}: operation {} ({result})",
                descriptor.batch_tool.name(),
                result["error"]["code"]
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "production batch adapters did not complete successfully: {failures:#?}"
    );

    mcp.finish();
    daemon.finish();
}

#[test]
fn process_preflight_rejects_non_subtools_and_profile_hidden_members() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    let mut developer = McpProcess::spawn(
        true,
        &fixture.path().join("state-developer"),
        &fixture.path().join("runtime-developer"),
        "developer",
    );
    for forbidden in ["query.batch", "repo.index", "operation.status"] {
        let response = developer.call(
            &format!("forbidden-{forbidden}"),
            "query.batch",
            json!({
                "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
                "operations": [{
                    "id": "forbidden",
                    "tool": forbidden,
                    "arguments": {}
                }]
            }),
        );
        assert_public_error(&response, "INVALID_ARGUMENT");
    }
    developer.finish();

    let mut scout = McpProcess::spawn(
        true,
        &fixture.path().join("state-scout"),
        &fixture.path().join("runtime-scout"),
        "scout",
    );
    let hidden = scout.call(
        "profile-hidden",
        "query.batch",
        json!({
            "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
            "operations": [{
                "id": "hidden",
                "tool": "plan.change",
                "arguments": {
                    "objective": "bug_fix",
                    "objective_text": "fixture",
                    "targets": [{"symbol_id": SymbolId::from_bytes([7; 20])}]
                }
            }]
        }),
    );
    assert_public_error(&hidden, "UNSUPPORTED_CAPABILITY");
    scout.finish();
}

fn dispatch_arguments(tool: BatchTool, symbol: &Value, source_ref: &Value) -> Value {
    match tool {
        BatchTool::CodeLocate => {
            json!({"query": "__batch_absent__", "search_modes": ["exact"]})
        }
        BatchTool::SymbolExplain => json!({"symbol_ids": [symbol.clone()]}),
        BatchTool::SymbolRelationships => {
            json!({"symbol_ids": [symbol.clone()], "relations": ["calls"]})
        }
        BatchTool::FlowTrace => {
            json!({"from": {"symbol_id": symbol.clone()}, "relations": ["calls"]})
        }
        BatchTool::ChangeImpact => json!({"change": {"symbol_ids": [symbol.clone()]}}),
        BatchTool::TestsSelect => json!({"seeds": {"symbols": [symbol.clone()]}}),
        BatchTool::ArchitectureOverview => json!({}),
        BatchTool::ArchitectureCycles => {
            json!({"projection": {"relations": ["calls"], "level": "symbol"}})
        }
        BatchTool::CodeDead => json!({}),
        BatchTool::PlanChange => {
            json!({
                "objective": "bug_fix",
                "objective_text": "fixture",
                "targets": [{"symbol_id": symbol.clone()}]
            })
        }
        BatchTool::ContextPack => {
            json!({
                "task": "fixture",
                "seeds": {"symbols": [symbol.clone()]},
                "token_budget": 4_500
            })
        }
        BatchTool::SourceRead => {
            json!({"references": [{"source_ref": source_ref.clone()}]})
        }
    }
}

fn wait_for_publication(mcp: &mut McpProcess, index: &Value, operation_id: &str) {
    let initial_state = index["result"]["structuredContent"]["data"]["state"].as_str();
    if initial_state == Some("published") {
        return;
    }
    for attempt in 0..30 {
        let status = mcp.call(
            &format!("operation-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 1_000}),
        );
        assert_success(&status, "operation.status");
        let data = &status["result"]["structuredContent"]["data"];
        match data["operation"]["state"].as_str() {
            Some("published") => return,
            Some("failed" | "cancelled") => {
                panic!("fixture indexing terminated without publication: {status:#}")
            }
            _ => {}
        }
    }
    panic!("fixture indexing did not publish within the bounded wait");
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

fn ensure_daemon_binary() -> PathBuf {
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let profile_dir = mcp_binary
        .parent()
        .expect("MCP binary has a profile directory");
    let daemon = profile_dir.join(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX));
    if daemon.is_file() {
        return daemon;
    }

    let target_dir = profile_dir
        .parent()
        .expect("profile directory belongs to a Cargo target directory");
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
            OsStr::new("--target-dir"),
        ])
        .arg(target_dir)
        .output()
        .expect("test-only daemon build starts");
    assert!(
        output.status.success(),
        "test-only daemon build failed: {}",
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
            .stderr(Stdio::null())
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
                panic!("daemon exited before publishing discovery");
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
    output: Option<BufReader<ChildStdout>>,
}

impl McpProcess {
    fn spawn(transport_only: bool, state_dir: &Path, runtime_dir: &Path, profile: &str) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"));
        if transport_only {
            command.arg("--transport-only");
        }
        let mut child = command
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env("ROOTLIGHT_MCP_PROFILE", profile)
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", profile)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP fixture process starts");
        let mut process = Self {
            input: child.stdin.take(),
            output: child.stdout.take().map(BufReader::new),
            child: Some(child),
        };
        process.write(&json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "batch-process", "version": "1.0"},
                "initializationOptions": {"rootlight_exposure_profile": profile}
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

    fn read(&mut self) -> Value {
        let mut line = String::new();
        self.output
            .as_mut()
            .expect("MCP stdout is retained")
            .read_line(&mut line)
            .expect("MCP response reads");
        serde_json::from_str(&line).expect("MCP response is valid JSON")
    }

    fn finish(&mut self) {
        self.input.take();
        self.output.take();
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
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.input.take();
        self.output.take();
        terminate(&mut self.child);
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
