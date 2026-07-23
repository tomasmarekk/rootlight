//! Context-pack objective and role-coverage evidence across real processes.
//!
//! The fixture crosses MCP stdio and the supervised daemon boundary so profile,
//! role, identity, and budget assertions cover the deployed composition.

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const FULL_TOKEN_BUDGET: u64 = 4_500;

const CONTEXT_SOURCE: &str = r#"
pub fn context_entry(value: usize) -> usize {
    context_alpha(value)
        + context_beta(value)
        + context_gamma(value)
        + context_delta(value)
        + context_epsilon(value)
        + context_zeta(value)
}

pub fn context_alpha(
    value: usize,
) -> usize {
    value.saturating_add(1)
}

pub fn context_beta(
    value: usize,
) -> usize {
    value.saturating_add(2)
}

pub fn context_gamma(
    value: usize,
) -> usize {
    value.saturating_add(3)
}

pub fn context_delta(
    value: usize,
) -> usize {
    value.saturating_add(4)
}

pub fn context_epsilon(
    value: usize,
) -> usize {
    value.saturating_add(5)
}

pub fn context_zeta(
    value: usize,
) -> usize {
    value.saturating_add(6)
}

#[cfg(test)]
mod tests {
    use super::context_entry;

    #[test]
    fn entry_combines_all_helpers() {
        assert_eq!(context_entry(1), 27);
    }
}
"#;

#[test]
fn context_contract_crosses_real_process_boundaries() {
    let mut fixture = ContextFixture::spawn();
    objective_role_truth_is_profile_invariant(&mut fixture);
    fixture.finish();
}

fn objective_role_truth_is_profile_invariant(fixture: &mut ContextFixture) {
    let cases = [
        ("fix the context entry bug", "bug_fix"),
        ("refactor the context entry implementation", "refactor"),
        ("explain the context entry behavior", "explanation"),
        ("migrate the context entry API", "migration"),
        ("review the context entry security", "review"),
    ];

    for (case_index, (task, objective)) in cases.into_iter().enumerate() {
        let mut expected_semantics = None;
        for profile in ["compact", "standard", "evidence"] {
            let response = fixture.context_call(
                &format!("objective-{case_index}-{profile}"),
                task,
                FULL_TOKEN_BUDGET,
                profile,
            );
            assert_success(&response, "context.pack");
            let output = structured(&response);
            assert_context_identity(output, fixture);
            let coverage = &output["data"]["role_coverage"];
            assert_eq!(coverage["objective"], objective);
            assert_eq!(coverage["objective_rule_version"], 1);
            assert_role_coverage_is_truthful(coverage, &output["completeness"]);
            assert_accounting_is_bounded(output, FULL_TOKEN_BUDGET);

            let semantics = json!({
                "role_coverage": coverage,
                "state": output["completeness"]["state"],
                "limiting_resources": output["completeness"]["limiting_resources"],
            });
            if let Some(expected) = &expected_semantics {
                assert_eq!(
                    &semantics, expected,
                    "response profile changed objective-role truth"
                );
            } else {
                expected_semantics = Some(semantics);
            }
        }
    }
}

fn assert_role_coverage_is_truthful(coverage: &Value, completeness: &Value) {
    let roles = coverage["roles"]
        .as_array()
        .expect("role coverage is a bounded array");
    assert_eq!(roles.len(), 7);
    let required = roles
        .iter()
        .filter(|role| role["requirement"] == "required")
        .collect::<Vec<_>>();
    assert!(!required.is_empty());
    let derived_complete = required.iter().all(|role| {
        let selected = role["selected_items"].as_u64().unwrap_or(0);
        match role["status"].as_str() {
            Some("satisfied") => {
                assert!(selected > 0);
                assert!(role["missing_reason"].is_null());
                true
            }
            Some("missing_required") => {
                assert_eq!(selected, 0);
                assert!(role["missing_reason"].is_string());
                false
            }
            status => panic!("required role has invalid status {status:?}: {role:#}"),
        }
    });
    assert_eq!(coverage["complete"], derived_complete);
    assert_eq!(
        completeness["state"] == "complete",
        derived_complete,
        "envelope completeness must agree with required-role coverage"
    );
}

fn assert_accounting_is_bounded(output: &Value, token_budget: u64) {
    let accounting = &output["data"]["token_accounting"];
    let estimated_total = accounting["estimated_total"]
        .as_u64()
        .expect("context accounting total is unsigned");
    assert!(estimated_total <= token_budget);
    let by_section = accounting["by_section"]
        .as_object()
        .expect("context accounting contains per-section counters");
    assert!(
        !by_section.is_empty(),
        "a non-empty pack requires per-section accounting"
    );
    assert_eq!(
        by_section
            .values()
            .map(|value| value.as_u64().expect("section tokens are unsigned"))
            .sum::<u64>(),
        estimated_total
    );
    assert!(
        output["usage"]["estimated_tokens"]
            .as_u64()
            .is_some_and(|tokens| tokens <= token_budget),
        "the final serialized representation must fit the original budget"
    );
}

fn structured(response: &Value) -> &Value {
    &response["result"]["structuredContent"]
}

fn assert_context_identity(output: &Value, fixture: &ContextFixture) {
    assert_eq!(
        output["repository"]["repository_id"], fixture.repository_id,
        "context response changed repository identity"
    );
    assert_eq!(
        output["generation"]["generation_id"], fixture.generation_id,
        "context response changed generation identity"
    );
    assert_eq!(output["trust"], "untrusted_repository_data");
}

