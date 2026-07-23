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
        if result["status"] != "ok" {
            failures.push(format!(
                "{}: operation {} ({result})",
                descriptor.batch_tool.name(),
                result["error"]["code"]
            ));
            continue;
        }
        assert!(result["data"].is_object());
    }
    assert!(
        failures.is_empty(),
        "production batch adapters did not complete successfully: {failures:#?}"
    );
    assert_process_profile_semantics(&mut mcp, &repository_id);

    mcp.finish();
    daemon.finish();
}

#[test]
fn positive_retrievals_match_standalone_semantics_in_production_processes() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    let repository_root = fixture.path().join("repository");
    fs::create_dir_all(repository_root.join("src")).expect("fixture source directory is created");
    fs::write(
        repository_root.join("Cargo.toml"),
        "[package]\nname = \"batch_parity_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        repository_root.join("src").join("lib.rs"),
        "pub fn batch_parity_fixture() -> usize { 12 }\n",
    )
    .expect("fixture source is written");

    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = ensure_daemon_binary();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(false, &state_dir, &runtime_dir, "developer");

    let index = mcp.call(
        "parity-index",
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

    let locate_arguments = json!({
        "repository": {"repository_id": repository_id},
        "generation": "active",
        "query": "batch_parity_fixture",
        "search_modes": ["exact"]
    });
    let standalone_locate = mcp.call("standalone-locate", "code.locate", locate_arguments.clone());
    assert_success(&standalone_locate, "code.locate");
    let symbol =
        standalone_locate["result"]["structuredContent"]["data"]["matches"][0]["symbol_id"].clone();
    let batch_locate = mcp.call(
        "batch-locate",
        "query.batch",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "operations": [{
                "id": "locate",
                "tool": "code.locate",
                "arguments": {
                    "query": "batch_parity_fixture",
                    "search_modes": ["exact"]
                }
            }]
        }),
    );
    assert_standalone_batch_parity(&standalone_locate, &batch_locate, "code.locate");

    let standalone_explain = mcp.call(
        "standalone-explain",
        "symbol.explain",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "symbol_ids": [symbol.clone()]
        }),
    );
    assert_success(&standalone_explain, "symbol.explain");
    let batch_explain = mcp.call(
        "batch-explain",
        "query.batch",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "operations": [{
                "id": "explain",
                "tool": "symbol.explain",
                "arguments": {"symbol_ids": [symbol]}
            }]
        }),
    );
    assert_standalone_batch_parity(&standalone_explain, &batch_explain, "symbol.explain");

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
    for forbidden in [
        "query.batch",
        "repo.index",
        "repo.list",
        "repo.status",
        "operation.status",
        "history.compare",
        "query.advanced",
    ] {
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
    let mut excessive_depth = Vec::new();
    for index in 0..10 {
        let mut operation = json!({
            "id": format!("depth_{index}"),
            "tool": "code.locate",
            "arguments": {"query": "fixture"}
        });
        if index > 0 {
            operation["depends_on"] = json!([format!("depth_{}", index - 1)]);
        }
        excessive_depth.push(operation);
    }
    let oversized = (0..17)
        .map(|index| {
            json!({
                "id": format!("oversized_{index}"),
                "tool": "code.locate",
                "arguments": {"query": "fixture"}
            })
        })
        .collect::<Vec<_>>();
    let mut excessive_fan_in = (0..9)
        .map(|index| {
            json!({
                "id": format!("source_{index}"),
                "tool": "code.locate",
                "arguments": {"query": "fixture"}
            })
        })
        .collect::<Vec<_>>();
    excessive_fan_in.push(json!({
        "id": "dependent",
        "tool": "code.locate",
        "depends_on": (0..9).map(|index| format!("source_{index}")).collect::<Vec<_>>(),
        "arguments": {"query": "fixture"}
    }));
    for (case, operations, expected) in [
        ("empty", json!([]), "INVALID_ARGUMENT"),
        ("oversized", Value::Array(oversized), "INVALID_ARGUMENT"),
        (
            "invalid-id",
            json!([{
                "id": "invalid-id",
                "tool": "code.locate",
                "arguments": {"query": "fixture"}
            }]),
            "INVALID_ARGUMENT",
        ),
        (
            "duplicate-id",
            json!([
                {"id": "same", "tool": "code.locate", "arguments": {"query": "fixture"}},
                {"id": "same", "tool": "code.locate", "arguments": {"query": "fixture"}}
            ]),
            "INVALID_ARGUMENT",
        ),
        (
            "unknown-dependency",
            json!([{
                "id": "dependent",
                "tool": "code.locate",
                "depends_on": ["missing"],
                "arguments": {"query": "fixture"}
            }]),
            "INVALID_ARGUMENT",
        ),
        (
            "cycle",
            json!([
                {
                    "id": "first",
                    "tool": "code.locate",
                    "depends_on": ["second"],
                    "arguments": {"query": "fixture"}
                },
                {
                    "id": "second",
                    "tool": "code.locate",
                    "depends_on": ["first"],
                    "arguments": {"query": "fixture"}
                }
            ]),
            "INVALID_ARGUMENT",
        ),
        (
            "excessive-depth",
            Value::Array(excessive_depth),
            "INVALID_ARGUMENT",
        ),
        (
            "excessive-fan-in",
            Value::Array(excessive_fan_in),
            "INVALID_ARGUMENT",
        ),
        (
            "later-static-arguments",
            json!([
                {"id": "safe", "tool": "code.locate", "arguments": {"query": "fixture"}},
                {"id": "invalid", "tool": "code.locate", "arguments": {}}
            ]),
            "INVALID_ARGUMENT",
        ),
        (
            "incompatible-binding",
            json!([
                {"id": "find", "tool": "code.locate", "arguments": {"query": "fixture"}},
                {
                    "id": "refine",
                    "tool": "code.locate",
                    "depends_on": ["find"],
                    "arguments": {
                        "query": "fixture",
                        "search_modes": {
                            "$from": "find",
                            "source": "symbol_id",
                            "index": 0
                        }
                    }
                }
            ]),
            "INVALID_ARGUMENT",
        ),
        (
            "child-profile-override",
            json!([{
                "id": "impact",
                "tool": "change.impact",
                "arguments": {
                    "change": {"symbol_ids": [SymbolId::from_bytes([7; 20])]},
                    "profile": "standard"
                }
            }]),
            "INVALID_ARGUMENT",
        ),
        (
            "unsupported-local-evidence",
            json!([{
                "id": "locate",
                "tool": "code.locate",
                "arguments": {"query": "fixture"},
                "local_budget": {"evidence_level": "full"}
            }]),
            "UNSUPPORTED_CAPABILITY",
        ),
    ] {
        let response = developer.call(
            &format!("preflight-{case}"),
            "query.batch",
            json!({
                "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
                "operations": operations
            }),
        );
        assert_public_error(&response, expected);
    }
    for (case, tool, arguments) in [
        (
            "relationships-data-flow",
            "symbol.relationships",
            json!({
                "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
                "symbol_ids": [SymbolId::from_bytes([7; 20])],
                "relations": ["data_flow"]
            }),
        ),
        (
            "flow-called-by",
            "flow.trace",
            json!({
                "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
                "from": {"symbol_id": SymbolId::from_bytes([7; 20])},
                "relations": ["called_by"]
            }),
        ),
        (
            "cycles-messaging",
            "architecture.cycles",
            json!({
                "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
                "projection": {"relations": ["messaging"], "level": "symbol"}
            }),
        ),
        (
            "dead-library-policy",
            "code.dead",
            json!({
                "repository": {"repository_id": RepositoryId::from_bytes([3; 16])},
                "entry_point_policy": "library"
            }),
        ),
    ] {
        let response = developer.call(case, tool, arguments);
        assert_public_error(&response, "UNSUPPORTED_CAPABILITY");
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

#[test]
fn ordered_runtime_outcomes_match_the_public_process_golden() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    let repository_root = fixture.path().join("repository");
    fs::create_dir_all(repository_root.join("src")).expect("fixture source directory is created");
    fs::write(
        repository_root.join("Cargo.toml"),
        "[package]\nname = \"batch_outcome_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        repository_root.join("src").join("lib.rs"),
        "pub fn batch_outcome_fixture() -> usize { 12 }\n",
    )
    .expect("fixture source is written");

    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = ensure_daemon_binary();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(false, &state_dir, &runtime_dir, "developer");

    let index = mcp.call(
        "outcome-index",
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

    let locate = |id: &str, local_tokens: u16| {
        json!({
            "id": id,
            "tool": "code.locate",
            "arguments": {
                "query": "__batch_absent__",
                "search_modes": ["exact"]
            },
            "local_budget": {"max_tokens": local_tokens}
        })
    };
    let missing_binding = |id: &str, source: &str| {
        json!({
            "id": id,
            "tool": "plan.change",
            "depends_on": [source],
            "arguments": {
                "objective": "bug_fix",
                "objective_text": "fix the fixture",
                "targets": [{
                    "symbol_id": {
                        "$from": source,
                        "source": "symbol_id",
                        "index": 99
                    }
                }]
            }
        })
    };
    let cases = [
        (
            "mixed",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": "active",
                "operations": [
                    locate("success", 500),
                    missing_binding("failure", "success")
                ],
                "failure_policy": "continue_independent"
            }),
        ),
        (
            "all_error",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": "active",
                "operations": [locate("only_error", 100)],
                "failure_policy": "continue_independent"
            }),
        ),
        (
            "fail_fast",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": "active",
                "operations": [
                    locate("not_run", 500),
                    locate("source", 500),
                    missing_binding("failure", "source")
                ],
                "failure_policy": "fail_fast"
            }),
        ),
        (
            "resource_exhausted",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": "active",
                "operations": [locate("later", 500), locate("overrun", 500)],
                "failure_policy": "continue_independent",
                "budget": {"max_tokens": 500}
            }),
        ),
    ];
    let mut observed = serde_json::Map::new();
    for (name, arguments) in cases {
        let response = mcp.call(&format!("outcome-{name}"), "query.batch", arguments);
        assert_success(&response, "query.batch");
        observed.insert(
            name.to_owned(),
            process_outcome_snapshot(&response, &repository_id),
        );
    }

    mcp.finish();
    let hook_binary = build_batch_hook_mcp();
    let mut cancellation_mcp = McpProcess::spawn_with_binary(
        &hook_binary,
        false,
        &state_dir,
        &runtime_dir,
        "developer",
        true,
    );
    let cancelled = cancellation_mcp.call(
        "outcome-cancelled",
        "query.batch",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "operations": [locate("cancelled", 500)]
        }),
    );
    assert_success(&cancelled, "query.batch");
    observed.insert(
        "cancelled".to_owned(),
        process_outcome_snapshot(&cancelled, &repository_id),
    );
    cancellation_mcp.finish();

    let expected: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/mcp/batch-process-outcomes-v1.json"
    ))
    .expect("batch process outcome golden is valid JSON");
    assert_eq!(
        Value::Object(observed),
        expected["cases"],
        "public process batch outcomes drifted"
    );

    daemon.finish();
}

