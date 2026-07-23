//! Real-process performance evidence for the public MCP tool inventory.
//!
//! The ignored suite reuses one daemon and MCP session for the measured
//! denominator, retains every attempt, and writes only source-free evidence.

use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_bench::{
    CacheState, CancellationClassPlan, EvidenceValue, FixtureScale, MIN_PRIMARY_SUCCESS_SAMPLES,
    PERFORMANCE_EVIDENCE_SCHEMA_VERSION, PUBLIC_MCP_TOOLS, PerformanceCondition,
    PerformanceDimensions, PerformanceEnvironmentManifest, PerformanceProtocol,
    PerformanceRawSample, PerformanceSampleOutcome, ProcessState, ResourceMeasurementMethod,
    ResultCompleteness, SamplePhase, ToolMeasurementPlan, build_performance_evidence,
    encode_performance_evidence, performance_protocol_sha256, sha256_hex,
};
#[cfg(target_os = "linux")]
use rootlight_bench::{
    LinuxProcTreeSampler, ProcessTreeMeasurement, ProcessTreeSample, ProcessTreeSampler,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const EVIDENCE_PATH_ENV: &str = "ROOTLIGHT_PERFORMANCE_EVIDENCE";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const WARMUP_SAMPLES: u64 = 5;
const MAX_MEASURED_ATTEMPTS_PER_TOOL: u64 = 600;
const TOKENIZER_ASSET_SHA256: &str =
    "446a9538cb6c348e3516120d7c08b09f57c36495e2acfffe59a5bf8b0cfb1a2d";

#[test]
fn synthetic_wire_counter_calibration_matches_exact_input() {
    let input = br#"{"tool":"code.locate","max_results":20}"#;
    let tokenizer = tiktoken_rs::o200k_base().expect("pinned tokenizer initializes");

    assert_eq!(input.len(), 39);
    assert_eq!(
        tokenizer
            .encode_ordinary(std::str::from_utf8(input).expect("fixture is utf8"))
            .len(),
        12
    );
    assert_eq!(
        sha256_hex(input),
        "84d3a4905643bd165c5fa09bfad7f2cb54838d0c1c49402e2acea54688903d89"
    );
}

#[test]
#[ignore = "runs 1,995 calls through persistent release-like daemon and MCP processes"]
fn real_daemon_mcp_produces_all_tool_performance_evidence() {
    let fixture = tempfile::tempdir().expect("isolated process fixture is available");
    let repository_root = fixture.path().join("repository");
    write_repository(&repository_root, 64);
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = daemon_binary();
    assert!(
        daemon_binary.is_file(),
        "performance evidence requires a prebuilt daemon at {daemon_binary:?}"
    );
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let mut mcp = McpProcess::spawn(&mcp_binary, &state_dir, &runtime_dir);

    let base = index_repository(&mut mcp, &repository_root, "setup-base");
    append_head_change(&repository_root);
    let head = index_repository(&mut mcp, &repository_root, "setup-head");
    let located = locate_symbol(&mut mcp, &head, "answer");
    let protocol = protocol();
    let cases = tool_cases(&repository_root, &base, &head, &located);
    assert_eq!(
        cases.iter().map(|case| case.tool).collect::<Vec<_>>(),
        PUBLIC_MCP_TOOLS
    );
    let tokenizer = tiktoken_rs::o200k_base().expect("pinned tokenizer initializes");
    let mut samples = Vec::with_capacity(
        PUBLIC_MCP_TOOLS.len().saturating_mul(
            usize::try_from(WARMUP_SAMPLES + MIN_PRIMARY_SUCCESS_SAMPLES)
                .expect("sample denominator fits"),
        ),
    );

    for case in &cases {
        let mut successful_measured = 0_u64;
        for ordinal in 0..WARMUP_SAMPLES + MAX_MEASURED_ATTEMPTS_PER_TOOL {
            let phase = if ordinal < WARMUP_SAMPLES {
                SamplePhase::Warmup
            } else {
                SamplePhase::Measured
            };
            let measurement = ResourceIntervals::begin(&daemon, &mcp);
            let started = Instant::now();
            let response = mcp.call(
                &format!("perf-{}-{ordinal}", case.tool.replace('.', "-")),
                case.tool,
                case.arguments.clone(),
            );
            let outcome = response_outcome(&response);
            if case.tool == "repo.index" && matches!(outcome, PerformanceSampleOutcome::Succeeded) {
                wait_for_index_response(&mut mcp, &response);
            }
            let elapsed_ns = duration_ns(started.elapsed());
            let resources = measurement.finish();
            let encoded = serde_json::to_vec(&response).expect("response serializes");
            let actual_tokens = u64::try_from(
                tokenizer
                    .encode_ordinary(std::str::from_utf8(&encoded).expect("JSON is UTF-8"))
                    .len(),
            )
            .expect("token count fits u64");
            let structured = &response["result"]["structuredContent"];
            samples.push(PerformanceRawSample {
                schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
                tool_id: case.tool.to_owned(),
                condition_id: "warm-small-complete".to_owned(),
                ordinal,
                phase,
                elapsed_ns: elapsed_ns.max(1),
                process_tree_cpu_ns: resources.cpu_ns,
                process_tree_peak_rss_bytes: resources.peak_rss_bytes,
                dimensions: response_dimensions(structured, encoded.len(), actual_tokens),
                outcome: outcome.clone(),
            });
            if phase == SamplePhase::Measured
                && matches!(outcome, PerformanceSampleOutcome::Succeeded)
            {
                successful_measured = successful_measured.saturating_add(1);
                if successful_measured == MIN_PRIMARY_SUCCESS_SAMPLES {
                    break;
                }
            } else if phase == SamplePhase::Measured {
                // A failed attempt remains in the evidence. This bounded pause
                // lets the single analytical lane recover before the retry.
                thread::sleep(Duration::from_millis(2));
            }
        }
        assert_eq!(
            successful_measured, MIN_PRIMARY_SUCCESS_SAMPLES,
            "{} did not retain the preregistered successful denominator",
            case.tool
        );
    }

    let environment =
        environment_manifest(&protocol, &daemon_binary, &mcp_binary, &repository_root);
    let package = build_performance_evidence(
        protocol,
        environment,
        samples,
        Vec::new(),
        None,
        platform_limitations(),
    )
    .expect("real-process performance package validates");
    assert_eq!(package.aggregates.len(), PUBLIC_MCP_TOOLS.len());
    assert!(
        package
            .aggregates
            .iter()
            .all(|aggregate| { aggregate.reconciliation.succeeded == MIN_PRIMARY_SUCCESS_SAMPLES })
    );
    if let Some(output) = std::env::var_os(EVIDENCE_PATH_ENV) {
        let bytes = encode_performance_evidence(&package).expect("package encodes");
        fs::write(PathBuf::from(output), bytes).expect("performance evidence writes");
    }

    mcp.finish();
    daemon.finish();
}

fn protocol() -> PerformanceProtocol {
    PerformanceProtocol {
        schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
        protocol_id: "mcp-public-tools-performance-v1".to_owned(),
        warmup_samples: WARMUP_SAMPLES,
        timeout_ms: u64::try_from(RESPONSE_TIMEOUT.as_millis()).expect("timeout fits u64"),
        concurrency: 1,
        conditions: vec![
            PerformanceCondition {
                condition_id: "warm-small-complete".to_owned(),
                process_state: ProcessState::Warm,
                fixture_scale: FixtureScale::Small,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Warm,
                concurrency: 1,
            },
            PerformanceCondition {
                condition_id: "cold-small-complete".to_owned(),
                process_state: ProcessState::Cold,
                fixture_scale: FixtureScale::Small,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Cold,
                concurrency: 1,
            },
            PerformanceCondition {
                condition_id: "warm-large-complete".to_owned(),
                process_state: ProcessState::Warm,
                fixture_scale: FixtureScale::Large,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Cold,
                concurrency: 1,
            },
            PerformanceCondition {
                condition_id: "warm-small-truncated".to_owned(),
                process_state: ProcessState::Warm,
                fixture_scale: FixtureScale::Small,
                completeness: ResultCompleteness::Truncated,
                cache_state: CacheState::Warm,
                concurrency: 1,
            },
        ],
        tools: PUBLIC_MCP_TOOLS
            .iter()
            .map(|tool| ToolMeasurementPlan::Required {
                tool_id: (*tool).to_owned(),
                primary_condition_id: "warm-small-complete".to_owned(),
                minimum_success_samples: MIN_PRIMARY_SUCCESS_SAMPLES,
            })
            .collect(),
        // Active analytical cancellation uses deterministic process hooks and is
        // retained by the dedicated cancellation-process producer.
        cancellation_classes: vec![
            CancellationClassPlan::NotApplicable {
                class_id: "architecture-analysis".to_owned(),
                reason_code: "measured_by_cancellation_process_artifact".to_owned(),
                reviewer_role: "performance-evidence-owner".to_owned(),
            },
            CancellationClassPlan::NotApplicable {
                class_id: "advanced-query".to_owned(),
                reason_code: "measured_by_cancellation_process_artifact".to_owned(),
                reviewer_role: "performance-evidence-owner".to_owned(),
            },
        ],
        thresholds: Vec::new(),
        exclusion_reason_codes: vec![
            "host_interference".to_owned(),
            "thermal_or_power_state_changed".to_owned(),
        ],
    }
}

fn response_dimensions(
    structured: &Value,
    encoded_bytes: usize,
    actual_tokens: u64,
) -> PerformanceDimensions {
    let usage = &structured["usage"];
    PerformanceDimensions {
        rows: observed_usage(usage, "rows", "rows_not_reported"),
        edges: observed_usage(usage, "edges", "edges_not_reported"),
        traversal_depth: max_named_u64(structured, "depth").map_or_else(
            || EvidenceValue::unavailable("traversal_depth_not_reported"),
            EvidenceValue::observed,
        ),
        result_items: EvidenceValue::observed(count_result_items(&structured["data"])),
        source_bytes: observed_usage(usage, "source_bytes", "source_bytes_not_reported"),
        response_json_bytes: EvidenceValue::observed(
            u64::try_from(encoded_bytes).expect("bounded response length fits u64"),
        ),
        estimated_tokens: observed_usage(usage, "estimated_tokens", "estimate_not_reported"),
        actual_tokens: EvidenceValue::observed(actual_tokens),
        calls: 1,
    }
}

fn response_outcome(response: &Value) -> PerformanceSampleOutcome {
    if let Some(code) = response["error"]["code"].as_i64() {
        return PerformanceSampleOutcome::Failed {
            error_code: format!("json_rpc_{}", code.unsigned_abs()),
        };
    }
    if response["result"]["isError"] == true {
        return PerformanceSampleOutcome::Failed {
            error_code: response["result"]["structuredContent"]["error"]["code"]
                .as_str()
                .map(normalize_identifier)
                .unwrap_or_else(|| "public_tool_error".to_owned()),
        };
    }
    if response["result"]["structuredContent"].is_object() {
        PerformanceSampleOutcome::Succeeded
    } else {
        PerformanceSampleOutcome::Failed {
            error_code: "missing_structured_content".to_owned(),
        }
    }
}

fn observed_usage(usage: &Value, name: &str, unavailable: &str) -> EvidenceValue<u64> {
    usage[name].as_u64().map_or_else(
        || EvidenceValue::unavailable(unavailable),
        EvidenceValue::observed,
    )
}

fn max_named_u64(value: &Value, name: &str) -> Option<u64> {
    match value {
        Value::Object(object) => object.iter().fold(None, |maximum, (key, value)| {
            let direct = (key == name).then(|| value.as_u64()).flatten();
            maximum.max(direct).max(max_named_u64(value, name))
        }),
        Value::Array(values) => values
            .iter()
            .filter_map(|value| max_named_u64(value, name))
            .max(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
    }
}

fn count_result_items(data: &Value) -> u64 {
    const ITEM_ARRAYS: [&str; 17] = [
        "repositories",
        "matches",
        "symbols",
        "groups",
        "paths",
        "impacted",
        "tests",
        "components",
        "cycles",
        "candidates",
        "changes",
        "steps",
        "entries",
        "chunks",
        "rows",
        "results",
        "operations",
    ];
    fn visit(value: &Value) -> u64 {
        match value {
            Value::Object(object) => object
                .iter()
                .map(|(key, value)| {
                    let direct = if ITEM_ARRAYS.contains(&key.as_str()) {
                        value
                            .as_array()
                            .and_then(|values| u64::try_from(values.len()).ok())
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    direct.saturating_add(visit(value))
                })
                .fold(0, u64::saturating_add),
            Value::Array(values) => values.iter().map(visit).fold(0, u64::saturating_add),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
        }
    }
    visit(data)
}

struct ToolCase {
    tool: &'static str,
    arguments: Value,
}

fn tool_cases(
    repository_root: &Path,
    base: &IndexReceipt,
    head: &IndexReceipt,
    located: &LocatedSymbol,
) -> Vec<ToolCase> {
    let repository = || json!({"repository_id": head.repository_id});
    let generation = || Value::String(head.generation_id.clone());
    let symbol = || Value::String(located.symbol_id.clone());
    vec![
        ToolCase {
            tool: "repo.index",
            arguments: json!({"root": repository_root, "mode": "auto", "detached": false}),
        },
        ToolCase {
            tool: "repo.status",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "include_operations": true,
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "repo.list",
            arguments: json!({"max_results": 20, "response_profile": "compact"}),
        },
        ToolCase {
            tool: "operation.status",
            arguments: json!({"operation_id": head.operation_id, "wait_ms": 0}),
        },
        ToolCase {
            tool: "code.locate",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "query": "answer",
                "search_modes": ["exact"],
                "max_results": 10,
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "symbol.explain",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "symbol_ids": [symbol()],
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "symbol.relationships",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "symbol_ids": [symbol()],
                "relations": ["calls"],
                "direction": "both",
                "max_results": 20,
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "flow.trace",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "from": {"symbol_id": symbol()},
                "relations": ["calls"],
                "direction": "outbound",
                "max_depth": 3,
                "max_paths": 20,
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "change.impact",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "change": {"symbol_ids": [symbol()]},
                "max_depth": 3,
                "include_tests": true,
                "profile": "compact"
            }),
        },
        ToolCase {
            tool: "tests.select",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "seeds": {"symbols": [symbol()]},
                "profile": "compact"
            }),
        },
        ToolCase {
            tool: "architecture.overview",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "architecture.cycles",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "projection": {"relations": ["calls"], "level": "symbol"},
                "max_cycles": 20,
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "code.dead",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "entry_point_policy": "standard",
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "history.compare",
            arguments: json!({
                "repository": repository(),
                "base": base.generation_id,
                "head": head.generation_id,
                "max_results": 20,
                "profile": "compact"
            }),
        },
        ToolCase {
            tool: "plan.change",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "objective": "bug_fix",
                "objective_text": "adjust the bounded fixture behavior",
                "targets": [{"symbol_id": symbol()}],
                "profile": "compact"
            }),
        },
        ToolCase {
            tool: "context.pack",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "task": "explain the bounded fixture behavior",
                "seeds": {"symbols": [symbol()]},
                "token_budget": 4_500,
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "source.read",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "references": [{"source_ref": located.source_ref}],
                "include_line_numbers": true,
                "encoding": "utf8_lossless_when_valid",
                "response_profile": "compact"
            }),
        },
        ToolCase {
            tool: "query.advanced",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "query": {"op": "scan", "entity": "function"},
                "max_results": 100,
                "max_depth": 5,
                "cost_limit": 10_000_000
            }),
        },
        ToolCase {
            tool: "query.batch",
            arguments: json!({
                "repository": repository(),
                "generation": generation(),
                "operations": [{
                    "id": "locate",
                    "tool": "code.locate",
                    "arguments": {
                        "query": "answer",
                        "search_modes": ["exact"],
                        "max_results": 10
                    }
                }],
                "response_profile": "compact"
            }),
        },
    ]
}

