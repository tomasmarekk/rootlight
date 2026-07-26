//! Production-process evidence for the five public graph-analysis tools.
//!
//! The matrix crosses the real MCP stdio boundary and a supervised daemon.

mod process_support;

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    thread::{self, JoinHandle},
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
    let fixture = process_support::private_process_tempdir("rl-graph-");
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
    let dynamic_dispatch_symbol = locate_symbol(&mut mcp, &index, "dynamic_dispatch_probe");
    let descriptions = tool_descriptions(&mut mcp);
    assert_graph_descriptions_are_bounded(&descriptions);

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

    assert_deterministic_graph_outputs(&mut mcp, &index, &symbol_id, &outputs);
    assert_standalone_batch_parity(&mut mcp, &index, &symbol_id, &outputs);
    assert_context_provider_parity(&mut mcp, &index, &symbol_id);
    assert_relationship_pagination(&mut mcp, &index, &symbol_id);
    assert_five_tool_capability_matrix(&outputs);

    let relationships = &outputs["symbol.relationships"];
    assert!(relationships["data"]["groups"].is_array());
    assert_eq!(relationships["data"]["totals"]["exact"], true);

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
            !candidate["why"]
                .as_array()
                .expect("a dead-code observation explains its basis")
                .is_empty(),
            "dead-code observation lacks a negative rationale: {candidate:#}"
        );
        assert!(
            candidate["classification"]
                .as_str()
                .expect("a dead-code observation is classified")
                .starts_with("not_observed")
                || candidate["classification"] == "no_observed_incoming_references"
        );
    }
    assert!(
        descriptions["code.dead"]
            .as_str()
            .expect("code.dead has a description")
            .contains("do not prove runtime liveness")
    );
    assert_dynamic_dispatch_remains_inconclusive(&mut mcp, &index, &dynamic_dispatch_symbol);

    assert_profile_matrix(&mut mcp, &index, &symbol_id, &outputs);
    assert_hard_token_budget_taxonomy(&mut mcp, &index, &symbol_id);
    assert_truncated_negative_analyses_are_caveated(&mut mcp, &index, &symbol_id);
    assert_flow_truncation_is_explicit(&mut mcp, &index, &symbol_id);

    let negative_root = fixture.path().join("negative-repository");
    write_negative_repository_fixture(&negative_root);
    let negative = index_repository(&mut mcp, &negative_root);
    assert_safe_negative_analyses(&mut mcp, &negative);

    mcp.finish();
    daemon.finish();
}

fn assert_standalone_batch_parity(
    mcp: &mut McpProcess,
    index: &IndexReceipt,
    symbol_id: &str,
    standalone_outputs: &Map<String, Value>,
) {
    for (tool_index, (tool, arguments)) in
        graph_tool_calls(index, symbol_id).into_iter().enumerate()
    {
        let mut operation_arguments = arguments
            .as_object()
            .expect("graph arguments are objects")
            .clone();
        operation_arguments.remove("repository");
        operation_arguments.remove("generation");
        let response = mcp.call(
            &format!("graph-batch-parity-{tool_index}"),
            "query.batch",
            json!({
                "repository": {"repository_id": index.repository_id},
                "generation": index.generation_id,
                "operations": [{
                    "id": format!("graph_{tool_index}"),
                    "tool": tool,
                    "arguments": operation_arguments
                }]
            }),
        );
        assert_success(&response, "query.batch");
        let batch = &response["result"]["structuredContent"];
        let operation = &batch["data"]["operation_results"][0];
        let standalone = &standalone_outputs[tool];
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
        assert_eq!(batch["trust"], standalone["trust"]);
        assert_eq!(batch["truncated"], standalone["truncated"]);
        for field in ["state", "limiting_resources", "continuation"] {
            assert_eq!(
                batch["completeness"][field], standalone["completeness"][field],
                "{tool} changed completeness.{field} through query.batch"
            );
        }
        let batch_guidance = batch["completeness"]["guidance"]
            .as_array()
            .expect("batch completeness returns guidance");
        for guidance in standalone["completeness"]["guidance"]
            .as_array()
            .expect("standalone completeness returns guidance")
        {
            assert!(
                batch_guidance.contains(guidance),
                "query.batch dropped {tool} guidance {guidance}"
            );
        }
    }
}

