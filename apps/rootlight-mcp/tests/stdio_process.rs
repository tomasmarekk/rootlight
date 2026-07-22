//! Process-boundary coverage for the Rootlight MCP stdio bridge.

use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Command, Output, Stdio},
};

use rootlight_mcp_contract::{
    ExposureProfile,
    accounting::tool_list_payload,
    capability::{CAPABILITIES, DISCOVERY_METADATA_KEY},
};
use serde_json::Value;

#[test]
fn stdio_process_initializes_pings_and_exits_on_eof() {
    let output = run_process(
        br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{"roots":{"listChanged":true},"vendor.example/flag":true},"clientInfo":{"name":"fixture","version":"1.0","icons":[{"src":"data:image/png;base64,AA==","theme":"dark"}]}}}
{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
{"jsonrpc":"2.0","id":"ping","method":"ping","params":{"_meta":{"vendor.example/trace":"fixture"}}}
"#,
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());

    let responses = response_lines(&output);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        responses[0]["result"]["capabilities"],
        serde_json::json!({"tools": {"listChanged": false}})
    );
    assert_eq!(responses[1]["id"], "ping");
    assert_eq!(responses[1]["result"], serde_json::json!({}));
}

#[test]
fn raw_lf_input_is_rejected_without_leaking_peer_content_and_processing_recovers() {
    let output = run_process(
        b"{\"jsonrpc\":\"2.0\",\"id\":\"private-token\",\"method\":\"ping\",\"params\":{\"x\":\"raw\nline\"}}\n\
          {\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-11-25\",\"capabilities\":{},\"clientInfo\":{\"name\":\"fixture\",\"version\":\"1.0\"}}}\n\
          {\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n\
          {\"jsonrpc\":\"2.0\",\"id\":\"ping-after-malformed\",\"method\":\"ping\"}\n",
    );
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert!(
        !output
            .stdout
            .windows(b"private-token".len())
            .any(|window| { window == b"private-token" })
    );

    let responses = response_lines(&output);
    assert_eq!(responses.len(), 4);
    assert_eq!(responses[0]["error"]["code"], -32_700);
    assert_eq!(
        responses[0].as_object().and_then(|value| value.get("id")),
        Some(&Value::Null)
    );
    assert_eq!(responses[1]["error"]["code"], -32_700);
    assert_eq!(responses[2]["id"], 2);
    assert_eq!(responses[3]["id"], "ping-after-malformed");
}

#[test]
fn invalid_message_limit_exits_with_only_a_static_stderr_category() {
    let output = run_process(b"{\n{\n{\n{\n{\n{\n{\n{\n");
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr).trim(),
        "rootlight-mcp terminated: protocol_limit"
    );

    let responses = response_lines(&output);
    assert_eq!(responses.len(), 8);
    assert!(
        responses
            .iter()
            .all(|response| response["error"]["code"] == -32_700)
    );
}

#[test]
fn repo_list_cursor_failures_share_the_public_restart_contract() {
    let cursors = [
        String::new(),
        "A".repeat(4_097),
        "\u{1f4a1}".repeat(1_025),
        "c2.A".to_owned(),
    ];
    let expected_error = serde_json::json!({
        "code": "INVALID_CURSOR",
        "message": "pagination cursor is invalid or expired",
        "retryable": false,
        "retry_after_ms": null,
        "repository": null,
        "operation": null,
        "generation": null,
        "details": {},
        "next_actions": [{"action": "restart_enumeration"}]
    });

    for (index, cursor) in cursors.into_iter().enumerate() {
        let (initialize, response, output) = run_initialized_call(serde_json::json!({
            "jsonrpc": "2.0",
            "id": format!("cursor-{index}"),
            "method": "tools/call",
            "params": {"name": "repo.list", "arguments": {"cursor": cursor}}
        }));
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        assert_eq!(initialize["id"], "initialize");
        assert_eq!(
            response["result"]["isError"], true,
            "unexpected response: {response:#?}"
        );
        assert_eq!(
            response["result"]["structuredContent"]["error"],
            expected_error
        );
    }
}

