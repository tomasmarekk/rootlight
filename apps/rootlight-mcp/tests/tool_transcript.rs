//! Retained end-to-end transcript coverage for the first MCP tool vertical.

use std::sync::Arc;

use rootlight_mcp::{
    RequestCancellation, RequestHandler, Session, StdioLimits, ToolExecutionError,
    ToolExecutionFuture, ToolExecutor, ToolRouter, serve,
};
use rootlight_mcp_contract::{
    ErrorCode, OperationStatusOutput, PublicError, RepoIndexOutput, VerticalTool,
};
use serde_json::{Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug)]
struct TranscriptExecutor;

impl ToolExecutor for TranscriptExecutor {
    fn execute(
        &self,
        tool: VerticalTool,
        _arguments: Map<String, Value>,
        _cancellation: RequestCancellation,
    ) -> ToolExecutionFuture {
        Box::pin(async move {
            match tool {
                VerticalTool::RepoIndex => Ok(retained_output("repo.index")),
                VerticalTool::OperationStatus => {
                    let error =
                        PublicError::builder(ErrorCode::NotFound, "requested entity was not found")
                            .build()
                            .expect("retained domain error is checked");
                    Err(ToolExecutionError::new(error))
                }
                _ => panic!("retained transcript invokes only index and operation status"),
            }
        })
    }
}

#[tokio::test]
async fn retained_tool_transcript_preserves_protocol_and_contract_outcomes() {
    let (mut input_writer, input_reader) = tokio::io::duplex(64 * 1024);
    let (output_writer, output_reader) = tokio::io::duplex(64 * 1024);
    let handler: Arc<dyn RequestHandler> =
        Arc::new(ToolRouter::new(TranscriptExecutor).expect("retained tool registry compiles"));
    let server = tokio::spawn(async move {
        let mut session = Session::rootlight();
        serve(
            BufReader::new(input_reader),
            output_writer,
            &mut session,
            handler,
            StdioLimits::default(),
        )
        .await
    });
    let mut output_lines = BufReader::new(output_reader).lines();
    let mut responses = Vec::new();

    for frame in include_str!("../../../tests/fixtures/mcp/1.0/tool-transcript.jsonl").lines() {
        input_writer
            .write_all(frame.as_bytes())
            .await
            .expect("transcript frame writes");
        input_writer
            .write_all(b"\n")
            .await
            .expect("transcript delimiter writes");
        let request: Value = serde_json::from_str(frame).expect("transcript frame is valid JSON");
        if request.get("id").is_some() {
            let response = output_lines
                .next_line()
                .await
                .expect("response stream reads")
                .expect("request has a response");
            responses.push(
                serde_json::from_str::<Value>(&response).expect("response line is valid JSON"),
            );
        }
    }
    input_writer
        .shutdown()
        .await
        .expect("transcript input closes");
    server
        .await
        .expect("server task joins")
        .expect("transcript session completes");

    assert_eq!(responses.len(), 7);
    let response = |id: &str| {
        responses
            .iter()
            .find(|response| response["id"] == id)
            .unwrap_or_else(|| panic!("response {id} exists"))
    };
    assert_eq!(
        response("initialize")["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert_eq!(
        response("list")["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert!(
        response("list")["result"]["tools"]
            .as_array()
            .expect("tool list is an array")
            .iter()
            .all(|tool| tool["execution"]["taskSupport"] == "forbidden")
    );

    let index = &response("index")["result"];
    assert_eq!(index["isError"], false);
    serde_json::from_value::<RepoIndexOutput>(index["structuredContent"].clone())
        .expect("index output follows the typed contract");
    assert_eq!(
        serde_json::from_str::<Value>(
            index["content"][0]["text"]
                .as_str()
                .expect("index output has a text mirror")
        )
        .expect("index mirror is JSON"),
        index["structuredContent"]
    );

    let invalid = &response("invalid-input")["result"];
    assert_eq!(invalid["isError"], true);
    assert_eq!(
        invalid["structuredContent"]["error"]["code"],
        "INVALID_ARGUMENT"
    );
    serde_json::from_value::<RepoIndexOutput>(invalid["structuredContent"].clone())
        .expect("invalid input follows the checked tool error contract");

    let domain = &response("domain-error")["result"];
    assert_eq!(domain["isError"], true);
    assert_eq!(domain["structuredContent"]["error"]["code"], "NOT_FOUND");
    serde_json::from_value::<OperationStatusOutput>(domain["structuredContent"].clone())
        .expect("domain error follows the checked tool error contract");

    assert_eq!(response("invalid-meta")["error"]["code"], -32_602);
    assert_eq!(response("unsupported-task")["error"]["code"], -32_601);
}

fn retained_output(name: &str) -> Map<String, Value> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/mcp/1.0/tool-contracts.json"
    ))
    .expect("retained tool contracts are valid JSON");
    fixture["tools"]
        .as_array()
        .expect("tool contracts contain an array")
        .iter()
        .find(|entry| entry["tool"] == name)
        .unwrap_or_else(|| panic!("retained tool contract {name} exists"))["output"]
        .as_object()
        .expect("retained output is an object")
        .clone()
}
