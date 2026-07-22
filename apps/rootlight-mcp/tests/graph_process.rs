//! Production-process evidence for the five public graph-analysis tools.
//!
//! The matrix crosses the real MCP stdio boundary and a supervised daemon.

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
use serde_json::{Map, Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const GRAPH_TOOLS: [&str; 5] = [
    "symbol.relationships",
    "flow.trace",
    "architecture.overview",
    "architecture.cycles",
    "code.dead",
];

#[test]
fn graph_tools_preserve_bounded_truthful_contracts_across_processes() {
    let fixture = tempfile::tempdir().expect("isolated graph process fixture is available");
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");

    assert_unsupported_inputs_are_rejected_without_a_daemon(&state_dir, &runtime_dir);

    let repository_root = fixture.path().join("repository");
    write_repository_fixture(&repository_root);
    let daemon_binary = ensure_daemon_binary();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(false, &state_dir, &runtime_dir);

    let index = index_repository(&mut mcp, &repository_root);
    let symbol_id = locate_symbol(&mut mcp, &index, "matrix_entry");
    let descriptions = tool_descriptions(&mut mcp);
    assert_graph_descriptions_are_bounded(&descriptions);

    let repository = || json!({"repository_id": index.repository_id});
    let generation = || Value::String(index.generation_id.clone());
    let mut outputs = Map::new();
    for (index_number, (tool, arguments)) in
        graph_tool_calls(&index, &symbol_id).into_iter().enumerate()
    {
        let response = mcp.call(&format!("graph-{index_number}"), tool, arguments);
        assert_success(&response, tool);
        let output = response["result"]["structuredContent"].clone();
        assert_common_read_contract(tool, &output, &index);
        outputs.insert(tool.to_owned(), output);
    }

    let relationships = &outputs["symbol.relationships"];
    assert!(relationships["data"]["groups"].is_array());
    assert_eq!(relationships["data"]["totals"]["exact"], true);
    let repeated = mcp.call(
        "graph-relationships-repeat",
        "symbol.relationships",
        json!({
            "repository": repository(),
            "generation": generation(),
            "symbol_ids": [symbol_id],
            "relations": ["calls"],
            "direction": "outbound",
            "max_results": 1
        }),
    );
    assert_success(&repeated, "symbol.relationships");
    let repeated = &repeated["result"]["structuredContent"];
    for field in [
        "repository",
        "generation",
        "data",
        "coverage",
        "completeness",
        "truncated",
        "next_cursor",
        "trust",
    ] {
        assert_eq!(
            relationships[field], repeated[field],
            "symbol.relationships changed deterministic field {field}"
        );
    }

    let trace = &outputs["flow.trace"];
    assert_eq!(trace["data"]["projection"]["relations"], json!(["calls"]));
    assert!(
        trace["data"]["frontier"]["reached_nodes"]
            .as_u64()
            .is_some()
    );
    assert!(
        trace["data"]["frontier"]["examined_edges"]
            .as_u64()
            .is_some()
    );

    let overview = &outputs["architecture.overview"];
    assert_eq!(
        overview["data"]["components"]
            .as_array()
            .expect("architecture.overview returns components")
            .len(),
        1
    );
    assert_eq!(overview["truncated"], true);
    assert_eq!(overview["completeness"]["state"], "truncated");
    assert_eq!(overview["completeness"]["continuation"], "unavailable");
    assert!(
        overview["completeness"]["limiting_resources"]
            .as_array()
            .expect("truncated overview names limiting resources")
            .iter()
            .any(|resource| resource["kind"] == "results")
    );
    assert!(
        !overview["completeness"]["guidance"]
            .as_array()
            .expect("truncated overview provides guidance")
            .is_empty()
    );
    assert!(overview["next_cursor"].is_null());

    let cycles = &outputs["architecture.cycles"];
    assert!(cycles["data"]["components"].is_array());
    assert!(cycles["data"]["cycles"].is_array());
    assert!(cycles["data"]["break_candidates"].is_array());
    if cycles["data"]["cycles"]
        .as_array()
        .expect("architecture.cycles returns cycles")
        .is_empty()
    {
        assert!(
            descriptions["architecture.cycles"]
                .as_str()
                .expect("architecture.cycles has a description")
                .contains("static"),
            "an empty cycle result must remain scoped to static evidence"
        );
    }

    let dead = &outputs["code.dead"];
    assert_eq!(dead["data"]["entry_points"]["policy"], "standard");
    assert_eq!(dead["data"]["entry_points"]["complete"], false);
    let blind_spots = dead["data"]["blind_spots"]
        .as_array()
        .expect("code.dead returns blind spots");
    for category in [
        "dynamic_dispatch",
        "incomplete_language_coverage",
        "partial_entry_point_model",
    ] {
        assert!(
            blind_spots
                .iter()
                .any(|blind_spot| blind_spot["category"] == category),
            "code.dead omitted the {category} caveat"
        );
    }
    for candidate in dead["data"]["candidates"]
        .as_array()
        .expect("code.dead returns candidates")
    {
        assert!(
            candidate["why"]
                .as_array()
                .expect("a dead-code observation explains its basis")
                .iter()
                .any(|reason| reason == "not_observed_from_partial_entry_points")
        );
        assert!(
            candidate["classification"]
                .as_str()
                .expect("a dead-code observation is classified")
                .starts_with("not_observed")
        );
    }
    assert!(
        descriptions["code.dead"]
            .as_str()
            .expect("code.dead has a description")
            .contains("do not prove runtime liveness")
    );

    assert_profile_matrix(&mut mcp, &index, &symbol_id, &outputs);
    assert_hard_token_budget_taxonomy(&mut mcp, &index, &symbol_id);
    assert_truncated_negative_analyses_are_caveated(&mut mcp, &index, &symbol_id);

    mcp.finish();
    daemon.finish();
}

fn graph_tool_calls(index: &IndexReceipt, symbol_id: &str) -> [(&'static str, Value); 5] {
    let repository = || json!({"repository_id": index.repository_id});
    let generation = || Value::String(index.generation_id.clone());
    [
        (
            "symbol.relationships",
            json!({
                "repository": repository(),
                "generation": generation(),
                "symbol_ids": [symbol_id],
                "relations": ["calls"],
                "direction": "outbound",
                "max_results": 1
            }),
        ),
        (
            "flow.trace",
            json!({
                "repository": repository(),
                "generation": generation(),
                "from": {"symbol_id": symbol_id},
                "relations": ["calls"],
                "direction": "outbound",
                "max_depth": 1,
                "max_paths": 1
            }),
        ),
        (
            "architecture.overview",
            json!({
                "repository": repository(),
                "generation": generation(),
                "views": ["hotspots"],
                "max_components": 1,
                "include_edges": true
            }),
        ),
        (
            "architecture.cycles",
            json!({
                "repository": repository(),
                "generation": generation(),
                "projection": {"relations": ["calls"], "level": "symbol"},
                "max_cycles": 1
            }),
        ),
        (
            "code.dead",
            json!({
                "repository": repository(),
                "generation": generation(),
                "entry_point_policy": "standard",
                "max_candidates": 1
            }),
        ),
    ]
}

fn assert_profile_matrix(
    mcp: &mut McpProcess,
    index: &IndexReceipt,
    symbol_id: &str,
    compact_outputs: &Map<String, Value>,
) {
    for profile in ["compact", "standard", "evidence"] {
        for (tool_index, (tool, mut arguments)) in
            graph_tool_calls(index, symbol_id).into_iter().enumerate()
        {
            arguments["response_profile"] = json!(profile);
            let response = mcp.call(
                &format!("graph-profile-{profile}-{tool_index}"),
                tool,
                arguments,
            );
            assert_success(&response, tool);
            let output = &response["result"]["structuredContent"];
            assert_common_read_contract(tool, output, index);
            assert_exact_json_usage(tool, output);
            for field in [
                "repository",
                "generation",
                "coverage",
                "completeness",
                "truncated",
                "next_cursor",
                "trust",
            ] {
                assert_eq!(
                    output[field], compact_outputs[tool][field],
                    "{tool} changed invariant field {field} under the {profile} profile"
                );
            }
        }
    }
}

fn assert_hard_token_budget_taxonomy(mcp: &mut McpProcess, index: &IndexReceipt, symbol_id: &str) {
    for (tool_index, (tool, mut arguments)) in
        graph_tool_calls(index, symbol_id).into_iter().enumerate()
    {
        arguments["response_profile"] = json!("evidence");
        arguments["budget"] = json!({"max_tokens": 100});
        let response = mcp.call(&format!("graph-token-budget-{tool_index}"), tool, arguments);
        assert_public_error(&response, "BUDGET_EXCEEDED");
    }
}

fn assert_truncated_negative_analyses_are_caveated(
    mcp: &mut McpProcess,
    index: &IndexReceipt,
    symbol_id: &str,
) {
    for (tool, data_field) in [
        ("architecture.cycles", "cycles"),
        ("code.dead", "candidates"),
    ] {
        let (_, mut arguments) = graph_tool_calls(index, symbol_id)
            .into_iter()
            .find(|(candidate, _)| *candidate == tool)
            .expect("the graph matrix contains every bounded negative analysis");
        arguments["budget"] = json!({"max_traversal_facts": 1});
        let response = mcp.call(&format!("graph-{tool}-small-traversal"), tool, arguments);
        assert_success(&response, tool);
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract(tool, output, index);
        assert!(output["data"][data_field].is_array());
        if tool == "architecture.cycles" {
            assert_eq!(output["data"][data_field], json!([]));
        } else {
            assert_eq!(output["data"]["entry_points"]["complete"], false);
            assert!(
                !output["data"]["blind_spots"]
                    .as_array()
                    .expect("truncated dead-code result retains blind spots")
                    .is_empty()
            );
        }
        assert_eq!(output["truncated"], true);
        assert_eq!(output["completeness"]["state"], "truncated");
        assert_eq!(output["completeness"]["continuation"], "unavailable");
        assert!(
            output["completeness"]["limiting_resources"]
                .as_array()
                .expect("truncated graph result names limiting resources")
                .iter()
                .any(|resource| resource["kind"] == "edges")
        );
        assert!(
            !output["completeness"]["guidance"]
                .as_array()
                .expect("truncated graph result provides guidance")
                .is_empty()
        );
    }
}

fn assert_unsupported_inputs_are_rejected_without_a_daemon(state_dir: &Path, runtime_dir: &Path) {
    let repository_id = RepositoryId::from_bytes([3; 16]);
    let symbol_id = SymbolId::from_bytes([7; 20]);
    let mut mcp = McpProcess::spawn(true, state_dir, runtime_dir);
    for (case, tool, arguments) in [
        (
            "unsupported-relationships",
            "symbol.relationships",
            json!({
                "repository": {"repository_id": repository_id},
                "symbol_ids": [symbol_id],
                "relations": ["data_flow"]
            }),
        ),
        (
            "unsupported-flow",
            "flow.trace",
            json!({
                "repository": {"repository_id": repository_id},
                "from": {"symbol_id": symbol_id},
                "relations": ["called_by"]
            }),
        ),
        (
            "unsupported-cycles",
            "architecture.cycles",
            json!({
                "repository": {"repository_id": repository_id},
                "projection": {"relations": ["messaging"], "level": "symbol"}
            }),
        ),
        (
            "unsupported-dead-policy",
            "code.dead",
            json!({
                "repository": {"repository_id": repository_id},
                "entry_point_policy": "library"
            }),
        ),
        (
            "unsupported-overview-view",
            "architecture.overview",
            json!({
                "repository": {"repository_id": repository_id},
                "views": ["services"]
            }),
        ),
        (
            "unsupported-budget-evidence",
            "architecture.overview",
            json!({
                "repository": {"repository_id": repository_id},
                "budget": {"evidence_level": "full"}
            }),
        ),
    ] {
        let response = mcp.call(case, tool, arguments);
        assert_public_error(&response, "UNSUPPORTED_CAPABILITY");
    }
    mcp.finish();
}

fn write_repository_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"graph_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        root.join("src/lib.rs"),
        "mod alpha;\nmod beta;\npub fn matrix_entry() -> usize { alpha::alpha() + beta::beta() }\n",
    )
    .expect("fixture root source is written");
    fs::write(root.join("src/alpha.rs"), "pub fn alpha() -> usize { 1 }\n")
        .expect("first fixture module is written");
    fs::write(root.join("src/beta.rs"), "pub fn beta() -> usize { 2 }\n")
        .expect("second fixture module is written");
}