fn assert_success(response: &Value, tool: &str) {
    assert!(
        response.get("error").is_none(),
        "{tool} returned a JSON-RPC error: {response:#}"
    );
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

struct ContextFixture {
    _root: tempfile::TempDir,
    daemon: DaemonProcess,
    mcp: McpProcess,
    repository_id: String,
    generation_id: String,
    symbols: Vec<Value>,
}

impl ContextFixture {
    fn spawn() -> Self {
        let root = tempfile::tempdir().expect("isolated context fixture is available");
        let repository_root = root.path().join("repository");
        let source_path = repository_root.join("src").join("lib.rs");
        fs::create_dir_all(source_path.parent().expect("source file has a parent"))
            .expect("fixture source directory is created");
        fs::write(
            repository_root.join("Cargo.toml"),
            "[package]\nname = \"context_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("fixture manifest is written");
        fs::write(&source_path, CONTEXT_SOURCE).expect("fixture source is written");

        let state_dir = root.path().join("state");
        let runtime_dir = root.path().join("runtime");
        let daemon_binary = ensure_daemon_binary();
        let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
        daemon.wait_until_ready(&runtime_dir);
        let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);
        let (repository_id, generation_id) =
            index_repository(&mut mcp, &repository_root, "initial-index");
        let symbol_names = ["context_entry".to_owned()];
        let symbols = locate_symbols(
            &mut mcp,
            &repository_id,
            &generation_id,
            "context_entry",
            &symbol_names,
        );

        Self {
            _root: root,
            daemon,
            mcp,
            repository_id,
            generation_id,
            symbols,
        }
    }

    fn context_arguments(&self, task: &str, token_budget: u64, response_profile: &str) -> Value {
        json!({
            "repository": {"repository_id": self.repository_id},
            "generation": self.generation_id,
            "task": task,
            "seeds": {"symbols": self.symbols},
            "token_budget": token_budget,
            "response_profile": response_profile,
        })
    }

    fn context_call(
        &mut self,
        id: &str,
        task: &str,
        token_budget: u64,
        response_profile: &str,
    ) -> Value {
        let arguments = self.context_arguments(task, token_budget, response_profile);
        self.mcp.call(id, "context.pack", arguments)
    }

    fn finish(mut self) {
        self.mcp.finish();
        self.daemon.finish();
    }
}

fn index_repository(mcp: &mut McpProcess, root: &Path, id: &str) -> (String, String) {
    let response = mcp.call(
        id,
        "repo.index",
        json!({"root": root, "mode": "auto", "detached": false}),
    );
    assert_success(&response, "repo.index");
    let data = &structured(&response)["data"];
    let repository_id = required_string(&data["repository_id"], "repository identity");
    let operation_id = required_string(&data["operation_id"], "operation identity");
    let generation_id = if data["state"] == "published" {
        required_string(&data["published_generation"], "published generation")
    } else {
        wait_for_publication(mcp, &operation_id)
    };
    (repository_id, generation_id)
}

fn wait_for_publication(mcp: &mut McpProcess, operation_id: &str) -> String {
    for attempt in 0..30 {
        let response = mcp.call(
            &format!("operation-{operation_id}-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 1_000}),
        );
        assert_success(&response, "operation.status");
        let data = &structured(&response)["data"];
        match data["operation"]["state"].as_str() {
            Some("published") => {
                return required_string(&data["published_generation"], "published generation");
            }
            Some("failed" | "cancelled") => {
                panic!("fixture indexing terminated without publication: {response:#}")
            }
            _ => {}
        }
    }
    panic!("fixture indexing did not publish within the bounded wait");
}

fn locate_symbols(
    mcp: &mut McpProcess,
    repository_id: &str,
    generation_id: &str,
    query: &str,
    names: &[String],
) -> Vec<Value> {
    let response = mcp.call(
        "locate-context-symbols",
        "code.locate",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": generation_id,
            "query": query,
            "search_modes": ["lexical"],
            "max_results": 20
        }),
    );
    assert_success(&response, "code.locate");
    let matches = structured(&response)["data"]["matches"]
        .as_array()
        .expect("code.locate returns matches");
    names
        .iter()
        .map(|name| {
            matches
                .iter()
                .find(|matched| matched["display_name"] == name.as_str())
                .unwrap_or_else(|| panic!("setup locate omitted {name}: {matches:#?}"))["symbol_id"]
                .clone()
        })
        .collect()
}

fn ensure_daemon_binary() -> PathBuf {
    static DAEMON_BINARY: OnceLock<PathBuf> = OnceLock::new();
    DAEMON_BINARY.get_or_init(build_daemon_binary).clone()
}

fn build_daemon_binary() -> PathBuf {
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
    responses: std::sync::mpsc::Receiver<String>,
    reader: Option<thread::JoinHandle<()>>,
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
        let (responses_tx, responses) = std::sync::mpsc::sync_channel(64);
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
                "clientInfo": {"name": "context-process", "version": "1.0"},
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
        const MAX_ATTEMPTS: usize = 3;

        let mut response = Value::Null;
        for attempt in 1..=MAX_ATTEMPTS {
            let attempt_id = format!("{id}-attempt-{attempt}");
            self.write(&json!({
                "jsonrpc": "2.0",
                "id": attempt_id,
                "method": "tools/call",
                "params": {"name": tool, "arguments": arguments.clone()}
            }));
            response = self.read();
            assert_eq!(response["id"], attempt_id);
            if response["error"]["code"] != -32603 {
                return response;
            }
            thread::yield_now();
        }
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
        self.reader
            .take()
            .expect("MCP response reader is retained")
            .join()
            .expect("MCP response reader exits");
        self.child.take();
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
