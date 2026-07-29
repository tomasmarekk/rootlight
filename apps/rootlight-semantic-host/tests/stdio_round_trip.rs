//! Real child-process coverage for the semantic host protocol.
//!
//! The test proves the shipped binary stays opt-in, source-free, and capable of
//! a complete health/build/query exchange over its bounded stdio transport.

#![forbid(unsafe_code)]

use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
};

use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use serde_json::{Value, json};

fn limits() -> Value {
    json!({
        "max_input_bytes": 1_048_576,
        "max_disk_bytes": 1_048_576,
        "max_memory_bytes": 4_194_304,
        "max_items": 100,
        "max_dimensions": 128,
        "max_results": 10
    })
}

fn exchange(stdin: &mut impl Write, stdout: &mut impl BufRead, request: &Value) -> Value {
    serde_json::to_writer(&mut *stdin, request).expect("request serializes");
    stdin.write_all(b"\n").expect("request delimiter writes");
    stdin.flush().expect("request reaches child");
    let mut line = String::new();
    stdout.read_line(&mut line).expect("response reads");
    serde_json::from_str(&line).expect("response is valid JSON")
}

#[test]
fn child_process_health_build_and_query_round_trip() {
    let working_directory = tempfile::tempdir().expect("isolated working directory is available");
    let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-semantic-host"))
        .current_dir(working_directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("semantic host starts only when explicitly invoked");
    let mut stdin = child.stdin.take().expect("child stdin is piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("child stdout is piped"));

    let health = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "schema": "rootlight.semantic-host/1",
            "request_id": "health-1",
            "operation": {"kind": "health", "payload": {}}
        }),
    );
    assert_eq!(health["outcome"]["status"], "ok");
    assert_eq!(
        health["outcome"]["payload"]["payload"]["persistence"],
        "none"
    );
    assert_eq!(
        health["outcome"]["payload"]["payload"]["repository_filesystem"],
        "unavailable"
    );

    let repository = RepositoryId::from_bytes([1; 16]);
    let generation = GenerationId::from_bytes([2; 20]);
    let model_hash = ContentHash::from_bytes([3; 32]);
    let build = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "schema": "rootlight.semantic-host/1",
            "request_id": "build-1",
            "operation": {
                "kind": "build",
                "payload": {
                    "repository": repository,
                    "generation": generation,
                    "model_id": "local-model-v1",
                    "model_hash": model_hash,
                    "chunk_policy_version": "chunk-policy-v1",
                    "items": [
                        {
                            "item_id": "item-b",
                            "content_hash": ContentHash::from_bytes([5; 32]),
                            "vector": [0.0, 1.0]
                        },
                        {
                            "item_id": "item-a",
                            "content_hash": ContentHash::from_bytes([4; 32]),
                            "vector": [1.0, 0.0]
                        }
                    ],
                    "limits": limits(),
                    "cancelled": false
                }
            }
        }),
    );
    assert_eq!(build["outcome"]["status"], "ok");
    assert_eq!(
        build["outcome"]["payload"]["payload"]["repository"],
        json!(repository)
    );
    assert_eq!(
        build["outcome"]["payload"]["payload"]["generation"],
        json!(generation)
    );
    let artifact = build["outcome"]["payload"]["payload"]["artifact_base64"]
        .as_str()
        .expect("build returns artifact bytes")
        .to_owned();

    let cancelled = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "schema": "rootlight.semantic-host/1",
            "request_id": "query-cancelled",
            "operation": {
                "kind": "query",
                "payload": {
                    "artifact_base64": artifact.clone(),
                    "repository": repository,
                    "generation": generation,
                    "model_id": "local-model-v1",
                    "model_hash": model_hash,
                    "chunk_policy_version": "chunk-policy-v1",
                    "vector": [1.0, 0.0],
                    "max_results": 2,
                    "limits": limits(),
                    "cancelled": true
                }
            }
        }),
    );
    assert_eq!(cancelled["outcome"]["status"], "error");
    assert_eq!(cancelled["outcome"]["payload"]["code"], "cancelled");

    let query = exchange(
        &mut stdin,
        &mut stdout,
        &json!({
            "schema": "rootlight.semantic-host/1",
            "request_id": "query-1",
            "operation": {
                "kind": "query",
                "payload": {
                    "artifact_base64": artifact,
                    "repository": repository,
                    "generation": generation,
                    "model_id": "local-model-v1",
                    "model_hash": model_hash,
                    "chunk_policy_version": "chunk-policy-v1",
                    "vector": [1.0, 0.0],
                    "max_results": 2,
                    "limits": limits(),
                    "cancelled": false
                }
            }
        }),
    );
    let first = &query["outcome"]["payload"]["payload"]["matches"][0];
    assert_eq!(query["outcome"]["status"], "ok");
    assert_eq!(
        query["outcome"]["payload"]["payload"]["repository"],
        json!(repository)
    );
    assert_eq!(
        query["outcome"]["payload"]["payload"]["generation"],
        json!(generation)
    );
    assert_eq!(first["item_id"], "item-a");
    assert_eq!(first["score"], 1.0);
    assert_eq!(first["model_id"], "local-model-v1");
    assert_eq!(first["model_hash"], json!(model_hash));
    assert_eq!(first["chunk_policy_version"], "chunk-policy-v1");

    drop(stdin);
    let status = child.wait().expect("child exits after EOF");
    assert!(status.success());
    assert!(
        working_directory
            .path()
            .read_dir()
            .expect("working directory remains readable")
            .next()
            .is_none(),
        "the byte-only host must not persist ambient files"
    );
}
