//! Production-process evidence for exposure-profile authorization.
//!
//! The matrix keeps rejected calls transport-only so a profile bypass cannot
//! accidentally create daemon work while the test is proving preflight.

use std::{
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_ids::RepositoryId;
use rootlight_mcp_contract::{McpTool, context::BatchTool};
use serde_json::{Value, json};

const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const UNTRUSTED_MARKER: &str = "untrusted-control-marker";

const SCOUT_TOOLS: &[&str] = &[
    "repo.status",
    "code.locate",
    "symbol.explain",
    "context.pack",
    "source.read",
    "query.batch",
];

const ANALYSIS_TOOLS: &[&str] = &[
    "repo.status",
    "code.locate",
    "symbol.explain",
    "symbol.relationships",
    "flow.trace",
    "change.impact",
    "tests.select",
    "architecture.overview",
    "architecture.cycles",
    "code.dead",
    "context.pack",
    "source.read",
    "query.batch",
];

const DEVELOPER_TOOLS: &[&str] = &[
    "repo.index",
    "repo.status",
    "repo.list",
    "operation.status",
    "code.locate",
    "symbol.explain",
    "symbol.relationships",
    "flow.trace",
    "change.impact",
    "tests.select",
    "architecture.overview",
    "architecture.cycles",
    "code.dead",
    "history.compare",
    "plan.change",
    "context.pack",
    "source.read",
    "query.advanced",
    "query.batch",
];

#[test]
fn profiles_enforce_discovery_annotations_and_every_hidden_process_path() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    for (profile, expected_tools) in [
        ("scout", SCOUT_TOOLS),
        ("analysis", ANALYSIS_TOOLS),
        ("developer", DEVELOPER_TOOLS),
    ] {
        let mut process = McpProcess::spawn(
            fixture.path(),
            profile,
            "developer",
            &format!("profile-{profile}"),
        );
        let initial_list = process.list_tools(&format!("list-{profile}"));
        assert_profile_listing(profile, &initial_list, expected_tools);

        for hidden in McpTool::ALL
            .iter()
            .map(|tool| tool.name())
            .filter(|name| !expected_tools.contains(name))
        {
            let response = process.call_with_metadata(
                &format!("direct-{profile}-{hidden}"),
                hidden,
                json!({
                    "schema_version": "0.9",
                    "objective_text": UNTRUSTED_MARKER
                }),
            );
            assert_unavailable(&response, hidden);
        }

        for hidden in BatchTool::ALL
            .iter()
            .map(|tool| tool.name())
            .filter(|name| !expected_tools.contains(name))
        {
            let response = process.call_with_metadata(
                &format!("batch-{profile}-{hidden}"),
                "query.batch",
                json!({
                    "repository": {
                        "repository_id": RepositoryId::from_bytes([31; 16])
                    },
                    "operations": [{
                        "id": "hidden",
                        "tool": hidden,
                        "arguments": {
                            "schema_version": "0.9",
                            "objective_text": UNTRUSTED_MARKER
                        }
                    }]
                }),
            );
            assert_public_error(&response, "UNSUPPORTED_CAPABILITY");
        }

        let final_list = process.list_tools(&format!("list-after-rejections-{profile}"));
        assert_eq!(
            final_list["result"], initial_list["result"],
            "{profile} discovery changed after rejected untrusted requests"
        );
        assert!(
            !serde_json::to_string(&final_list)
                .expect("tools/list response serializes")
                .contains(UNTRUSTED_MARKER),
            "{profile} discovery incorporated rejected untrusted text"
        );
        process.finish();
    }
}

fn assert_profile_listing(profile: &str, response: &Value, expected_tools: &[&str]) {
    let tools = response["result"]["tools"]
        .as_array()
        .expect("tools/list returns an array");
    let names = tools
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name is a string"))
        .collect::<Vec<_>>();
    assert_eq!(names, expected_tools, "{profile} membership differs");

    for tool in tools {
        let name = tool["name"].as_str().expect("tool name is a string");
        let annotations = &tool["annotations"];
        assert_eq!(
            annotations["readOnlyHint"],
            name != "repo.index" && name != "operation.status",
            "{profile} {name} read-only annotation differs"
        );
        assert_eq!(
            annotations["idempotentHint"],
            name != "repo.index",
            "{profile} {name} idempotency annotation differs"
        );
        assert_eq!(
            annotations["destructiveHint"], false,
            "{profile} {name} destructive annotation differs"
        );
        assert_eq!(
            annotations["openWorldHint"], false,
            "{profile} {name} open-world annotation differs"
        );
        assert_eq!(
            tool["execution"]["taskSupport"], "forbidden",
            "{profile} {name} task support differs"
        );
    }

    if profile == "developer" {
        let operation_status = tools
            .iter()
            .find(|tool| tool["name"] == "operation.status")
            .expect("developer exposes operation.status");
        assert_eq!(operation_status["annotations"]["readOnlyHint"], false);
        assert_eq!(operation_status["annotations"]["idempotentHint"], true);
    }
}

fn assert_unavailable(response: &Value, tool: &str) {
    assert_eq!(
        response["error"]["code"], -32_602,
        "{tool} must fail at profile authorization"
    );
    assert_eq!(response["error"]["message"], "tool is not available");
    assert!(
        response.get("result").is_none(),
        "{tool} must not reach typed execution"
    );
}

fn assert_public_error(response: &Value, expected_code: &str) {
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"],
        expected_code
    );
}

struct McpProcess {
    child: Option<Child>,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
}

impl McpProcess {
    fn spawn(root: &Path, ceiling: &str, requested: &str, name: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
            .arg("--transport-only")
            .env("ROOTLIGHT_STATE_DIR", root.join(format!("{name}-state")))
            .env(
                "ROOTLIGHT_RUNTIME_DIR",
                root.join(format!("{name}-runtime")),
            )
            .env("ROOTLIGHT_MCP_PROFILE", ceiling)
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", ceiling)
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
                "clientInfo": {"name": "profile-security-process", "version": "1.0"},
                "initializationOptions": {
                    "rootlight_exposure_profile": requested
                }
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

    fn list_tools(&mut self, id: &str) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/list",
            "params": {
                "_meta": {
                    "rootlight/capabilities": {
                        "contractVersion": "0.9",
                        "profiles": ["developer"],
                        "inputShapeHash": "stale"
                    }
                }
            }
        }));
        let response = self.read();
        assert_eq!(response["id"], id);
        response
    }

    fn call_with_metadata(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": tool,
                "arguments": arguments,
                "_meta": {
                    "progressToken": id,
                    "rootlight/capabilities": {
                        "contractVersion": "0.9",
                        "profiles": ["developer"],
                        "inputShapeHash": "stale",
                        "trusted": true
                    }
                }
            }
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
        let status = wait_for_exit(child);
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

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
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