fn assert_context_provider_parity(mcp: &mut McpProcess, index: &IndexReceipt, symbol_id: &str) {
    let relationships = mcp.call(
        "graph-context-relationships-standalone",
        "symbol.relationships",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "symbol_ids": [symbol_id],
            "relations": ["calls", "references"],
            "max_results": 8,
            "min_confidence": 0
        }),
    );
    assert_success(&relationships, "symbol.relationships");
    let relationships = &relationships["result"]["structuredContent"];
    let overview = mcp.call(
        "graph-context-architecture-standalone",
        "architecture.overview",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "views": ["hotspots"],
            "max_components": 4,
            "include_edges": true,
            "min_confidence": 0
        }),
    );
    assert_success(&overview, "architecture.overview");
    let overview = &overview["result"]["structuredContent"];
    let context = mcp.call(
        "graph-context-provider-parity",
        "context.pack",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "task": "explain callers and architecture for the selected symbol",
            "seeds": {"symbols": [symbol_id]},
            "token_budget": 20_000,
            "source_policy": "references_only",
            "sections": ["definitions", "callers", "architecture"],
            "min_confidence": 0,
            "response_profile": "evidence"
        }),
    );
    assert_success(&context, "context.pack");
    let context = &context["result"]["structuredContent"];
    assert_eq!(
        context["repository"]["repository_id"], index.repository_id,
        "context.pack changed repository identity"
    );
    assert_eq!(
        context["generation"]["generation_id"], index.generation_id,
        "context.pack changed generation identity"
    );
    assert_eq!(context["trust"], "untrusted_repository_data");

    let caller_items = context_items_for_role(context, "caller");
    let expected_callers = relationship_targets(relationships);
    assert_eq!(
        caller_items, expected_callers,
        "the relationships context provider diverged from symbol.relationships"
    );

    let architecture_scores = context_items_for_role(context, "architecture")
        .into_iter()
        .map(|(_, score)| score)
        .collect::<Vec<_>>();
    let expected_scores = overview["data"]["components"]
        .as_array()
        .expect("architecture.overview returns components")
        .iter()
        .map(|component| {
            component["confidence"]
                .as_u64()
                .expect("an architecture component has confidence")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        architecture_scores, expected_scores,
        "the architecture context provider diverged from architecture.overview; \
         standalone usage={:#}, context={context:#}",
        overview["usage"]
    );
}

fn context_items_for_role(context: &Value, role: &str) -> Vec<(Option<String>, u64)> {
    context["data"]["items"]
        .as_array()
        .expect("context.pack returns items")
        .iter()
        .filter(|item| item["role"] == role)
        .map(|item| {
            (
                item["symbol_id"].as_str().map(str::to_owned),
                item["score"].as_u64().expect("a context item has a score"),
            )
        })
        .collect()
}

fn relationship_targets(output: &Value) -> Vec<(Option<String>, u64)> {
    output["data"]["groups"]
        .as_array()
        .expect("symbol.relationships returns groups")
        .iter()
        .flat_map(|group| {
            group["items"]
                .as_array()
                .expect("a relationship group returns items")
        })
        .map(|item| {
            (
                item["symbol_id"].as_str().map(str::to_owned),
                item["confidence"]
                    .as_u64()
                    .expect("a relationship item has confidence"),
            )
        })
        .collect()
}

