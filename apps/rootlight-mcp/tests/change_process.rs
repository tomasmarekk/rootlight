//! Production-process evidence for change, test, history, and planning tools.
//!
//! The matrix crosses MCP stdio and the supervised daemon while keeping
//! unsupported Git and selector dimensions in preflight.

mod process_support;

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use rootlight_ids::{GenerationId, RepositoryId, SymbolId};
use serde_json::{Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const CHANGE_TOOLS: [&str; 4] = [
    "change.impact",
    "tests.select",
    "history.compare",
    "plan.change",
];

#[test]
fn change_tools_preserve_truthful_contracts_across_processes() {
    let fixture = process_support::private_process_tempdir("rl-change-");
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    assert_unsupported_selectors_are_rejected_before_daemon(&state_dir, &runtime_dir);

    let repository_root = fixture.path().join("repository");
    write_repository_revision(&repository_root, false);
    let daemon_binary = ensure_daemon_binary();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(false, &state_dir, &runtime_dir);

    let first = index_repository(&mut mcp, &repository_root, "first");
    write_repository_revision(&repository_root, true);
    let second = index_repository(&mut mcp, &repository_root, "second");
    assert_eq!(
        first.repository_id, second.repository_id,
        "reindexing the same root preserves repository identity"
    );
    assert_ne!(
        first.generation_id, second.generation_id,
        "the changed repository publishes a distinct generation"
    );
    let symbol_id = locate_symbol(&mut mcp, &second, "change_target");

    let descriptions = tool_descriptions(&mut mcp);
    for tool in CHANGE_TOOLS {
        let description = descriptions
            .iter()
            .find(|candidate| candidate["name"] == tool)
            .and_then(|candidate| candidate["description"].as_str())
            .unwrap_or_else(|| panic!("{tool} is absent from tools/list"));
        assert!(
            description.contains("unsupported"),
            "{tool} must disclose unsupported dimensions"
        );
    }

    let calls = change_tool_calls(&first, &second, &symbol_id);
    for (call_index, (tool, arguments)) in calls.iter().enumerate() {
        let first_response = mcp.call(
            &format!("change-{call_index}-first"),
            tool,
            arguments.clone(),
        );
        assert_success(&first_response, tool);
        let output = &first_response["result"]["structuredContent"];
        assert_common_read_contract(tool, output, &second);
        assert_tool_data(tool, output, &first, &second);

        let repeated = mcp.call(
            &format!("change-{call_index}-repeat"),
            tool,
            arguments.clone(),
        );
        assert_success(&repeated, tool);
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
                output[field], repeated[field],
                "{tool} changed deterministic field {field}"
            );
        }
    }

    assert_supported_profiles(&mut mcp, &first, &second, &symbol_id);
    assert_hard_budgets_are_public(&mut mcp, &first, &second, &symbol_id);
    assert_plan_objectives_are_served(&mut mcp, &second, &symbol_id);
    assert_history_absence_is_distinct_from_empty(&mut mcp, &second);

    mcp.finish();
    daemon.finish();
}

