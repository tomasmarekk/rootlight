//! Public retrieval-contract evidence across real MCP and daemon processes.
//!
//! The fixtures exercise only advertised retrieval capabilities and verify
//! stable rejection for schema-visible options outside the accepted surface.

mod process_support;

use std::{
    ffi::OsStr,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use rootlight_ids::SymbolId;
use rootlight_mcp_contract::accounting::estimate_tokens;
use serde_json::{Map, Value, json};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const RETRIEVAL_SOURCE: &str = "\
pub fn matrix_target_alpha(value: usize) -> usize {
    matrix_target_beta(value).saturating_add(1)
}

pub fn matrix_target_beta(value: usize) -> usize {
    matrix_target_gamma(value).saturating_mul(2)
}

pub fn matrix_target_gamma(value: usize) -> usize {
    value.saturating_sub(1)
}

pub fn matrix_target_delta(value: usize) -> usize {
    value.saturating_add(4)
}

pub fn matrix_target_epsilon(value: usize) -> usize {
    value.saturating_add(5)
}

pub fn matrix_target_zeta(value: usize) -> usize {
    value.saturating_add(6)
}

pub fn matrix_target_eta(value: usize) -> usize {
    value.saturating_add(7)
}

pub fn matrix_target_theta(value: usize) -> usize {
    value.saturating_add(8)
}

pub fn matrix_target_iota(value: usize) -> usize {
    value.saturating_add(9)
}

pub fn matrix_target_kappa(value: usize) -> usize {
    value.saturating_add(10)
}

pub fn matrix_target_lambda(value: usize) -> usize {
    value.saturating_add(11)
}

pub fn matrix_target_mu(value: usize) -> usize {
    value.saturating_add(12)
}
";

#[test]
fn retrieval_contract_matrix_crosses_real_process_boundaries() {
    let mut fixture = RetrievalFixture::spawn();
    supported_profiles_preserve_standalone_and_batch_semantics(&mut fixture);
    unsupported_retrieval_options_fail_with_stable_preflight_errors(&mut fixture);
    retrieval_limits_cursors_and_unresolved_ids_are_truthful(&mut fixture);
    fixture.finish();
}