#[test]
fn capability_registry_maps_every_tool_across_the_process_boundary() {
    let isolated = tempfile::tempdir().expect("isolated MCP runtime root is available");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
        .arg("--transport-only")
        .env("ROOTLIGHT_STATE_DIR", isolated.path().join("state"))
        .env("ROOTLIGHT_RUNTIME_DIR", isolated.path().join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("MCP fixture process starts");
    let mut input = child.stdin.take().expect("fixture stdin is piped");
    let output = child.stdout.take().expect("fixture stdout is piped");
    let mut output = BufReader::new(output);

    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "fixture", "version": "1.0"}
            }
        }),
    );
    assert_eq!(read_response(&mut output)["id"], "initialize");
    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );
    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "list",
            "method": "tools/list",
            "params": {}
        }),
    );
    let list_response = read_response(&mut output);
    let list = list_response["result"]["tools"]
        .as_array()
        .expect("tools/list result is an array");
    assert_eq!(list.len(), CAPABILITIES.len());
    for (listed, capability) in list.iter().zip(&CAPABILITIES) {
        assert_eq!(listed["name"], capability.tool.name());
    }
    for intent in [
        "code.locate",
        "change.impact",
        "architecture.overview",
        "history.compare",
        "context.pack",
        "query.batch",
    ] {
        let metadata = &list
            .iter()
            .find(|tool| tool["name"] == intent)
            .unwrap_or_else(|| panic!("{intent} is discoverable"))["_meta"][DISCOVERY_METADATA_KEY];
        assert_eq!(metadata["status"], "fallback_limited");
        assert!(
            metadata["fallbackSummary"]
                .as_str()
                .is_some_and(|summary| summary.starts_with("bounded"))
        );
    }

    for capability in &CAPABILITIES {
        let id = format!("call-{}", capability.tool.name());
        write_message(
            &mut input,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": id.clone(),
                "method": "tools/call",
                "params": {
                    "name": capability.tool.name(),
                    "arguments": {}
                }
            }),
        );
        let response = read_response(&mut output);
        assert_eq!(response["id"], id);
        if response["error"]["code"] == -32_603 {
            assert_eq!(response["error"]["message"], "tool transport failed");
        } else if response["result"]["isError"] == true {
            assert!(
                response["result"]["structuredContent"]["error"]["code"]
                    .as_str()
                    .is_some(),
                "{} must return a stable public error",
                capability.tool.name()
            );
        } else {
            assert!(
                response["result"]["structuredContent"].is_object(),
                "{} must return a checked handler result",
                capability.tool.name()
            );
        }
    }
    drop(input);
    drop(output);
    let status = child.wait().expect("MCP fixture process terminates");
    assert!(status.success());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("fixture stderr is piped")
        .read_to_string(&mut stderr)
        .expect("fixture stderr reads");
    assert!(stderr.is_empty());
}

#[test]
fn tools_list_payloads_match_all_profile_goldens_across_the_process_boundary() {
    let expected = [
        (
            ExposureProfile::Scout,
            193_362,
            "82eeca17c486228a72588fe3fa6889e13d8dea125eceb7cdf580817ceda6df2f",
        ),
        (
            ExposureProfile::Analysis,
            424_905,
            "68c1600528e13a07b0425408ef9db7ee5795354b6a605230b8df9577a03591fd",
        ),
        (
            ExposureProfile::Developer,
            578_475,
            "74f5823b6bf98024108057b423dcad8153f80aa996ead64ad413c03afd599039",
        ),
    ];
    for (profile, expected_bytes, expected_hash) in expected {
        let isolated = tempfile::tempdir().expect("isolated MCP runtime root is available");
        let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
            .arg("--transport-only")
            .env("ROOTLIGHT_STATE_DIR", isolated.path().join("state"))
            .env("ROOTLIGHT_RUNTIME_DIR", isolated.path().join("runtime"))
            .env("ROOTLIGHT_MCP_PROFILE", "developer")
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP fixture process starts");
        let mut input = child.stdin.take().expect("fixture stdin is piped");
        let output = child.stdout.take().expect("fixture stdout is piped");
        let mut output = BufReader::new(output);
        write_message(
            &mut input,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "initialize",
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-11-25",
                    "capabilities": {},
                    "clientInfo": {"name": "fixture", "version": "1.0"},
                    "initializationOptions": {
                        "rootlight_exposure_profile": profile.name()
                    }
                }
            }),
        );
        assert_eq!(read_response(&mut output)["id"], "initialize");
        write_message(
            &mut input,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        );
        write_message(
            &mut input,
            &serde_json::json!({
                "jsonrpc": "2.0",
                "id": "list",
                "method": "tools/list",
                "params": {}
            }),
        );
        let payload = read_response(&mut output)["result"].clone();
        assert_eq!(
            payload,
            tool_list_payload(profile),
            "{} process payload drifted from canonical accounting",
            profile.name()
        );
        let encoded = serde_json::to_vec(&payload).expect("tools/list payload serializes");
        assert_eq!(encoded.len(), expected_bytes);
        assert_eq!(blake3::hash(&encoded).to_hex().as_str(), expected_hash);
        drop(input);
        drop(output);
        let output = child
            .wait_with_output()
            .expect("MCP fixture process terminates");
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
    }
}