#[derive(Debug)]
struct IndexReceipt {
    repository_id: String,
    generation_id: String,
    operation_id: String,
}

#[derive(Debug)]
struct LocatedSymbol {
    symbol_id: String,
    source_ref: Value,
}

fn index_repository(mcp: &mut McpProcess, root: &Path, id: &str) -> IndexReceipt {
    let response = mcp.call_success(
        id,
        "repo.index",
        json!({"root": root, "mode": "auto", "detached": false}),
    );
    assert_success(&response, "repo.index");
    wait_for_index_response(mcp, &response);
    let data = &response["result"]["structuredContent"]["data"];
    let operation_id = required_string(&data["operation_id"], "operation identity");
    let generation_id = data["generation_id"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let status = mcp.call_success(
                &format!("{id}-terminal"),
                "operation.status",
                json!({"operation_id": operation_id, "wait_ms": 0}),
            );
            required_string(
                &status["result"]["structuredContent"]["data"]["published_generation"],
                "published generation",
            )
        });
    IndexReceipt {
        repository_id: required_string(&data["repository_id"], "repository identity"),
        generation_id,
        operation_id,
    }
}

fn wait_for_index_response(mcp: &mut McpProcess, response: &Value) {
    if response["result"]["structuredContent"]["data"]["state"] == "published" {
        return;
    }
    let operation_id = required_string(
        &response["result"]["structuredContent"]["data"]["operation_id"],
        "operation identity",
    );
    for attempt in 0..30 {
        let status = mcp.call_success(
            &format!("publication-{operation_id}-{attempt}"),
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

fn locate_symbol(mcp: &mut McpProcess, index: &IndexReceipt, query: &str) -> LocatedSymbol {
    let response = mcp.call_success(
        "setup-locate",
        "code.locate",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation_id,
            "query": query,
            "search_modes": ["exact"],
            "max_results": 10,
            "response_profile": "compact"
        }),
    );
    assert_success(&response, "code.locate");
    let item = response["result"]["structuredContent"]["data"]["matches"]
        .as_array()
        .and_then(|matches| matches.first())
        .expect("fixture symbol is located");
    LocatedSymbol {
        symbol_id: required_string(&item["symbol_id"], "symbol identity"),
        source_ref: item["source_ref"].clone(),
    }
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

fn required_string(value: &Value, field: &str) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{field} is absent: {value:#}"))
        .to_owned()
}

fn write_repository(root: &Path, function_count: usize) {
    fs::create_dir_all(root.join("src")).expect("fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"performance_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("fixture manifest is written");
    let mut source = String::from(
        "pub fn answer() -> usize { helper_000() }\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn answer_works() { assert_eq!(super::answer(), 0); }\n}\n",
    );
    for index in 0..function_count {
        let next = (index + 1) % function_count;
        source.push_str(&format!(
            "pub fn helper_{index:03}() -> usize {{ if false {{ helper_{next:03}() }} else {{ {index} }} }}\n"
        ));
    }
    fs::write(root.join("src").join("lib.rs"), source).expect("fixture source is written");
}

fn append_head_change(root: &Path) {
    let path = root.join("src").join("lib.rs");
    let mut source = fs::read_to_string(&path).expect("fixture source reads");
    source.push_str("pub fn head_only() -> usize { answer() + 1 }\n");
    fs::write(path, source).expect("fixture head source writes");
}

fn environment_manifest(
    protocol: &PerformanceProtocol,
    daemon_binary: &Path,
    mcp_binary: &Path,
    repository_root: &Path,
) -> PerformanceEnvironmentManifest {
    let rustc_verbose = command_output("rustc", &["-Vv"]);
    let target_triple = rustc_verbose
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .expect("rustc reports host target")
        .to_owned();
    PerformanceEnvironmentManifest {
        schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
        source_revision: source_revision(),
        protocol_sha256: performance_protocol_sha256(protocol).expect("protocol hashes"),
        rustc_verbose_sha256: sha256_hex(rustc_verbose.as_bytes()),
        dependency_graph_sha256: sha256_file(
            &Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("Cargo.lock"),
        ),
        target_triple,
        operating_system: normalize_identifier(std::env::consts::OS),
        architecture: normalize_identifier(std::env::consts::ARCH),
        cpu_model: cpu_model(),
        cpu_count: std::thread::available_parallelism()
            .expect("CPU parallelism is available")
            .get()
            .try_into()
            .expect("CPU count fits u32"),
        memory_bytes: system_memory_bytes(),
        build_profile: "cargo-test-profile".to_owned(),
        features: vec!["default".to_owned()],
        binary_sha256: BTreeMap::from([
            ("rootlight-daemon".to_owned(), sha256_file(daemon_binary)),
            ("rootlight-mcp".to_owned(), sha256_file(mcp_binary)),
        ]),
        fixture_sha256: BTreeMap::from([(
            "small".to_owned(),
            sha256_regular_tree(repository_root),
        )]),
        tokenizer_id: "o200k_base-tiktoken_rs-0.12.0".to_owned(),
        tokenizer_sha256: TOKENIZER_ASSET_SHA256.to_owned(),
        monotonic_clock: "std-instant".to_owned(),
        background_process_policy: "isolated-process-best-effort".to_owned(),
        resource_method: resource_method(),
    }
}

#[cfg(target_os = "linux")]
fn resource_method() -> ResourceMeasurementMethod {
    let sampler = LinuxProcTreeSampler::new(std::process::id(), Duration::from_millis(2))
        .expect("Linux proc accounting units are available");
    ResourceMeasurementMethod {
        method_id: "linux-proc-tree-polling".to_owned(),
        platform: "linux".to_owned(),
        polling_interval_us: EvidenceValue::observed(
            u64::try_from(sampler.polling_interval().as_micros())
                .expect("polling interval fits u64"),
        ),
        cpu_resolution_ns: EvidenceValue::observed(
            1_000_000_000_u64 / sampler.clock_ticks_per_second(),
        ),
        rss_resolution_bytes: EvidenceValue::observed(sampler.page_size_bytes()),
        process_tree_included: true,
        caveat_codes: vec![
            "children_shorter_than_poll_interval_may_be_missed".to_owned(),
            "rss_is_sampled_not_kernel_high_water".to_owned(),
        ],
    }
}

#[cfg(not(target_os = "linux"))]
fn resource_method() -> ResourceMeasurementMethod {
    ResourceMeasurementMethod {
        method_id: "unavailable-portable".to_owned(),
        platform: normalize_identifier(std::env::consts::OS),
        polling_interval_us: EvidenceValue::unavailable("safe_process_tree_sampler_unavailable"),
        cpu_resolution_ns: EvidenceValue::unavailable("safe_process_tree_sampler_unavailable"),
        rss_resolution_bytes: EvidenceValue::unavailable("safe_process_tree_sampler_unavailable"),
        process_tree_included: false,
        caveat_codes: vec!["cpu_rss_not_claimed".to_owned()],
    }
}

#[cfg(target_os = "linux")]
struct ResourceIntervals {
    daemon: rootlight_bench::LinuxProcTreeSample,
    mcp: rootlight_bench::LinuxProcTreeSample,
}

#[cfg(target_os = "linux")]
impl ResourceIntervals {
    fn begin(daemon: &DaemonProcess, mcp: &McpProcess) -> Self {
        let daemon_sampler = LinuxProcTreeSampler::new(daemon.pid(), Duration::from_millis(2))
            .expect("daemon sampler initializes");
        let mcp_sampler = LinuxProcTreeSampler::new(mcp.pid(), Duration::from_millis(2))
            .expect("MCP sampler initializes");
        Self {
            daemon: daemon_sampler.begin(),
            mcp: mcp_sampler.begin(),
        }
    }

    fn finish(self) -> ResourceObservation {
        combine_measurements(self.daemon.finish(), self.mcp.finish())
    }
}

#[cfg(target_os = "linux")]
fn combine_measurements(
    daemon: ProcessTreeMeasurement,
    mcp: ProcessTreeMeasurement,
) -> ResourceObservation {
    ResourceObservation {
        cpu_ns: sum_evidence(daemon.cpu_ns, mcp.cpu_ns, "process_tree_cpu_unavailable"),
        peak_rss_bytes: sum_evidence(
            daemon.peak_rss_bytes,
            mcp.peak_rss_bytes,
            "process_tree_rss_unavailable",
        ),
    }
}

#[cfg(target_os = "linux")]
fn sum_evidence(
    left: EvidenceValue<u64>,
    right: EvidenceValue<u64>,
    reason: &str,
) -> EvidenceValue<u64> {
    match (left, right) {
        (EvidenceValue::Observed { value: left }, EvidenceValue::Observed { value: right }) => {
            EvidenceValue::observed(left.saturating_add(right))
        }
        _ => EvidenceValue::unavailable(reason),
    }
}

#[cfg(not(target_os = "linux"))]
struct ResourceIntervals;

#[cfg(not(target_os = "linux"))]
impl ResourceIntervals {
    fn begin(_daemon: &DaemonProcess, _mcp: &McpProcess) -> Self {
        Self
    }

    fn finish(self) -> ResourceObservation {
        ResourceObservation {
            cpu_ns: EvidenceValue::unavailable("safe_process_tree_sampler_unavailable"),
            peak_rss_bytes: EvidenceValue::unavailable("safe_process_tree_sampler_unavailable"),
        }
    }
}

struct ResourceObservation {
    cpu_ns: EvidenceValue<u64>,
    peak_rss_bytes: EvidenceValue<u64>,
}

fn platform_limitations() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        vec![
            "rss_polling_may_miss_short_spikes".to_owned(),
            "cold_large_and_truncated_conditions_are_separate_unexecuted_protocol_cells".to_owned(),
            "cancellation_samples_are_retained_by_separate_process_artifact".to_owned(),
        ]
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec![
            "cpu_rss_unavailable_without_safe_process_tree_api".to_owned(),
            "cold_large_and_truncated_conditions_are_separate_unexecuted_protocol_cells".to_owned(),
            "cancellation_samples_are_retained_by_separate_process_artifact".to_owned(),
        ]
    }
}

fn source_revision() -> String {
    let revision = command_output("git", &["rev-parse", "HEAD"]);
    let revision = revision.trim().to_owned();
    assert!(
        revision.len() == 40
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "source revision is an exact lowercase Git object ID"
    );
    revision
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    let output = Command::new(program)
        .args(arguments)
        .output()
        .unwrap_or_else(|error| panic!("{program} starts: {error}"));
    assert!(
        output.status.success(),
        "{program} succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("command output is UTF-8")
}

fn sha256_file(path: &Path) -> String {
    sha256_hex(&fs::read(path).unwrap_or_else(|error| panic!("{path:?} reads: {error}")))
}

fn sha256_regular_tree(root: &Path) -> String {
    fn walk(root: &Path, directory: &Path, hasher: &mut Sha256) {
        let mut entries = fs::read_dir(directory)
            .expect("fixture directory reads")
            .collect::<Result<Vec<_>, _>>()
            .expect("fixture directory enumerates");
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let file_type = entry.file_type().expect("fixture type reads");
            assert!(!file_type.is_symlink(), "fixture cannot contain symlinks");
            if file_type.is_dir() {
                walk(root, &entry.path(), hasher);
            } else if file_type.is_file() {
                let path = entry.path();
                let relative = path
                    .strip_prefix(root)
                    .expect("fixture path remains under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let bytes = fs::read(&path).expect("fixture file reads");
                hash_length_prefixed(hasher, relative.as_bytes());
                hash_length_prefixed(hasher, &bytes);
            }
        }
    }
    let mut hasher = Sha256::new();
    walk(root, root, &mut hasher);
    hex_digest(hasher.finalize())
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(
        u64::try_from(bytes.len())
            .expect("fixture component length fits u64")
            .to_le_bytes(),
    );
    hasher.update(bytes);
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write as _;

    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to a string cannot fail");
    }
    output
}

fn normalize_identifier(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len().min(128));
    for character in value.chars() {
        if normalized.len() >= 128 {
            break;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ':') {
            normalized.push(character.to_ascii_lowercase());
        } else if !normalized.ends_with('_') {
            normalized.push('_');
        }
    }
    normalized.trim_matches('_').to_owned()
}

