//! Cross-process evidence for MCP cancellation of active daemon analysis.

use std::{
    ffi::OsStr,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_ipc::{Endpoint, LocalListener, LocalStream};
use serde_json::{Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const CANCELLATION_TIMEOUT: Duration = Duration::from_secs(10);
const FOLLOW_UP_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const LATE_RESPONSE_WINDOW: Duration = Duration::from_millis(250);
const CYCLES_REQUEST_ID: &str = "cancel-active-cycles";
const CYCLES_FOLLOW_UP_ID: &str = "status-after-cycles-cancel";
const ADVANCED_REQUEST_ID: &str = "cancel-active-advanced-query";
const ADVANCED_FOLLOW_UP_ID: &str = "status-after-advanced-cancel";

#[test]
fn cancellation_reaches_active_daemon_analyses_without_emitting_responses() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    let repository_root = fixture.path().join("repository");
    write_large_repository(&repository_root);

    let hook_endpoint = cancellation_hook_endpoint(fixture.path());
    let listener =
        LocalListener::bind(hook_endpoint.clone()).expect("cancellation hook listener binds");
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = build_hook_daemon();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir, &hook_endpoint);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);

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

    assert_cancelled_request(
        &listener,
        &mut daemon,
        &mut mcp,
        CYCLES_REQUEST_ID,
        CYCLES_FOLLOW_UP_ID,
        "architecture.cycles",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "projection": {"relations": ["calls"], "level": "symbol"},
            "max_cycles": 200,
            "budget": {
                "max_results": 200,
                "max_traversal_facts": 100000,
                "timeout_ms": 30000
            }
        }),
    );
    assert_cancelled_request(
        &listener,
        &mut daemon,
        &mut mcp,
        ADVANCED_REQUEST_ID,
        ADVANCED_FOLLOW_UP_ID,
        "query.advanced",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "query": {
                "op": "scan",
                "entity": "function"
            },
            "max_results": 1000,
            "max_depth": 5,
            "cost_limit": 10000000
        }),
    );

    mcp.finish();
    daemon.finish();
}

fn assert_cancelled_request(
    listener: &LocalListener,
    daemon: &mut DaemonProcess,
    mcp: &mut McpProcess,
    request_id: &str,
    follow_up_id: &str,
    tool: &str,
    arguments: Value,
) {
    let repository_id = arguments["repository"]["repository_id"]
        .as_str()
        .expect("cancelled analytical request has a repository selector")
        .to_owned();
    mcp.write(&tool_call(request_id, tool, arguments));
    let mut hook = match listener.accept_timeout(CANCELLATION_TIMEOUT) {
        Ok(hook) => hook,
        Err(error) => {
            let early_response = mcp.responses.try_recv().ok();
            let daemon_status = daemon
                .child
                .as_mut()
                .expect("daemon child is retained")
                .try_wait()
                .expect("daemon status is readable");
            panic!(
                "{tool} did not reach the cancellation hook: \
                 {error}; early MCP response: {early_response:?}; daemon status: {daemon_status:?}"
            );
        }
    };
    assert_eq!(
        read_hook_signal(&mut hook, CANCELLATION_TIMEOUT),
        b'E',
        "hook reports entry only after generation resolution in the daemon worker"
    );

    let cancelled_at = Instant::now();
    mcp.write(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {
            "requestId": request_id,
            "reason": "process cancellation evidence"
        }
    }));
    assert_eq!(
        read_hook_signal(&mut hook, CANCELLATION_TIMEOUT),
        b'C',
        "daemon worker observes client-request cancellation"
    );
    let cancellation_latency = cancelled_at.elapsed();
    assert!(
        cancellation_latency <= CANCELLATION_TIMEOUT,
        "daemon cancellation must be observed within the cancellation bound"
    );

    let follow_up_started = Instant::now();
    mcp.write(&tool_call(
        follow_up_id,
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active"
        }),
    ));
    let follow_up = mcp.read(FOLLOW_UP_TIMEOUT);
    assert_ne!(
        follow_up["id"], request_id,
        "cancelled MCP requests must not emit a JSON-RPC response"
    );
    assert_eq!(follow_up["id"], follow_up_id);
    assert_success(&follow_up, "repo.status");
    let follow_up_latency = follow_up_started.elapsed();
    assert!(
        follow_up_latency <= FOLLOW_UP_TIMEOUT,
        "the analytical lane must be reusable within the follow-up bound"
    );
    mcp.assert_no_response_for(request_id, LATE_RESPONSE_WINDOW);
    eprintln!(
        "cancellation_measurement tool={tool} cancellation_latency_micros={} \
         follow_up_latency_micros={} late_response_window_millis={}",
        cancellation_latency.as_micros(),
        follow_up_latency.as_micros(),
        LATE_RESPONSE_WINDOW.as_millis()
    );
}