fn supported_profiles_preserve_standalone_and_batch_semantics(fixture: &mut RetrievalFixture) {
    let mut locate_outputs = Map::new();
    let mut explain_outputs = Map::new();

    for profile in ["compact", "standard", "evidence"] {
        let locate_arguments = json!({
            "query": "matrix_target_alpha",
            "search_modes": ["exact"],
            "response_profile": profile
        });
        let standalone = fixture.standalone(
            &format!("locate-{profile}"),
            "code.locate",
            locate_arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("batch-locate-{profile}"),
            "code.locate",
            locate_arguments,
            profile,
        );
        assert_standalone_batch_parity(&standalone, &batch, "code.locate");
        locate_outputs.insert(
            profile.to_owned(),
            standalone["result"]["structuredContent"].clone(),
        );

        let explain_arguments = json!({
            "symbol_ids": [fixture.symbols[0].clone()],
            "include_provenance": "compact",
            "response_profile": profile
        });
        let standalone = fixture.standalone(
            &format!("explain-{profile}"),
            "symbol.explain",
            explain_arguments.clone(),
        );
        let batch = fixture.batch(
            &format!("batch-explain-{profile}"),
            "symbol.explain",
            explain_arguments,
            profile,
        );
        assert_standalone_batch_parity(&standalone, &batch, "symbol.explain");
        explain_outputs.insert(
            profile.to_owned(),
            standalone["result"]["structuredContent"].clone(),
        );
    }

    assert_profile_identity(&locate_outputs, "/data/matches/0");
    assert_profile_identity(&explain_outputs, "/data/symbols/0");

    let source_arguments = json!({
        "references": [{"source_ref": fixture.source_refs[0].clone()}],
        "include_line_numbers": true,
        "encoding": "utf8_lossless_when_valid",
        "response_profile": "compact"
    });
    let standalone_source =
        fixture.standalone("source-compact", "source.read", source_arguments.clone());
    let batch_source = fixture.batch(
        "batch-source-compact",
        "source.read",
        source_arguments,
        "compact",
    );
    assert_standalone_batch_parity(&standalone_source, &batch_source, "source.read");
    let source = &standalone_source["result"]["structuredContent"];
    assert_common_read_contract(source, &fixture.repository_id);
    assert_eq!(source["data"]["chunks"][0]["encoding"], "utf8");
    assert_eq!(
        source["data"]["chunks"][0]["trust"],
        "untrusted_repository_data"
    );
    assert!(
        source["data"]["chunks"][0]["content"]
            .as_str()
            .is_some_and(|content| content.contains("matrix_target_alpha"))
    );
    assert!(source["next_cursor"].is_null());
    assert_eq!(source["completeness"]["continuation"], "not_applicable");

    let no_provenance = fixture.standalone(
        "explain-no-provenance",
        "symbol.explain",
        json!({
            "symbol_ids": [fixture.symbols[0].clone()],
            "include_provenance": "none"
        }),
    );
    assert_success(&no_provenance, "symbol.explain");
    assert_eq!(
        no_provenance["result"]["structuredContent"]["data"]["symbols"][0]["provenance"],
        json!([])
    );

    let base64_source = fixture.standalone(
        "source-base64",
        "source.read",
        json!({
            "references": [{"source_ref": fixture.source_refs[0].clone()}],
            "encoding": "bytes_base64"
        }),
    );
    assert_success(&base64_source, "source.read");
    let base64_output = &base64_source["result"]["structuredContent"];
    assert_common_read_contract(base64_output, &fixture.repository_id);
    assert_eq!(base64_output["data"]["chunks"][0]["encoding"], "base64");
    assert!(
        base64_output["data"]["chunks"][0]["content"]
            .as_str()
            .is_some_and(|content| !content.is_empty() && content.len().is_multiple_of(4))
    );

    let contextual_source = fixture.standalone(
        "source-context",
        "source.read",
        json!({
            "references": [{"source_ref": fixture.source_refs[1].clone()}],
            "context_lines_before": 1,
            "context_lines_after": 1,
            "include_line_numbers": true
        }),
    );
    assert_success(&contextual_source, "source.read");
    let contextual_output = &contextual_source["result"]["structuredContent"];
    assert_common_read_contract(contextual_output, &fixture.repository_id);
    assert!(
        contextual_output["data"]["chunks"][0]["start_byte"]
            .as_u64()
            .zip(fixture.source_refs[1]["span"]["start_byte"].as_u64())
            .is_some_and(|(expanded, selected)| expanded < selected)
    );
    assert!(
        contextual_output["data"]["chunks"][0]["end_byte"]
            .as_u64()
            .zip(fixture.source_refs[1]["span"]["end_byte"].as_u64())
            .is_some_and(|(expanded, selected)| expanded > selected)
    );

    let mut overlapping_reference = fixture.source_refs[0].clone();
    let overlapping_start = overlapping_reference["span"]["start_byte"]
        .as_u64()
        .expect("fixture source start is an unsigned byte")
        .saturating_add(1);
    overlapping_reference["span"]["start_byte"] = json!(overlapping_start);
    let merged_source = fixture.standalone(
        "source-merge",
        "source.read",
        json!({
            "references": [
                {"source_ref": fixture.source_refs[0].clone()},
                {"source_ref": overlapping_reference}
            ],
            "merge_overlaps": true
        }),
    );
    assert_success(&merged_source, "source.read");
    assert_eq!(
        merged_source["result"]["structuredContent"]["data"]["chunks"]
            .as_array()
            .expect("overlapping reads return chunks")
            .len(),
        1
    );
}