#[cfg(target_os = "linux")]
fn cpu_model() -> String {
    let model = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|cpuinfo| {
            cpuinfo
                .lines()
                .find_map(|line| line.strip_prefix("model name\t: "))
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unavailable".to_owned());
    normalize_identifier(&model)
}

#[cfg(windows)]
fn cpu_model() -> String {
    normalize_identifier(
        &std::env::var("PROCESSOR_IDENTIFIER").unwrap_or_else(|_| "unavailable".to_owned()),
    )
}

#[cfg(not(any(target_os = "linux", windows)))]
fn cpu_model() -> String {
    "unavailable".to_owned()
}

#[cfg(target_os = "linux")]
fn system_memory_bytes() -> u64 {
    let kib = fs::read_to_string("/proc/meminfo")
        .expect("Linux memory information reads")
        .lines()
        .find_map(|line| {
            line.strip_prefix("MemTotal:")
                .and_then(|rest| rest.split_ascii_whitespace().next())
                .and_then(|value| value.parse::<u64>().ok())
        })
        .expect("Linux total memory is reported");
    kib.saturating_mul(1_024)
}

#[cfg(windows)]
fn system_memory_bytes() -> u64 {
    let script = "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory";
    command_output(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", script],
    )
    .trim()
    .parse()
    .expect("Windows total memory is numeric")
}

#[cfg(not(any(target_os = "linux", windows)))]
fn system_memory_bytes() -> u64 {
    1
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn daemon_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"))
        .parent()
        .expect("MCP binary has a profile directory")
        .join(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX))
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
            .stderr(Stdio::piped())
            .spawn()
            .expect("isolated daemon process starts");
        let input = child.stdin.take().expect("daemon stdin is piped");
        Self {
            child: Some(child),
            input: Some(input),
        }
    }

    #[cfg(target_os = "linux")]
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("daemon child is retained").id()
    }

    fn wait_until_ready(&mut self, runtime_dir: &Path) {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        let discovery = runtime_dir.join("daemon.json");
        while Instant::now() < deadline {
            if discovery.is_file() {
                return;
            }
            assert!(
                self.child
                    .as_mut()
                    .expect("daemon child is retained")
                    .try_wait()
                    .expect("daemon status is readable")
                    .is_none(),
                "daemon exited before publishing discovery"
            );
            thread::sleep(POLL_INTERVAL);
        }
        panic!("daemon did not publish discovery within the startup bound");
    }

    fn finish(&mut self) {
        self.input.take();
        let child = self.child.as_mut().expect("daemon child is retained");
        let status = wait_for_exit(child, SHUTDOWN_TIMEOUT);
        let mut stderr = String::new();
        child
            .stderr
            .take()
            .expect("daemon stderr is piped")
            .read_to_string(&mut stderr)
            .expect("daemon stderr reads");
        assert!(
            status.success(),
            "daemon process exits successfully: {stderr}"
        );
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
    responses: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}