fn assert_relationship_pagination(mcp: &mut McpProcess, index: &IndexReceipt, symbol_id: &str) {
    let unpaged = mcp.call(
        "graph-relationships-unpaged",
        "symbol.relationships",
        relationship_arguments(index, symbol_id, 100, None),
    );
    assert_success(&unpaged, "symbol.relationships");
    let output = &unpaged["result"]["structuredContent"];
    let expected = relationship_records(output);
    if expected.is_empty() {
        // The production first-slice parser records containment and dispatch
        // candidates, not the served semantic relationship families. This
        // path proves that missing Tier B facts stay exact and non-pageable;
        // authenticated multi-page concatenation is covered by the executor's
        // semantic-port fixture.
        assert_eq!(output["data"]["totals"]["exact"], true);
        assert_eq!(output["data"]["totals"]["total_edges"], 0);
        assert_eq!(output["truncated"], false);
        assert!(output["next_cursor"].is_null());
        assert_eq!(output["completeness"]["continuation"], "not_applicable");
        assert_ne!(output["coverage"]["status"], "complete");
        assert!(
            output["coverage"]["skipped_inputs"]
                .as_u64()
                .is_some_and(|skipped| skipped > 0)
        );
        assert!(
            output["warnings"]
                .as_array()
                .expect("bounded relationship coverage returns warnings")
                .iter()
                .any(|warning| warning["code"] == "negative_claims_inconclusive")
        );
        return;
    }

    let first = collect_relationship_pages(mcp, index, symbol_id, "first");
    let second = collect_relationship_pages(mcp, index, symbol_id, "second");
    assert_eq!(first, expected);
    assert_eq!(second, expected);
}

fn collect_relationship_pages(
    mcp: &mut McpProcess,
    index: &IndexReceipt,
    symbol_id: &str,
    run: &str,
) -> Vec<Value> {
    let mut records = Vec::new();
    let mut cursor = None;
    for page in 0..32 {
        let response = mcp.call(
            &format!("graph-relationships-page-{run}-{page}"),
            "symbol.relationships",
            relationship_arguments(index, symbol_id, 1, cursor.as_deref()),
        );
        assert_success(&response, "symbol.relationships");
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract("symbol.relationships", output, index);
        records.extend(relationship_records(output));
        let Some(next_cursor) = output["next_cursor"].as_str() else {
            assert_eq!(output["truncated"], false);
            assert_eq!(output["completeness"]["continuation"], "not_applicable");
            let unique = records
                .iter()
                .map(|record| {
                    serde_json::to_string(record).expect("relationship record serializes")
                })
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(
                unique.len(),
                records.len(),
                "relationship pages contain a duplicate record"
            );
            return records;
        };
        assert_eq!(output["truncated"], true);
        assert_eq!(output["completeness"]["continuation"], "available");
        if page == 0 {
            let mut tampered = next_cursor.as_bytes().to_vec();
            let last = tampered
                .last_mut()
                .expect("an authenticated cursor has an encoded payload");
            *last = if *last == b'A' { b'B' } else { b'A' };
            let tampered = String::from_utf8(tampered).expect("base64url cursor stays UTF-8");
            let rejected = mcp.call(
                &format!("graph-relationships-tampered-{run}"),
                "symbol.relationships",
                relationship_arguments(index, symbol_id, 1, Some(&tampered)),
            );
            assert_public_error(&rejected, "INVALID_CURSOR");
        }
        cursor = Some(next_cursor.to_owned());
    }
    panic!("relationship pagination did not terminate within the bounded page count");
}

fn relationship_arguments(
    index: &IndexReceipt,
    symbol_id: &str,
    max_results: u16,
    cursor: Option<&str>,
) -> Value {
    let mut arguments = json!({
        "repository": {"repository_id": index.repository_id},
        "generation": index.generation_id,
        "symbol_ids": [symbol_id],
        "relations": ["calls", "references"],
        "direction": "outbound",
        "max_results": max_results
    });
    if let Some(cursor) = cursor {
        arguments["cursor"] = json!(cursor);
    }
    arguments
}