fn unsupported_retrieval_options_fail_with_stable_preflight_errors(fixture: &mut RetrievalFixture) {
    let repository = || json!({"repository_id": fixture.repository_id});
    let source_ref = fixture.source_refs[0].clone();
    let symbol = fixture.symbols[0].clone();
    let cases = [
        (
            "locate-kinds",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "kinds": ["function"]}),
        ),
        (
            "locate-scope",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "scope": {"symbols": [symbol.clone()]}}),
        ),
        (
            "locate-languages",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "languages": ["rust"]}),
        ),
        (
            "locate-related",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "related_to": [symbol.clone()]}),
        ),
        (
            "locate-confidence",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "min_confidence": 700}),
        ),
        (
            "locate-docs",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["docs"]}),
        ),
        (
            "locate-path",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["path"]}),
        ),
        (
            "locate-semantic",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["semantic"]}),
        ),
        (
            "locate-structural",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["structural"]}),
        ),
        (
            "locate-mixed-modes",
            "code.locate",
            json!({"repository": repository(), "query": "matrix", "search_modes": ["exact", "lexical"]}),
        ),
        (
            "explain-sections",
            "symbol.explain",
            json!({"repository": repository(), "symbol_ids": [symbol.clone()], "sections": ["signature"]}),
        ),
        (
            "explain-relation-limit",
            "symbol.explain",
            json!({"repository": repository(), "symbol_ids": [symbol.clone()], "relation_sample_limit": 1}),
        ),
        (
            "explain-source-preview",
            "symbol.explain",
            json!({"repository": repository(), "symbol_ids": [symbol.clone()], "source_preview_lines": 1}),
        ),
        (
            "explain-full-provenance",
            "symbol.explain",
            json!({"repository": repository(), "symbol_ids": [symbol.clone()], "include_provenance": "full"}),
        ),
        (
            "source-symbol-selector",
            "source.read",
            json!({"repository": repository(), "references": [{"symbol_id": symbol.clone()}]}),
        ),
        (
            "source-file-selector",
            "source.read",
            json!({"repository": repository(), "references": [{
                "file_id": source_ref["span"]["file"].clone(),
                "start_byte": 0,
                "end_byte": 1
            }]}),
        ),
        (
            "source-standard",
            "source.read",
            json!({"repository": repository(), "references": [{"source_ref": source_ref.clone()}], "response_profile": "standard"}),
        ),
        (
            "source-evidence",
            "source.read",
            json!({"repository": repository(), "references": [{"source_ref": source_ref}], "response_profile": "evidence"}),
        ),
        (
            "source-byte-lines",
            "source.read",
            json!({
                "repository": repository(),
                "references": [{"source_ref": fixture.source_refs[0].clone()}],
                "encoding": "bytes_base64",
                "include_line_numbers": true
            }),
        ),
    ];

    for (id, tool, arguments) in cases {
        let response = fixture.mcp.call(id, tool, arguments);
        assert_public_error(&response, "UNSUPPORTED_CAPABILITY");
        let error = &response["result"]["structuredContent"]["error"];
        assert_eq!(error["message"], "requested capability is unavailable");
        assert_eq!(error["retryable"], false);
    }
}