impl McpProcess {
    fn spawn(binary: &Path, state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut child = Command::new(binary)
            .env("ROOTLIGHT_STATE_DIR", state_dir)
            .env("ROOTLIGHT_RUNTIME_DIR", runtime_dir)
            .env("ROOTLIGHT_MCP_PROFILE", "developer")
            .env("ROOTLIGHT_MCP_PROFILE_CEILING", "developer")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("MCP fixture process starts");
        let output = child.stdout.take().expect("MCP stdout is piped");
        let (responses_tx, responses) = mpsc::sync_channel(64);
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    return;
                };
                if responses_tx.send(line).is_err() {
                    return;
                }
            }
        });
        let mut process = Self {
            input: child.stdin.take(),
            child: Some(child),
            responses,
            reader: Some(reader),
        };
        process.write(&json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "performance-process", "version": "1.0"},
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

    #[cfg(target_os = "linux")]
    fn pid(&self) -> u32 {
        self.child.as_ref().expect("MCP child is retained").id()
    }

    fn call(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        self.write(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments}
        }));
        let response = self.read();
        assert_eq!(response["id"], id, "MCP response identity differs");
        response
    }

    fn call_success(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        const MAX_ATTEMPTS: u64 = 10;

        for attempt in 1..=MAX_ATTEMPTS {
            let response = self.call(&format!("{id}-attempt-{attempt}"), tool, arguments.clone());
            if matches!(
                response_outcome(&response),
                PerformanceSampleOutcome::Succeeded
            ) {
                return response;
            }
            thread::yield_now();
        }
        panic!("{tool} did not return a successful setup response");
    }

    fn write(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("MCP stdin is retained");
        serde_json::to_writer(&mut *input, message).expect("MCP request serializes");
        input.write_all(b"\n").expect("MCP request terminates");
        input.flush().expect("MCP request flushes");
    }

    fn read(&self) -> Value {
        let line = self
            .responses
            .recv_timeout(RESPONSE_TIMEOUT)
            .expect("MCP response arrives within the bound");
        serde_json::from_str(&line).expect("MCP response is valid JSON")
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
        self.reader
            .take()
            .expect("MCP reader is retained")
            .join()
            .expect("MCP reader thread joins");
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("child status is readable") {
            return status;
        }
        thread::sleep(POLL_INTERVAL);
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