fn process_outcome_snapshot(response: &Value, repository_id: &str) -> Value {
    let content = &response["result"]["structuredContent"];
    let encoded = serde_json::to_vec(content).expect("structured batch response serializes");
    let operation_results = content["data"]["operation_results"]
        .as_array()
        .expect("batch response contains ordered operation results")
        .iter()
        .map(|result| {
            json!({
                "id": result["id"],
                "tool": result["tool"],
                "status": result["status"],
                "error_code": result
                    .get("error")
                    .map_or(Value::Null, |error| error["code"].clone()),
                "truncated": result["truncated"]
            })
        })
        .collect::<Vec<_>>();
    let limiting_resources = content["completeness"]["limiting_resources"]
        .as_array()
        .expect("batch completeness has limiting resources")
        .iter()
        .map(|resource| resource["kind"].clone())
        .collect::<Vec<_>>();
    json!({
        "repository_preserved":
            content["repository"]["repository_id"] == Value::String(repository_id.to_owned()),
        "generation_preserved":
            content["data"]["generation_id"] == content["generation"]["generation_id"],
        "batch_status": content["data"]["batch_status"],
        "operation_results": operation_results,
        "truncated": content["truncated"],
        "completeness": {
            "state": content["completeness"]["state"],
            "limiting_resources": limiting_resources
        },
        "warnings": content["warnings"]
            .as_array()
            .map_or(0, Vec::len),
        "usage": {
            "json_bytes_exact": content["usage"]["json_bytes"] == json!(encoded.len()),
            "estimated_tokens_exact": content["usage"]["estimated_tokens"]
                == json!(rootlight_mcp_contract::accounting::estimate_tokens(encoded.len()))
        }
    })
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

fn assert_process_profile_semantics(mcp: &mut McpProcess, repository_id: &str) {
    let success = |id: &str| {
        json!({
            "id": id,
            "tool": "code.locate",
            "arguments": {"query": "__batch_absent__", "search_modes": ["exact"]}
        })
    };

    let mut semantic_outcomes = Vec::new();
    for profile in ["compact", "standard", "evidence"] {
        let response = mcp.call(
            &format!("batch-profile-{profile}"),
            "query.batch",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": "active",
                "operations": [success("first"), success("second")],
                "response_profile": profile
            }),
        );
        assert_success(&response, "query.batch");
        let content = &response["result"]["structuredContent"];
        semantic_outcomes.push(json!({
            "repository": content["repository"],
            "generation": content["generation"],
            "batch_status": content["data"]["batch_status"],
            "operation_results": content["data"]["operation_results"]
                .as_array()
                .expect("profiled batch has operation results")
                .iter()
                .map(|result| json!({
                    "id": result["id"],
                    "tool": result["tool"],
                    "status": result["status"]
                }))
                .collect::<Vec<_>>(),
        }));
    }
    assert!(
        semantic_outcomes
            .windows(2)
            .all(|window| window[0] == window[1]),
        "response profiles changed batch identity or terminal semantics: {semantic_outcomes:#?}"
    );
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