fn retrieval_limits_cursors_and_unresolved_ids_are_truthful(fixture: &mut RetrievalFixture) {
    let first = fixture.standalone(
        "locate-page-one",
        "code.locate",
        json!({
            "query": "matrix_target",
            "search_modes": ["lexical"],
            "max_results": 1
        }),
    );
    let first_output = &first["result"]["structuredContent"];
    assert_common_read_contract(first_output, &fixture.repository_id);
    assert_eq!(first_output["truncated"], true);
    assert_eq!(first_output["completeness"]["state"], "truncated");
    assert_eq!(first_output["completeness"]["continuation"], "available");
    assert_eq!(
        first_output["completeness"]["limiting_resources"][0]["kind"],
        "results"
    );
    let cursor = first_output["next_cursor"]
        .as_str()
        .expect("truncated locate returns an authenticated cursor")
        .to_owned();

    let second = fixture.standalone(
        "locate-page-two",
        "code.locate",
        json!({
            "query": "matrix_target",
            "search_modes": ["lexical"],
            "max_results": 1,
            "cursor": cursor.clone()
        }),
    );
    let second_output = &second["result"]["structuredContent"];
    assert_common_read_contract(second_output, &fixture.repository_id);
    assert_ne!(
        first_output["data"]["matches"][0]["symbol_id"],
        second_output["data"]["matches"][0]["symbol_id"]
    );
    assert_eq!(first_output["repository"], second_output["repository"]);
    assert_eq!(first_output["generation"], second_output["generation"]);
    assert_eq!(first_output["trust"], second_output["trust"]);

    let mismatched_cursor = fixture.standalone(
        "locate-cursor-mismatch",
        "code.locate",
        json!({
            "query": "matrix_target_alpha",
            "search_modes": ["lexical"],
            "max_results": 1,
            "cursor": cursor
        }),
    );
    assert_public_error(&mismatched_cursor, "INVALID_CURSOR");

    let first_exhaustion = collect_locate_pages(fixture, "exhaustion-first");
    let second_exhaustion = collect_locate_pages(fixture, "exhaustion-second");
    assert_eq!(
        first_exhaustion, second_exhaustion,
        "multi-page locate ordering must be repeatable"
    );
    assert_eq!(first_exhaustion.len(), 12);
    assert_eq!(
        first_exhaustion
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        first_exhaustion.len(),
        "multi-page locate must not duplicate results"
    );

    let absent = serde_json::to_value(SymbolId::from_bytes([0xff; 20]))
        .expect("stable symbol identity serializes");
    let mixed = fixture.standalone(
        "explain-mixed",
        "symbol.explain",
        json!({
            "symbol_ids": [fixture.symbols[0].clone(), absent.clone()],
            "include_provenance": "compact",
            "response_profile": "evidence"
        }),
    );
    let mixed_output = &mixed["result"]["structuredContent"];
    assert_common_read_contract(mixed_output, &fixture.repository_id);
    assert_eq!(mixed_output["truncated"], false);
    assert_eq!(
        mixed_output["data"]["symbols"][0]["symbol_id"],
        fixture.symbols[0]
    );
    assert_eq!(mixed_output["data"]["unresolved_ids"], json!([absent]));
    assert_eq!(
        mixed_output["data"]["symbols"][0]["definition"]["repository"],
        fixture.repository_id
    );
    assert_eq!(
        mixed_output["data"]["symbols"][0]["definition"]["generation"],
        mixed_output["generation"]["generation_id"]
    );
    assert_eq!(
        mixed_output["data"]["symbols"][0]["trust"],
        "untrusted_repository_data"
    );
    assert!(
        mixed_output["data"]["symbols"][0]["provenance"]
            .as_array()
            .is_some_and(|items| !items.is_empty())
    );

    let limited_explain = fixture.standalone(
        "explain-result-limit",
        "symbol.explain",
        json!({
            "symbol_ids": [fixture.symbols[0].clone(), fixture.symbols[1].clone()],
            "budget": {"max_results": 2}
        }),
    );
    assert_success(&limited_explain, "symbol.explain");
    let limited_output = &limited_explain["result"]["structuredContent"];
    assert_common_read_contract(limited_output, &fixture.repository_id);
    assert_eq!(limited_output["truncated"], true);
    assert_eq!(limited_output["completeness"]["state"], "truncated");
    assert_eq!(
        limited_output["completeness"]["limiting_resources"][0]["kind"],
        "results"
    );
    assert_eq!(
        limited_output["data"]["symbols"]
            .as_array()
            .expect("bounded explain returns resolved symbols")
            .len(),
        1
    );
    assert_eq!(limited_output["data"]["unresolved_ids"], json!([]));

    for (id, tool, arguments) in [
        (
            "locate-token-budget",
            "code.locate",
            json!({
                "query": "matrix_target_alpha",
                "search_modes": ["exact"],
                "budget": {"max_tokens": 100}
            }),
        ),
        (
            "explain-token-budget",
            "symbol.explain",
            json!({
                "symbol_ids": [fixture.symbols[0].clone()],
                "budget": {"max_tokens": 100}
            }),
        ),
        (
            "source-token-budget",
            "source.read",
            json!({
                "references": [{"source_ref": fixture.source_refs[0].clone()}],
                "budget": {"max_tokens": 100}
            }),
        ),
        (
            "source-byte-budget",
            "source.read",
            json!({
                "references": [{"source_ref": fixture.source_refs[0].clone()}],
                "max_source_bytes": 1
            }),
        ),
    ] {
        let response = fixture.standalone(id, tool, arguments);
        assert_public_error(&response, "BUDGET_EXCEEDED");
        let error = &response["result"]["structuredContent"]["error"];
        assert_eq!(error["retryable"], false);
        assert!(error["repository"].is_null());
        assert!(error["generation"].is_null());
    }
}