fn relationship_records(output: &Value) -> Vec<Value> {
    output["data"]["groups"]
        .as_array()
        .expect("symbol.relationships returns groups")
        .iter()
        .flat_map(|group| {
            group["items"]
                .as_array()
                .expect("a relationship group returns items")
                .iter()
                .map(|item| {
                    json!({
                        "seed": group["seed"],
                        "relation": group["relation"],
                        "direction": group["direction"],
                        "item": item
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn assert_dynamic_dispatch_remains_inconclusive(
    mcp: &mut McpProcess,
    index: &IndexReceipt,
    dynamic_symbol: &str,
) {
    let response = mcp.call(
        "graph-dynamic-dispatch",
        "code.dead",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "entry_point_policy": "standard",
            "include_exported": false,
            "include_tests": true,
            "max_candidates": 100
        }),
    );
    assert_success(&response, "code.dead");
    let output = &response["result"]["structuredContent"];
    let blind_spots = output["data"]["blind_spots"]
        .as_array()
        .expect("code.dead returns blind spots");
    assert!(
        blind_spots
            .iter()
            .any(|blind_spot| blind_spot["category"] == "dynamic_dispatch")
    );
    if let Some(candidate) = output["data"]["candidates"]
        .as_array()
        .expect("code.dead returns candidates")
        .iter()
        .find(|candidate| candidate["symbol_id"] == dynamic_symbol)
    {
        assert!(
            candidate["classification"]
                .as_str()
                .is_some_and(|classification| {
                    classification.starts_with("not_observed")
                        || classification == "no_observed_incoming_references"
                }),
            "dynamic-dispatch reachability must remain an observation, not a proof"
        );
    }
    assert_eq!(output["data"]["entry_points"]["complete"], false);
}

fn assert_five_tool_capability_matrix(outputs: &Map<String, Value>) {
    let expected_data_fields = [
        ("symbol.relationships", &["groups", "totals"][..]),
        ("flow.trace", &["paths", "frontier", "projection"][..]),
        (
            "architecture.overview",
            &["components", "connections", "hotspots", "views"][..],
        ),
        (
            "architecture.cycles",
            &["components", "cycles", "break_candidates"][..],
        ),
        (
            "code.dead",
            &[
                "candidates",
                "entry_points",
                "blind_spots",
                "false_positive_controls",
            ][..],
        ),
    ];
    for (tool, fields) in expected_data_fields {
        let output = &outputs[tool];
        for field in fields {
            assert!(
                output["data"].get(*field).is_some(),
                "{tool} omitted capability field {field}"
            );
        }
        assert_ne!(output["coverage"]["status"], "unknown");
        assert!(
            output["coverage"]["languages"]
                .as_array()
                .is_some_and(
                    |languages| languages
                        .iter()
                        .all(|language| language["language"].is_string()
                            && language["tier"].is_string())
                )
        );
        assert!(output["coverage"]["skipped_inputs"].as_u64().is_some());
        assert!(output["completeness"]["state"].is_string());
    }
}

fn assert_deterministic_graph_outputs(
    mcp: &mut McpProcess,
    index: &IndexReceipt,
    symbol_id: &str,
    first_outputs: &Map<String, Value>,
) {
    for (tool_index, (tool, arguments)) in
        graph_tool_calls(index, symbol_id).into_iter().enumerate()
    {
        let repeated = mcp.call(&format!("graph-determinism-{tool_index}"), tool, arguments);
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
                first_outputs[tool][field], repeated[field],
                "{tool} changed deterministic field {field}"
            );
        }
    }
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

fn assert_flow_truncation_is_explicit(mcp: &mut McpProcess, index: &IndexReceipt, symbol_id: &str) {
    let response = mcp.call(
        "graph-flow-small-traversal",
        "flow.trace",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "from": {"symbol_id": symbol_id},
            "relations": ["calls"],
            "direction": "outbound",
            "max_depth": 8,
            "max_paths": 100,
            "budget": {"max_traversal_facts": 1}
        }),
    );
    assert_success(&response, "flow.trace");
    let output = &response["result"]["structuredContent"];
    assert_common_read_contract("flow.trace", output, index);
    assert_eq!(output["truncated"], true);
    assert_eq!(output["completeness"]["state"], "truncated");
    assert!(
        output["completeness"]["limiting_resources"]
            .as_array()
            .expect("truncated flow names limiting resources")
            .iter()
            .any(|resource| resource["kind"] == "edges")
    );
    assert!(
        !output["completeness"]["guidance"]
            .as_array()
            .expect("truncated flow provides guidance")
            .is_empty()
    );
}

fn assert_safe_negative_analyses(mcp: &mut McpProcess, index: &IndexReceipt) {
    let cycles = mcp.call(
        "graph-negative-cycles",
        "architecture.cycles",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "projection": {"relations": ["calls"], "level": "symbol"},
            "max_cycles": 20
        }),
    );
    assert_success(&cycles, "architecture.cycles");
    let cycles = &cycles["result"]["structuredContent"];
    assert_common_read_contract("architecture.cycles", cycles, index);
    assert_eq!(cycles["data"]["cycles"], json!([]));
    assert_eq!(cycles["truncated"], false);
    assert_ne!(cycles["coverage"]["status"], "unknown");

    let dead = mcp.call(
        "graph-negative-dead",
        "code.dead",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "entry_point_policy": "standard",
            "include_exported": true,
            "include_tests": true,
            "max_candidates": 20
        }),
    );
    assert_success(&dead, "code.dead");
    let dead = &dead["result"]["structuredContent"];
    assert_common_read_contract("code.dead", dead, index);
    assert_eq!(dead["truncated"], false);
    assert_eq!(dead["data"]["entry_points"]["complete"], false);
    assert!(
        !dead["data"]["blind_spots"]
            .as_array()
            .expect("negative dead-code analysis retains blind spots")
            .is_empty()
    );
    for candidate in dead["data"]["candidates"]
        .as_array()
        .expect("negative dead-code analysis returns candidates")
    {
        assert!(
            candidate["classification"]
                .as_str()
                .is_some_and(|classification| {
                    classification.starts_with("not_observed")
                        || classification == "no_observed_incoming_references"
                })
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
    fs::create_dir_all(root.join("generated")).expect("fixture generated directory is created");
    fs::create_dir_all(root.join("tests")).expect("fixture test directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"graph_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    fs::write(
        root.join("src/lib.rs"),
        concat!(
            "mod alpha;\n",
            "mod beta;\n",
            "pub fn matrix_entry() -> usize { alpha::alpha() + beta::beta() }\n",
            "pub fn cycle_left() -> usize { cycle_right() }\n",
            "pub fn cycle_right() -> usize { cycle_left() }\n",
            "pub fn dag_root() -> usize { dag_branch() }\n",
            "pub fn dag_branch() -> usize { dag_leaf() }\n",
            "pub fn dag_leaf() -> usize { 3 }\n",
            "pub fn disconnected_probe() -> usize { 5 }\n",
            "pub trait DynamicProbe { fn invoke(&self) -> usize; }\n",
            "pub fn dynamic_dispatch_probe(value: &dyn DynamicProbe) -> usize {\n",
            "    value.invoke()\n",
            "}\n",
        ),
    )
    .expect("fixture root source is written");
    fs::write(
        root.join("src/alpha.rs"),
        "pub fn alpha() -> usize { super::dag_root() }\n",
    )
    .expect("first fixture module is written");
    fs::write(
        root.join("src/beta.rs"),
        "pub fn beta() -> usize { super::dag_leaf() }\n",
    )
    .expect("second fixture module is written");
    fs::write(
        root.join("generated/bindings.rs"),
        "pub fn generated_probe() -> usize { 8 }\n",
    )
    .expect("generated fixture source is written");
    fs::write(
        root.join("tests/integration.rs"),
        "#[test]\nfn integration_probe() { assert_eq!(2 + 2, 4); }\n",
    )
    .expect("test fixture source is written");
    fs::write(
        root.join("src/partial_language.kt"),
        "fun unsupportedProbe(): Int = 13\n",
    )
    .expect("partial-language fixture source is written");
}

fn write_negative_repository_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("negative fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"graph_negative_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("negative fixture manifest is written");
    fs::write(
        root.join("src/lib.rs"),
        concat!(
            "pub fn negative_entry() -> usize { negative_branch() }\n",
            "fn negative_branch() -> usize { negative_leaf() }\n",
            "fn negative_leaf() -> usize { 1 }\n",
        ),
    )
    .expect("negative fixture source is written");
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
    stderr_reader: Option<JoinHandle<String>>,
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
        let mut stderr = child.stderr.take().expect("MCP stderr is piped");
        let stderr_reader = thread::spawn(move || {
            let mut output = String::new();
            stderr
                .read_to_string(&mut output)
                .expect("MCP stderr reads");
            output
        });
        let mut process = Self {
            input: child.stdin.take(),
            output: child.stdout.take().map(BufReader::new),
            stderr_reader: Some(stderr_reader),
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
        let stderr = self
            .stderr_reader
            .take()
            .expect("MCP stderr reader is retained")
            .join()
            .expect("MCP stderr reader thread joins");
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
        if let Some(stderr_reader) = self.stderr_reader.take() {
            let _ = stderr_reader.join();
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