fn index_repository(mcp: &mut McpProcess, root: &Path) -> IndexReceipt {
    let response = mcp.call(
        "index",
        "repo.index",
        json!({"root": root, "mode": "auto", "detached": false}),
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
    IndexReceipt {
        repository_id,
        generation_id,
    }
}

fn wait_for_publication(mcp: &mut McpProcess, operation_id: &str) -> String {
    for attempt in 0..30 {
        let response = mcp.call(
            &format!("operation-{attempt}"),
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
                panic!("fixture indexing terminated without publication: {response:#}")
            }
            _ => {}
        }
    }
    panic!("fixture indexing did not publish within the bounded wait");
}

fn locate_symbol(mcp: &mut McpProcess, index: &IndexReceipt, query: &str) -> String {
    let response = mcp.call(
        "locate",
        "code.locate",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "query": query,
            "search_modes": ["exact"],
            "max_results": 2
        }),
    );
    assert_success(&response, "code.locate");
    let matches = response["result"]["structuredContent"]["data"]["matches"]
        .as_array()
        .expect("code.locate returns matches");
    assert_eq!(matches.len(), 1, "setup locate returns one exact symbol");
    required_string(&matches[0]["symbol_id"], "symbol identity")
}

fn tool_descriptions(mcp: &mut McpProcess) -> Map<String, Value> {
    let response = mcp.request("tools", "tools/list", json!({}));
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools");
    let mut descriptions = Map::new();
    for tool in tools {
        let name = tool["name"].as_str().expect("a listed tool has a name");
        if GRAPH_TOOLS.contains(&name) {
            descriptions.insert(name.to_owned(), tool["description"].clone());
        }
    }
    assert_eq!(
        descriptions.len(),
        GRAPH_TOOLS.len(),
        "all graph tools are advertised"
    );
    descriptions
}