fn collect_locate_pages(fixture: &mut RetrievalFixture, run_id: &str) -> Vec<String> {
    let mut cursor = None;
    let mut identity = None;
    let mut symbols = Vec::new();
    for page_index in 0..16 {
        let mut arguments = json!({
            "query": "matrix_target",
            "search_modes": ["lexical"],
            "max_results": 2
        });
        if let Some(cursor) = cursor.take() {
            arguments["cursor"] = json!(cursor);
        }
        let response = fixture.standalone(
            &format!("{run_id}-page-{page_index}"),
            "code.locate",
            arguments,
        );
        assert_success(&response, "code.locate");
        let output = &response["result"]["structuredContent"];
        assert_common_read_contract(output, &fixture.repository_id);
        let observed_identity = (
            output["repository"].clone(),
            output["generation"].clone(),
            output["trust"].clone(),
        );
        if let Some(expected) = &identity {
            assert_eq!(&observed_identity, expected);
        } else {
            identity = Some(observed_identity);
        }
        let matches = output["data"]["matches"]
            .as_array()
            .expect("locate page returns matches");
        assert!(
            !matches.is_empty(),
            "locate emitted an empty intermediate page"
        );
        symbols.extend(matches.iter().map(|matched| {
            matched["symbol_id"]
                .as_str()
                .expect("locate match has a symbol identity")
                .to_owned()
        }));

        let Some(next_cursor) = output["next_cursor"].as_str() else {
            assert_eq!(output["truncated"], false);
            assert_eq!(output["completeness"]["state"], "complete");
            assert_eq!(output["completeness"]["continuation"], "not_applicable");
            return symbols;
        };
        assert_eq!(output["truncated"], true);
        assert_eq!(output["completeness"]["state"], "truncated");
        assert_eq!(output["completeness"]["continuation"], "available");
        cursor = Some(next_cursor.to_owned());
    }
    panic!("locate pagination did not terminate within the bounded page count");
}

fn assert_profile_identity(outputs: &Map<String, Value>, item_path: &str) {
    for field in [
        "repository",
        "generation",
        "coverage",
        "truncated",
        "completeness",
        "next_cursor",
        "trust",
    ] {
        assert_eq!(outputs["compact"][field], outputs["standard"][field]);
        assert_eq!(outputs["standard"][field], outputs["evidence"][field]);
    }
    for field in [
        "symbol_id",
        "file_id",
        "kind",
        "display_name",
        "path",
        "score",
    ] {
        let path = format!("{item_path}/{field}");
        let compact = outputs["compact"].pointer(&path);
        let standard = outputs["standard"].pointer(&path);
        let evidence = outputs["evidence"].pointer(&path);
        if compact.is_some() || standard.is_some() || evidence.is_some() {
            assert_eq!(compact, standard, "profile changed {path}");
            assert_eq!(standard, evidence, "profile changed {path}");
        }
    }
}

fn assert_standalone_batch_parity(standalone: &Value, batch: &Value, tool: &str) {
    assert_success(standalone, tool);
    assert_success(batch, "query.batch");
    let standalone = &standalone["result"]["structuredContent"];
    let batch = &batch["result"]["structuredContent"];
    let operation = &batch["data"]["operation_results"][0];

    assert_common_usage(standalone);
    assert_common_usage(batch);
    assert_eq!(
        batch["data"]["batch_status"], "ok",
        "batch did not preserve {tool}: {batch:#}"
    );
    assert_eq!(operation["tool"], tool);
    assert_eq!(
        operation["status"], "ok",
        "batch child did not preserve {tool}: {batch:#}"
    );
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

fn assert_common_read_contract(output: &Value, repository_id: &str) {
    assert_eq!(output["schema_version"], "1.0");
    assert_eq!(output["repository"]["repository_id"], repository_id);
    assert!(output["generation"]["generation_id"].is_string());
    assert_ne!(
        output["generation"]["generation_id"],
        output["generation"]["parent_generation"]
    );
    assert_eq!(output["trust"], "untrusted_repository_data");
    assert!(output["coverage"]["languages"].is_array());
    assert!(output["completeness"]["state"].is_string());
    assert!(output.get("next_cursor").is_some());
    assert_common_usage(output);
}

fn assert_common_usage(output: &Value) {
    let usage = &output["usage"];
    for field in [
        "rows",
        "edges",
        "source_bytes",
        "json_bytes",
        "estimated_tokens",
        "wall_time_ms",
    ] {
        assert!(
            usage[field].as_u64().is_some(),
            "usage.{field} is an unsigned counter"
        );
    }
    assert!(
        usage["trace_id"]
            .as_str()
            .is_some_and(|trace| !trace.is_empty())
    );
    let serialized = serde_json::to_vec(output).expect("structured output serializes");
    assert_eq!(
        usage["json_bytes"].as_u64(),
        Some(u64::try_from(serialized.len()).expect("response length fits u64"))
    );
    assert_eq!(
        usage["estimated_tokens"].as_u64(),
        Some(estimate_tokens(serialized.len()))
    );
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
        "{tool} did not return structured content"
    );
}