fn write_large_repository(root: &Path) {
    const FUNCTION_COUNT: usize = 1_024;

    fs::create_dir_all(root.join("src")).expect("fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"cancellation_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    let mut source = String::with_capacity(FUNCTION_COUNT * 64);
    for index in 0..FUNCTION_COUNT {
        let next = (index + 1) % FUNCTION_COUNT;
        source.push_str(&format!(
            "pub fn node_{index:04}() -> usize {{ node_{next:04}() + {index} }}\n"
        ));
    }
    fs::write(root.join("src").join("lib.rs"), source).expect("large fixture source is written");
}

fn wait_for_publication(mcp: &mut McpProcess, index: &Value, operation_id: &str) {
    if index["result"]["structuredContent"]["data"]["state"] == "published" {
        return;
    }
    for attempt in 0..30 {
        let status = mcp.call(
            &format!("operation-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 1_000}),
        );
        assert_success(&status, "operation.status");
        match status["result"]["structuredContent"]["data"]["operation"]["state"].as_str() {
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

fn tool_call(id: &str, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    })
}

fn build_hook_daemon() -> PathBuf {
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
            OsStr::new("--features"),
            OsStr::new("process-test-hooks"),
            OsStr::new("--target-dir"),
        ])
        .arg(target_dir)
        .output()
        .expect("hook-enabled daemon build starts");
    assert!(
        output.status.success(),
        "hook-enabled daemon build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.is_file(), "daemon build did not produce {daemon:?}");
    daemon
}

#[cfg(windows)]
fn cancellation_hook_endpoint(_fixture: &Path) -> Endpoint {
    Endpoint::new(PathBuf::from(format!(
        r"\\.\pipe\rootlight-cancellation-process-{}",
        std::process::id()
    )))
    .expect("Windows cancellation hook endpoint is valid")
}

#[cfg(unix)]
fn cancellation_hook_endpoint(fixture: &Path) -> Endpoint {
    Endpoint::new(fixture.join("cancellation.sock"))
        .expect("Unix cancellation hook endpoint is valid")
}

fn read_hook_signal(stream: &mut LocalStream, timeout: Duration) -> u8 {
    let deadline = Instant::now()
        .checked_add(timeout)
        .expect("hook deadline is representable");
    let mut signal = [0_u8; 1];
    loop {
        match stream.read(&mut signal) {
            Ok(1) => return signal[0],
            Ok(0) if Instant::now() < deadline => thread::yield_now(),
            Ok(0) => panic!("cancellation hook signal timed out"),
            Ok(_) => unreachable!("one-byte hook buffer cannot read more than one byte"),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) && Instant::now() < deadline =>
            {
                thread::yield_now();
            }
            Err(error) => panic!("cancellation hook signal failed: {error}"),
        }
    }
}

struct DaemonProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
}

impl DaemonProcess {
    fn spawn(
        binary: &Path,
        state_dir: &Path,
        runtime_dir: &Path,
        hook_endpoint: &Endpoint,
    ) -> Self {
        let mut child = Command::new(binary)
            .arg("--supervised-stdio")
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env(
                "ROOTLIGHT_PROCESS_TEST_CANCELLATION_ENDPOINT",
                hook_endpoint.as_path(),
            )
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
    responses: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl McpProcess {
    fn spawn(state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
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
                "clientInfo": {"name": "cancellation-process", "version": "1.0"},
                "initializationOptions": {"rootlight_exposure_profile": "developer"}
            }
        }));
        assert_eq!(process.read(RESPONSE_TIMEOUT)["id"], "initialize");
        process.write(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));
        process
    }

    fn call(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.write(&tool_call(id, tool, arguments));
        let response = self.read(RESPONSE_TIMEOUT);
        assert_eq!(response["id"], id);
        response
    }

    fn write(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("MCP stdin is retained");
        serde_json::to_writer(&mut *input, message).expect("MCP request serializes");
        input.write_all(b"\n").expect("MCP request terminates");
        input.flush().expect("MCP request flushes");
    }

    fn read(&self, timeout: Duration) -> Value {
        let line = self
            .responses
            .recv_timeout(timeout)
            .expect("MCP response arrives within the bound");
        serde_json::from_str(&line).expect("MCP response is valid JSON")
    }

    fn assert_no_response_for(&self, request_id: &str, window: Duration) {
        match self.responses.recv_timeout(window) {
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                panic!("MCP stdout closed before the late-response window elapsed")
            }
            Ok(line) => {
                let response: Value =
                    serde_json::from_str(&line).expect("MCP response is valid JSON");
                assert_ne!(
                    response["id"], request_id,
                    "cancelled MCP request emitted a late JSON-RPC response"
                );
                panic!("unexpected extra MCP response: {response:#}");
            }
        }
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