fn assert_graph_descriptions_are_bounded(descriptions: &Map<String, Value>) {
    for tool in GRAPH_TOOLS {
        let description = descriptions[tool]
            .as_str()
            .expect("a graph tool has a description");
        assert!(
            description.contains("bounded"),
            "{tool} must advertise bounded execution"
        );
        assert!(
            description.contains("unsupported"),
            "{tool} must advertise unsupported dimensions"
        );
    }
}

fn assert_common_read_contract(tool: &str, output: &Value, index: &IndexReceipt) {
    assert_eq!(
        output["repository"]["repository_id"], index.repository_id,
        "{tool} changed repository identity"
    );
    assert_eq!(
        output["generation"]["generation_id"], index.generation_id,
        "{tool} changed generation identity"
    );
    assert_eq!(output["trust"], "untrusted_repository_data");
    assert!(output["coverage"]["status"].is_string());
    assert!(output["coverage"]["skipped_inputs"].as_u64().is_some());
    assert!(output["completeness"]["state"].is_string());
    assert!(output["completeness"]["limiting_resources"].is_array());
    assert!(output["completeness"]["continuation"].is_string());
    assert!(output["completeness"]["guidance"].is_array());
    assert!(output["truncated"].is_boolean());
    assert!(output.get("next_cursor").is_some());
    for counter in [
        "rows",
        "edges",
        "source_bytes",
        "json_bytes",
        "estimated_tokens",
        "wall_time_ms",
    ] {
        assert!(
            output["usage"][counter].as_u64().is_some(),
            "{tool} omitted usage.{counter}"
        );
    }
    assert!(
        output["usage"]["trace_id"]
            .as_str()
            .is_some_and(|trace_id| !trace_id.is_empty()),
        "{tool} omitted its trace identity"
    );
    assert_eq!(
        output["truncated"],
        output["completeness"]["state"] == "truncated",
        "{tool} disagrees about truncation"
    );
}