fn assert_standalone_batch_parity(standalone: &Value, batch: &Value, tool: &str) {
    assert_success(standalone, tool);
    assert_success(batch, "query.batch");
    let standalone = &standalone["result"]["structuredContent"];
    let batch = &batch["result"]["structuredContent"];
    let operation = &batch["data"]["operation_results"][0];

    assert_eq!(batch["data"]["batch_status"], "ok");
    assert_eq!(operation["tool"], tool);
    assert_eq!(operation["status"], "ok");
    assert!(operation.get("error").is_none());
    assert_eq!(operation["data"], standalone["data"]);
    assert_eq!(operation["truncated"], standalone["truncated"]);
    assert_eq!(operation["next_cursor"], standalone["next_cursor"]);

    assert_eq!(
        batch["repository"]["repository_id"],
        standalone["repository"]["repository_id"]
    );
    assert_eq!(batch["generation"], standalone["generation"]);
    assert_eq!(
        batch["data"]["generation_id"],
        standalone["generation"]["generation_id"]
    );
    assert_eq!(batch["trust"], standalone["trust"]);
    assert_eq!(batch["truncated"], standalone["truncated"]);
    assert_eq!(batch["completeness"], standalone["completeness"]);
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

fn build_batch_hook_mcp() -> PathBuf {
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let profile_dir = mcp_binary
        .parent()
        .expect("MCP binary has a profile directory");
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
            OsStr::new("rootlight-mcp"),
            OsStr::new("--bin"),
            OsStr::new("rootlight-mcp"),
            OsStr::new("--features"),
            OsStr::new("process-test-hooks"),
            OsStr::new("--target-dir"),
        ])
        .arg(target_dir)
        .output()
        .expect("batch hook MCP build starts");
    assert!(
        output.status.success(),
        "batch hook MCP build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let binary = profile_dir.join(format!("rootlight-mcp{}", std::env::consts::EXE_SUFFIX));
    assert!(binary.is_file(), "batch hook MCP binary is present");
    binary
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
        Self::spawn_with_binary(
            Path::new(env!("CARGO_BIN_EXE_rootlight-mcp")),
            transport_only,
            state_dir,
            runtime_dir,
            profile,
            false,
        )
    }

    fn spawn_with_binary(
        binary: &Path,
        transport_only: bool,
        state_dir: &Path,
        runtime_dir: &Path,
        profile: &str,
        cancel_batch_child: bool,
    ) -> Self {
        let mut command = Command::new(binary);
        if transport_only {
            command.arg("--transport-only");
        }
        if cancel_batch_child {
            command.env("ROOTLIGHT_PROCESS_TEST_BATCH_CANCEL", "1");
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
