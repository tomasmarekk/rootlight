//! Process-boundary coverage for the Rootlight MCP stdio bridge.

use std::{
    io::Write,
    process::{Command, Output, Stdio},
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
        serde_json::json!({})
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

fn run_process(input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
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