fn assert_exact_json_usage(tool: &str, output: &Value) {
    let serialized = serde_json::to_vec(output).expect("structured graph output serializes");
    assert_eq!(
        output["usage"]["json_bytes"].as_u64(),
        u64::try_from(serialized.len()).ok(),
        "{tool} did not report its exact serialized byte count"
    );
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

fn assert_public_error(response: &Value, expected: &str) {
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"], expected,
        "unexpected process error: {response:#}"
    );
}

fn required_string(value: &Value, field: &str) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{field} is absent: {value:#}"))
        .to_owned()
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

struct IndexReceipt {
    repository_id: String,
    generation_id: String,
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
    fn spawn(transport_only: bool, state_dir: &Path, runtime_dir: &Path) -> Self {
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
        let mut process = Self {
            input: child.stdin.take(),
            output: child.stdout.take().map(BufReader::new),
            child: Some(child),
        };
        let response = process.request(
            "initialize",
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "graph-process", "version": "1.0"},
                "initializationOptions": {"rootlight_exposure_profile": "developer"}
            }),
        );
        assert_eq!(response["id"], "initialize");
        process.write(&json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }));
        process
    }

    fn call(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.request(
            id,
            "tools/call",
            json!({"name": tool, "arguments": arguments}),
        )
    }

    fn request(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
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