#[test]
fn scout_process_rejects_hidden_tool_invocation_after_profile_clamping() {
    let isolated = tempfile::tempdir().expect("isolated MCP runtime root is available");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
        .arg("--transport-only")
        .env("ROOTLIGHT_STATE_DIR", isolated.path().join("state"))
        .env("ROOTLIGHT_RUNTIME_DIR", isolated.path().join("runtime"))
        .env("ROOTLIGHT_MCP_PROFILE", "scout")
        .env("ROOTLIGHT_MCP_PROFILE_CEILING", "scout")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("MCP fixture process starts");
    let mut input = child.stdin.take().expect("fixture stdin is piped");
    let output = child.stdout.take().expect("fixture stdout is piped");
    let mut output = BufReader::new(output);

    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "fixture", "version": "1.0"},
                "initializationOptions": {"rootlight_exposure_profile": "developer"}
            }
        }),
    );
    assert_eq!(read_response(&mut output)["id"], "initialize");
    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );
    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "list",
            "method": "tools/list",
            "params": {}
        }),
    );
    let listed = read_response(&mut output);
    assert_eq!(listed["result"]["tools"].as_array().map(Vec::len), Some(6));

    write_message(
        &mut input,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "hidden-call",
            "method": "tools/call",
            "params": {"name": "query.advanced", "arguments": {}}
        }),
    );
    let hidden = read_response(&mut output);
    assert_eq!(hidden["id"], "hidden-call");
    assert_eq!(hidden["error"]["code"], -32_602);
    assert_eq!(hidden["error"]["message"], "tool is not available");

    drop(input);
    drop(output);
    let output = child
        .wait_with_output()
        .expect("MCP fixture process terminates");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

fn run_initialized_call(request: Value) -> (Value, Value, Output) {
    let isolated = tempfile::tempdir().expect("isolated MCP runtime root is available");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
        .arg("--transport-only")
        .env("ROOTLIGHT_STATE_DIR", isolated.path().join("state"))
        .env("ROOTLIGHT_RUNTIME_DIR", isolated.path().join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("MCP fixture process starts");
    let mut stdin = child.stdin.take().expect("fixture stdin is piped");
    let stdout = child.stdout.take().expect("fixture stdout is piped");
    let mut stdout = BufReader::new(stdout);

    write_message(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "cursor-fixture", "version": "1.0"}
            }
        }),
    );
    let initialize = read_response(&mut stdout);
    write_message(
        &mut stdin,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    );
    write_message(&mut stdin, &request);
    let response = read_response(&mut stdout);

    drop(stdin);
    drop(stdout);
    let output = child
        .wait_with_output()
        .expect("MCP fixture process terminates");
    (initialize, response, output)
}

fn write_message(writer: &mut impl Write, message: &Value) {
    serde_json::to_writer(&mut *writer, message).expect("fixture message serializes");
    writer.write_all(b"\n").expect("fixture message terminates");
    writer.flush().expect("fixture message flushes");
}

fn read_response(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("fixture response reads");
    serde_json::from_str(&line).expect("fixture response is valid JSON")
}

fn run_process(input: &[u8]) -> Output {
    let isolated = tempfile::tempdir().expect("isolated MCP runtime root is available");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
        .arg("--transport-only")
        .env("ROOTLIGHT_STATE_DIR", isolated.path().join("state"))
        .env("ROOTLIGHT_RUNTIME_DIR", isolated.path().join("runtime"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("MCP fixture process starts");
    child
        .stdin
        .as_mut()
        .expect("fixture stdin is piped")
        .write_all(input)
        .expect("fixture input writes");
    drop(child.stdin.take());

    child
        .wait_with_output()
        .expect("MCP fixture process terminates")
}

fn response_lines(output: &Output) -> Vec<Value> {
    output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).expect("response line is valid JSON"))
        .collect()
}