fn change_tool_calls(
    first: &IndexReceipt,
    second: &IndexReceipt,
    symbol_id: &str,
) -> [(&'static str, Value); 4] {
    let repository = || json!({"repository_id": second.repository_id});
    let generation = || Value::String(second.generation_id.clone());
    [
        (
            "change.impact",
            json!({
                "repository": repository(),
                "generation": generation(),
                "change": {
                    "symbol_ids": [symbol_id],
                    "paths": ["src/lib.rs"]
                },
                "relation_policy": "standard",
                "max_depth": 3,
                "include_tests": true,
                "min_confidence": 0
            }),
        ),
        (
            "tests.select",
            json!({
                "repository": repository(),
                "generation": generation(),
                "seeds": {"symbols": [symbol_id]},
                "test_kinds": ["unit"],
                "max_tests": 20,
                "include_commands": true
            }),
        ),
        (
            "history.compare",
            json!({
                "repository": repository(),
                "base": first.generation_id,
                "head": second.generation_id,
                "change_kinds": ["entities", "signatures"],
                "max_results": 100
            }),
        ),
        (
            "plan.change",
            json!({
                "repository": repository(),
                "generation": generation(),
                "objective": "bug_fix",
                "objective_text": "preserve the public result while correcting the helper",
                "targets": [{"symbol_id": symbol_id}],
                "max_steps": 12
            }),
        ),
    ]
}

fn assert_tool_data(tool: &str, output: &Value, first: &IndexReceipt, second: &IndexReceipt) {
    match tool {
        "change.impact" => {
            assert!(
                !output["data"]["resolved_changes"]
                    .as_array()
                    .expect("change.impact returns resolved changes")
                    .is_empty()
            );
            assert!(output["data"]["impacted"].is_array());
            assert!(output["data"]["tests"].is_array());
            assert!(output["data"]["risk_summary"]["coverage"].is_string());
        }
        "tests.select" => {
            assert!(output["data"]["tests"].is_array());
            assert!(output["data"]["gaps"].is_array());
            for signal in [
                "direct_edges",
                "transitive_signals",
                "history_signals",
                "file_colocation_signals",
            ] {
                assert!(
                    output["data"]["coverage_strategy"][signal]
                        .as_bool()
                        .is_some(),
                    "tests.select omitted coverage signal {signal}"
                );
            }
            for selected in output["data"]["tests"]
                .as_array()
                .expect("tests.select returns ranked tests")
            {
                assert!(
                    !selected["why"]
                        .as_array()
                        .expect("a selected test explains its evidence")
                        .is_empty()
                );
                assert!(selected["score"].as_u64().is_some());
            }
            assert!(
                !output["data"]["tests"]
                    .as_array()
                    .expect("tests.select returns tests")
                    .is_empty()
                    || !output["data"]["gaps"]
                        .as_array()
                        .expect("tests.select returns gaps")
                        .is_empty(),
                "test selection must return evidence or an explicit coverage gap"
            );
        }
        "history.compare" => {
            assert_eq!(
                output["data"]["matched_states"]["base_generation"],
                first.generation_id
            );
            assert_eq!(
                output["data"]["matched_states"]["head_generation"],
                second.generation_id
            );
            assert!(
                !output["data"]["changes"]
                    .as_array()
                    .expect("history.compare returns semantic changes")
                    .is_empty(),
                "the added fixture function must not compare as an empty delta"
            );
            assert!(output["data"]["matched_states"]["coverage"].is_string());
            assert!(output["data"]["lineage"].is_array());
        }
        "plan.change" => {
            let plan = output["data"]["plan"]
                .as_array()
                .expect("plan.change returns an ordered plan");
            assert!(!plan.is_empty());
            for (index, step) in plan.iter().enumerate() {
                assert_eq!(
                    step["step"].as_u64(),
                    u64::try_from(index + 1).ok(),
                    "plan.change step ordinals are contiguous"
                );
                assert!(step["depends_on"].is_array());
                assert!(step["risks"].is_array());
                assert!(
                    step["rationale"]
                        .as_str()
                        .is_some_and(|rationale| !rationale.is_empty())
                );
                assert!(step["evidence_refs"].is_array());
            }
            let provider_coverage = output["data"]["provider_coverage"]
                .as_array()
                .expect("plan.change reports every evidence provider");
            assert_eq!(provider_coverage.len(), 7);
            assert_eq!(
                provider_coverage
                    .iter()
                    .map(|coverage| coverage["provider"].as_str())
                    .collect::<Vec<_>>(),
                vec![
                    Some("change_impact"),
                    Some("relationships"),
                    Some("tests"),
                    Some("architecture"),
                    Some("history"),
                    Some("source"),
                    Some("ownership"),
                ]
            );
            for provider in ["history", "source", "ownership"] {
                let coverage = provider_coverage
                    .iter()
                    .find(|coverage| coverage["provider"] == provider)
                    .expect("bounded unsupported provider is present");
                assert_eq!(coverage["state"], "unsupported");
                assert!(coverage["omission"]["reason"].is_string());
            }
            assert!(output["data"]["affected_scope"]["affected_symbols"].is_number());
            assert!(output["data"]["test_plan"].is_array());
            assert!(output["data"]["open_decisions"].is_array());
            assert!(output["data"]["context_pack_request"].is_object());
        }
        _ => panic!("unexpected change tool {tool}"),
    }
}

fn assert_supported_profiles(
    mcp: &mut McpProcess,
    first: &IndexReceipt,
    second: &IndexReceipt,
    symbol_id: &str,
) {
    for profile in ["compact", "standard", "evidence"] {
        for (case_index, (tool, mut arguments)) in change_tool_calls(first, second, symbol_id)
            .into_iter()
            .enumerate()
        {
            if tool == "history.compare" && profile != "compact" {
                continue;
            }
            arguments["profile"] = json!(profile);
            let response = mcp.call(
                &format!("change-profile-{profile}-{case_index}"),
                tool,
                arguments,
            );
            assert_success(&response, tool);
            let output = &response["result"]["structuredContent"];
            assert_common_read_contract(tool, output, second);
            assert_exact_json_usage(tool, output);
        }
    }
}

fn assert_hard_budgets_are_public(
    mcp: &mut McpProcess,
    first: &IndexReceipt,
    second: &IndexReceipt,
    symbol_id: &str,
) {
    for (case_index, (tool, mut arguments)) in change_tool_calls(first, second, symbol_id)
        .into_iter()
        .enumerate()
    {
        arguments["budget"] = json!({"max_tokens": 100});
        if matches!(tool, "change.impact" | "tests.select") {
            arguments["profile"] = json!("evidence");
        }
        let response = mcp.call(&format!("change-budget-{case_index}"), tool, arguments);
        assert_public_error(&response, "BUDGET_EXCEEDED");
        if tool == "plan.change" {
            assert!(
                response.get("error").is_none(),
                "plan.change budget exhaustion must remain a checked tool error, not JSON-RPC -32603: {response:#}"
            );
        }
    }
}

fn assert_plan_objectives_are_served(mcp: &mut McpProcess, second: &IndexReceipt, symbol_id: &str) {
    for objective in ["bug_fix", "refactor", "explanation", "migration", "review"] {
        let response = mcp.call(
            &format!("change-plan-objective-{objective}"),
            "plan.change",
            json!({
                "repository": {"repository_id": second.repository_id},
                "generation": second.generation_id,
                "objective": objective,
                "objective_text": format!("prepare a bounded {objective} plan"),
                "targets": [{"symbol_id": symbol_id}],
                "max_steps": 12
            }),
        );
        assert_success(&response, "plan.change");
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract("plan.change", output, second);
        assert!(
            !output["data"]["plan"]
                .as_array()
                .expect("plan.change returns ordered steps")
                .is_empty()
        );
    }
}

fn assert_history_absence_is_distinct_from_empty(mcp: &mut McpProcess, second: &IndexReceipt) {
    let empty = mcp.call(
        "change-empty-history",
        "history.compare",
        json!({
            "repository": {"repository_id": second.repository_id},
            "base": second.generation_id,
            "head": second.generation_id,
            "change_kinds": ["entities", "signatures"],
            "max_results": 20
        }),
    );
    assert_success(&empty, "history.compare");
    let empty = &empty["result"]["structuredContent"];
    assert_common_read_contract("history.compare", empty, second);
    assert_eq!(empty["data"]["changes"], json!([]));
    assert_eq!(empty["truncated"], false);

    let response = mcp.call(
        "change-missing-history",
        "history.compare",
        json!({
            "repository": {"repository_id": second.repository_id},
            "base": GenerationId::from_bytes([91; 20]),
            "head": second.generation_id,
            "change_kinds": ["entities"],
            "max_results": 20
        }),
    );
    assert_public_error(&response, "STALE_GENERATION");
    assert!(
        response["result"]["structuredContent"]["data"].is_null(),
        "missing history must not serialize an empty successful delta"
    );
}

fn assert_unsupported_selectors_are_rejected_before_daemon(state_dir: &Path, runtime_dir: &Path) {
    let repository_id = RepositoryId::from_bytes([3; 16]);
    let symbol_id = SymbolId::from_bytes([7; 20]);
    let generation = GenerationId::from_bytes([11; 20]);
    let mut mcp = McpProcess::spawn(true, state_dir, runtime_dir);
    for (case, tool, arguments) in [
        (
            "change-working-tree",
            "change.impact",
            json!({
                "repository": {"repository_id": repository_id},
                "change": {"working_tree": "all"}
            }),
        ),
        (
            "change-revision-range",
            "change.impact",
            json!({
                "repository": {"repository_id": repository_id},
                "change": {"revision_range": "HEAD~1..HEAD"}
            }),
        ),
        (
            "change-scope",
            "change.impact",
            json!({
                "repository": {"repository_id": repository_id},
                "change": {"symbol_ids": [symbol_id]},
                "scope": {"paths": ["src"]}
            }),
        ),
        (
            "change-history-signals",
            "change.impact",
            json!({
                "repository": {"repository_id": repository_id},
                "change": {"symbol_ids": [symbol_id]},
                "include_history": true
            }),
        ),
        (
            "tests-path-seed",
            "tests.select",
            json!({
                "repository": {"repository_id": repository_id},
                "seeds": {"paths": ["src/lib.rs"]}
            }),
        ),
        (
            "tests-framework",
            "tests.select",
            json!({
                "repository": {"repository_id": repository_id},
                "seeds": {"symbols": [symbol_id]},
                "frameworks": ["cargo-nextest"]
            }),
        ),
        (
            "tests-integration-kind",
            "tests.select",
            json!({
                "repository": {"repository_id": repository_id},
                "seeds": {"symbols": [symbol_id]},
                "test_kinds": ["integration"]
            }),
        ),
        (
            "history-git-base",
            "history.compare",
            json!({
                "repository": {"repository_id": repository_id},
                "base": {"git": "main"},
                "head": generation
            }),
        ),
        (
            "history-relations",
            "history.compare",
            json!({
                "repository": {"repository_id": repository_id},
                "base": generation,
                "head": generation,
                "change_kinds": ["relations"]
            }),
        ),
        (
            "history-expanded-profile",
            "history.compare",
            json!({
                "repository": {"repository_id": repository_id},
                "base": generation,
                "head": generation,
                "profile": "evidence"
            }),
        ),
        (
            "plan-constraints",
            "plan.change",
            json!({
                "repository": {"repository_id": repository_id},
                "objective": "bug_fix",
                "objective_text": "fixture",
                "targets": [{"symbol_id": symbol_id}],
                "constraints": ["preserve public behavior"]
            }),
        ),
        (
            "plan-change-context",
            "plan.change",
            json!({
                "repository": {"repository_id": repository_id},
                "objective": "bug_fix",
                "objective_text": "fixture",
                "targets": [{"symbol_id": symbol_id}],
                "change_context": {"symbol_ids": [symbol_id]}
            }),
        ),
    ] {
        let response = mcp.call(case, tool, arguments);
        assert_public_error(&response, "UNSUPPORTED_CAPABILITY");
    }
    mcp.finish();
}

fn write_repository_revision(root: &Path, changed: bool) {
    fs::create_dir_all(root.join("src")).expect("fixture source directory is created");
    fs::create_dir_all(root.join("tests")).expect("fixture test directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"change_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    let source = if changed {
        concat!(
            "pub fn change_target() -> usize { change_helper() }\n",
            "fn change_helper() -> usize { 2 }\n",
            "pub fn added_surface() -> usize { change_target() }\n",
        )
    } else {
        concat!(
            "pub fn change_target() -> usize { change_helper() }\n",
            "fn change_helper() -> usize { 1 }\n",
        )
    };
    fs::write(root.join("src/lib.rs"), source).expect("fixture source is written");
    fs::write(
        root.join("tests/regression.rs"),
        "#[test]\nfn change_target_regression() { assert_eq!(change_process_fixture::change_target(), 2); }\n",
    )
    .expect("fixture test is written");
}

fn index_repository(mcp: &mut McpProcess, root: &Path, case: &str) -> IndexReceipt {
    let arguments = json!({"root": root, "mode": "auto", "detached": false});
    let response = process_support::retry_transient_busy(&format!("index-{case}"), |attempt_id| {
        mcp.call(attempt_id, "repo.index", arguments.clone())
    });
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
            &format!("operation-{operation_id}-{attempt}"),
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
        "change-locate",
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

fn tool_descriptions(mcp: &mut McpProcess) -> Vec<Value> {
    let response = mcp.request("change-tools", "tools/list", json!({}));
    response["result"]["tools"]
        .as_array()
        .expect("tools/list returns tools")
        .clone()
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
    assert_eq!(
        output["truncated"],
        output["completeness"]["state"] == "truncated",
        "{tool} disagrees about truncation"
    );
}

fn assert_exact_json_usage(tool: &str, output: &Value) {
    let serialized = serde_json::to_vec(output).expect("structured output serializes");
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
        .expect("daemon build starts");
    assert!(
        output.status.success(),
        "daemon build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.is_file(), "daemon build did not produce {daemon:?}");
    daemon
}

#[derive(Debug)]
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
    output: BufReader<ChildStdout>,
}

impl McpProcess {
    fn spawn(transport_only: bool, state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"));
        command
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env("ROOTLIGHT_MCP_PROFILE", "developer")
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if transport_only {
            command.arg("--transport-only");
        }
        let mut child = command.spawn().expect("isolated MCP process starts");
        let input = child.stdin.take().expect("MCP stdin is piped");
        let output = BufReader::new(child.stdout.take().expect("MCP stdout is piped"));
        let mut process = Self {
            child: Some(child),
            input: Some(input),
            output,
        };
        let response = process.request(
            "initialize",
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "change-process", "version": "1.0"},
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
            .read_line(&mut line)
            .expect("MCP response reads");
        assert!(!line.is_empty(), "MCP process closed stdout");
        serde_json::from_str(&line).expect("MCP response is JSON")
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
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.input.take();
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