fn assert_public_error(response: &Value, expected: &str) {
    assert!(
        response.get("error").is_none(),
        "public tool failure escaped as JSON-RPC: {response:#}"
    );
    assert_eq!(response["result"]["isError"], true);
    assert_eq!(
        response["result"]["structuredContent"]["error"]["code"], expected,
        "unexpected process error: {response:#}"
    );
}

struct RetrievalFixture {
    _root: tempfile::TempDir,
    daemon: DaemonProcess,
    mcp: McpProcess,
    repository_id: String,
    symbols: Vec<Value>,
    source_refs: Vec<Value>,
}

impl RetrievalFixture {
    fn spawn() -> Self {
        let root = process_support::private_process_tempdir("rl-retrieval-");
        let repository_root = root.path().join("repository");
        fs::create_dir_all(repository_root.join("src"))
            .expect("fixture source directory is created");
        fs::write(
            repository_root.join("Cargo.toml"),
            "[package]\nname = \"retrieval_process_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("fixture manifest is written");
        fs::write(repository_root.join("src").join("lib.rs"), RETRIEVAL_SOURCE)
            .expect("fixture source is written");

        let state_dir = root.path().join("state");
        let runtime_dir = root.path().join("runtime");
        let daemon_binary = ensure_daemon_binary();
        let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
        daemon.wait_until_ready(&runtime_dir);
        let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);
        let index = mcp.call(
            "index",
            "repo.index",
            json!({"root": repository_root, "mode": "auto", "detached": false}),
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

        let mut symbols = Vec::new();
        let mut source_refs = Vec::new();
        for (index, query) in [
            "matrix_target_alpha",
            "matrix_target_beta",
            "matrix_target_gamma",
        ]
        .into_iter()
        .enumerate()
        {
            let locate = mcp.call(
                &format!("setup-locate-{index}"),
                "code.locate",
                json!({
                    "repository": {"repository_id": repository_id},
                    "generation": "active",
                    "query": query,
                    "search_modes": ["exact"],
                    "response_profile": "evidence"
                }),
            );
            assert_success(&locate, "code.locate");
            let matched = &locate["result"]["structuredContent"]["data"]["matches"][0];
            symbols.push(matched["symbol_id"].clone());
            source_refs.push(matched["source_ref"].clone());
        }

        Self {
            _root: root,
            daemon,
            mcp,
            repository_id,
            symbols,
            source_refs,
        }
    }

    fn standalone(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        let mut arguments = arguments
            .as_object()
            .expect("retrieval arguments are objects")
            .clone();
        arguments.insert(
            "repository".to_owned(),
            json!({"repository_id": self.repository_id}),
        );
        arguments.insert("generation".to_owned(), json!("active"));
        self.mcp.call(id, tool, Value::Object(arguments))
    }

    fn batch(&mut self, id: &str, tool: &str, arguments: Value, response_profile: &str) -> Value {
        let mut arguments = arguments
            .as_object()
            .expect("batch retrieval arguments are objects")
            .clone();
        arguments.remove("response_profile");
        self.mcp.call(
            id,
            "query.batch",
            json!({
                "repository": {"repository_id": self.repository_id},
                "generation": "active",
                "response_profile": response_profile,
                "operations": [{
                    "id": "retrieval",
                    "tool": tool,
                    "arguments": arguments
                }]
            }),
        )
    }

    fn finish(mut self) {
        self.mcp.finish();
        self.daemon.finish();
    }
}

fn wait_for_publication(mcp: &mut McpProcess, index: &Value, operation_id: &str) {
    if index["result"]["structuredContent"]["data"]["state"] == "published" {
        return;
    }
    for attempt in 0..30 {
        let status = mcp.call(
            &format!("operation-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 1_000}),
        );
        assert_success(&status, "operation.status");
        match status["result"]["structuredContent"]["data"]["operation"]["state"].as_str() {
            Some("published") => return,
            Some("failed" | "cancelled") => {
                panic!("fixture indexing terminated without publication: {status:#}")
            }
            _ => {}
        }
    }
    panic!("fixture indexing did not publish within the bounded wait");
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
    output: Option<BufReader<ChildStdout>>,
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
                "clientInfo": {"name": "retrieval-process", "version": "1.0"},
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
