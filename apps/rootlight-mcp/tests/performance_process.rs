//! Real-process performance evidence for the public MCP tool inventory.
//!
//! The ignored suite reuses one daemon and MCP session for the measured
//! denominator, retains every attempt, and writes only source-free evidence.

mod process_support;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_bench::{
    CacheState, CancellationClassPlan, EvidenceValue, FixtureScale, GateDisposition,
    MIN_PRIMARY_SUCCESS_SAMPLES, PERFORMANCE_EVIDENCE_SCHEMA_VERSION, PUBLIC_MCP_TOOLS,
    PerformanceCondition, PerformanceDimensions, PerformanceEnvironmentManifest,
    PerformanceProtocol, PerformanceRawSample, PerformanceSampleOutcome, PerformanceThreshold,
    ProcessState, ResourceMeasurementMethod, ResultCompleteness, SamplePhase, ThresholdClass,
    ThresholdMetric, ToolMeasurementPlan, UnavailablePolicy, build_performance_evidence,
    encode_performance_evidence, performance_protocol_sha256, sha256_hex,
};
#[cfg(target_os = "linux")]
use rootlight_bench::{
    LinuxProcTreeSampler, ProcessTreeMeasurement, ProcessTreeSample, ProcessTreeSampler,
};
use rootlight_client::{Client, ConnectPolicy};
use rootlight_runtime::RuntimePaths;
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const EVIDENCE_PATH_ENV: &str = "ROOTLIGHT_PERFORMANCE_EVIDENCE";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const LARGE_FIXTURE_SOURCE_FILES: usize = 100;
const LARGE_FIXTURE_PHYSICAL_LOC: usize = 1_000_000;
const WARMUP_SAMPLES: u64 = 5;
const SECONDARY_SUCCESS_SAMPLES: u64 = 100;
const MAX_MEASURED_ATTEMPTS_PER_TOOL: u64 = 600;
const REPO_STATUS_P95_NS: u64 = 10_000_000;
const CODE_LOCATE_P95_NS: u64 = 20_000_000;
const SOURCE_READ_P95_NS: u64 = 30_000_000;
const RELATIONSHIPS_P95_NS: u64 = 40_000_000;
const FAST_P95_NS: u64 = 50_000_000;
const FLOW_P95_NS: u64 = 100_000_000;
const INTERACTIVE_P95_NS: u64 = 150_000_000;
const TEST_SELECTION_P95_NS: u64 = 300_000_000;
const IMPACT_P95_NS: u64 = 500_000_000;
const CONTEXT_PACK_P95_NS: u64 = 750_000_000;
const ARCHITECTURE_P95_NS: u64 = 750_000_000;
const CYCLES_P95_NS: u64 = 1_000_000_000;
#[cfg(target_os = "linux")]
const PROCESS_TREE_RSS_P99_BYTES: u64 = 1024 * 1024 * 1024;
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
fn public_tool_protocol_preregisters_enforced_slos() {
    let protocol = protocol();
    let primary_thresholds_per_tool = if cfg!(target_os = "linux") { 5 } else { 4 };

    assert_eq!(protocol.conditions.len(), 4);
    assert_eq!(
        protocol.thresholds.len(),
        PUBLIC_MCP_TOOLS.len() * primary_thresholds_per_tool + 12
    );
    for tool in PUBLIC_MCP_TOOLS {
        let thresholds = protocol
            .thresholds
            .iter()
            .filter(|threshold| {
                threshold.subject_id == tool && threshold.condition_id == "warm-small-complete"
            })
            .collect::<Vec<_>>();
        assert_eq!(thresholds.len(), primary_thresholds_per_tool, "{tool}");
        assert!(
            thresholds
                .iter()
                .all(|threshold| threshold.class == ThresholdClass::Gate
                    && threshold.unavailable_policy == UnavailablePolicy::Block)
        );
        assert!(
            thresholds
                .iter()
                .any(|threshold| threshold.metric == ThresholdMetric::WallLatencyP50Ns)
        );
        assert!(
            thresholds
                .iter()
                .any(|threshold| threshold.metric == ThresholdMetric::WallLatencyP95Ns)
        );
        assert!(
            thresholds
                .iter()
                .any(|threshold| threshold.metric == ThresholdMetric::WallLatencyP99Ns)
        );
        assert!(
            thresholds
                .iter()
                .any(|threshold| threshold.metric == ThresholdMetric::ReliabilityFailureRatePpm)
        );
        #[cfg(target_os = "linux")]
        assert!(
            thresholds
                .iter()
                .any(|threshold| threshold.metric == ThresholdMetric::PeakRssP99Bytes)
        );
    }
    performance_protocol_sha256(&protocol).expect("performance protocol validates");
}

#[test]
#[ignore = "runs release-profile cold, warm, large, and truncated MCP distributions"]
fn real_daemon_mcp_produces_all_tool_performance_evidence() {
    let fixture = process_support::private_process_tempdir("rl-perf-");
    let repository_root = fixture.path().join("repository");
    write_repository(&repository_root, 128);
    let large_repository_root = fixture.path().join("large-repository");
    write_large_repository(&large_repository_root);
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let daemon_binary = daemon_binary();
    assert!(
        daemon_binary.is_file(),
        "performance evidence requires a prebuilt daemon at {daemon_binary:?}"
    );
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let runtime_paths = RuntimePaths::new(state_dir.clone(), runtime_dir.clone())
        .expect("isolated runtime paths are valid");
    let control_client =
        Client::connect_or_start(&runtime_paths, [0x70; 16], ConnectPolicy::ExistingOnly)
            .expect("control client connects to the isolated daemon");
    let mcp_binary = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let mut mcp = McpProcess::spawn(&mcp_binary, &state_dir, &runtime_dir);

    let base = index_repository(&mut mcp, &repository_root, "setup-base");
    append_head_change(&repository_root);
    let head = index_repository(&mut mcp, &repository_root, "setup-head");
    let large = index_repository(&mut mcp, &large_repository_root, "setup-large");
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
        let mut next_context_session_rotation = 20_u64;
        let mut failure_details = Vec::new();
        for ordinal in 0..WARMUP_SAMPLES + MAX_MEASURED_ATTEMPTS_PER_TOOL {
            if case.tool == "context.pack" && successful_measured == next_context_session_rotation {
                mcp.finish();
                mcp = McpProcess::spawn(&mcp_binary, &state_dir, &runtime_dir);
                next_context_session_rotation = next_context_session_rotation.saturating_add(20);
            }
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
            if !matches!(outcome, PerformanceSampleOutcome::Succeeded) && failure_details.len() < 3
            {
                failure_details.push(response.clone());
            }
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
            if case.tool == "context.pack" {
                wait_until_connections_released(&control_client);
            }
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
        let failure_codes = samples
            .iter()
            .filter(|sample| sample.tool_id == case.tool)
            .filter_map(|sample| match &sample.outcome {
                PerformanceSampleOutcome::Failed { error_code } => Some(error_code.as_str()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            successful_measured, MIN_PRIMARY_SUCCESS_SAMPLES,
            "{} did not retain the preregistered successful denominator; failures={failure_codes:?}; details={failure_details:#?}",
            case.tool,
        );
    }

    let mut cold_successes = 0_u64;
    for ordinal in 0..WARMUP_SAMPLES + MAX_MEASURED_ATTEMPTS_PER_TOOL {
        mcp.finish();
        mcp = McpProcess::spawn(&mcp_binary, &state_dir, &runtime_dir);
        let phase = if ordinal < WARMUP_SAMPLES {
            SamplePhase::Warmup
        } else {
            SamplePhase::Measured
        };
        let query_index = ordinal % 128;
        let (sample, response) = measure_call(
            &daemon,
            &mut mcp,
            &tokenizer,
            "code.locate",
            "cold-small-complete",
            ordinal,
            phase,
            json!({
                "repository": {"repository_id": head.repository_id},
                "generation": head.generation_id,
                "query": format!("helper_{query_index:03}"),
                "search_modes": ["exact"],
                "max_results": 10,
                "response_profile": "compact"
            }),
        );
        let succeeded = matches!(sample.outcome, PerformanceSampleOutcome::Succeeded);
        assert_success(&response, "cold code.locate");
        samples.push(sample);
        if phase == SamplePhase::Measured && succeeded {
            cold_successes = cold_successes.saturating_add(1);
            if cold_successes == SECONDARY_SUCCESS_SAMPLES {
                break;
            }
        }
    }
    assert_eq!(cold_successes, SECONDARY_SUCCESS_SAMPLES);

    let large_arguments = json!({
        "repository": {"repository_id": large.repository_id},
        "generation": large.generation_id,
        "query": "helper_4095",
        "search_modes": ["exact"],
        "max_results": 10,
        "response_profile": "compact"
    });
    let large_warmup =
        mcp.call_success("large-cache-prime", "code.locate", large_arguments.clone());
    assert_success(&large_warmup, "large code.locate cache prime");
    collect_secondary_distribution(
        &daemon,
        &mut mcp,
        &tokenizer,
        "code.locate",
        "warm-large-complete",
        large_arguments,
        |response| assert_success(response, "large code.locate"),
        &mut samples,
    );

    let truncated_arguments = json!({
        "repository": {"repository_id": head.repository_id},
        "generation": head.generation_id,
        "query": "helper",
        "search_modes": ["lexical"],
        "max_results": 1,
        "response_profile": "compact"
    });
    let truncated_warmup = mcp.call_success(
        "truncated-cache-prime",
        "code.locate",
        truncated_arguments.clone(),
    );
    assert_truncated_locate(&truncated_warmup);
    collect_secondary_distribution(
        &daemon,
        &mut mcp,
        &tokenizer,
        "code.locate",
        "warm-small-truncated",
        truncated_arguments,
        assert_truncated_locate,
        &mut samples,
    );

    let environment = environment_manifest(
        &protocol,
        &daemon_binary,
        &mcp_binary,
        &repository_root,
        &large_repository_root,
    );
    let package = build_performance_evidence(
        protocol,
        environment,
        samples,
        Vec::new(),
        None,
        platform_limitations(),
    )
    .expect("real-process performance package validates");
    assert_eq!(package.aggregates.len(), PUBLIC_MCP_TOOLS.len() + 3);
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
    let blocked_thresholds = package
        .threshold_evaluations
        .iter()
        .filter(|evaluation| evaluation.disposition != GateDisposition::Pass)
        .collect::<Vec<_>>();
    assert_eq!(
        package.disposition,
        GateDisposition::Pass,
        "blocked performance thresholds: {blocked_thresholds:#?}"
    );
    assert!(blocked_thresholds.is_empty());

    mcp.finish();
    daemon.finish();
}

#[allow(clippy::too_many_arguments)]
fn measure_call(
    daemon: &DaemonProcess,
    mcp: &mut McpProcess,
    tokenizer: &tiktoken_rs::CoreBPE,
    tool: &str,
    condition_id: &str,
    ordinal: u64,
    phase: SamplePhase,
    arguments: Value,
) -> (PerformanceRawSample, Value) {
    let measurement = ResourceIntervals::begin(daemon, mcp);
    let started = Instant::now();
    let response = mcp.call(
        &format!("perf-{}-{}-{ordinal}", condition_id, tool.replace('.', "-")),
        tool,
        arguments,
    );
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
    (
        PerformanceRawSample {
            schema_version: PERFORMANCE_EVIDENCE_SCHEMA_VERSION.to_owned(),
            tool_id: tool.to_owned(),
            condition_id: condition_id.to_owned(),
            ordinal,
            phase,
            elapsed_ns: elapsed_ns.max(1),
            process_tree_cpu_ns: resources.cpu_ns,
            process_tree_peak_rss_bytes: resources.peak_rss_bytes,
            dimensions: response_dimensions(structured, encoded.len(), actual_tokens),
            outcome: response_outcome(&response),
        },
        response,
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_secondary_distribution(
    daemon: &DaemonProcess,
    mcp: &mut McpProcess,
    tokenizer: &tiktoken_rs::CoreBPE,
    tool: &str,
    condition_id: &str,
    arguments: Value,
    validate: impl Fn(&Value),
    samples: &mut Vec<PerformanceRawSample>,
) {
    let mut successes = 0_u64;
    for ordinal in 0..WARMUP_SAMPLES + MAX_MEASURED_ATTEMPTS_PER_TOOL {
        let phase = if ordinal < WARMUP_SAMPLES {
            SamplePhase::Warmup
        } else {
            SamplePhase::Measured
        };
        let (sample, response) = measure_call(
            daemon,
            mcp,
            tokenizer,
            tool,
            condition_id,
            ordinal,
            phase,
            arguments.clone(),
        );
        let succeeded = matches!(sample.outcome, PerformanceSampleOutcome::Succeeded);
        validate(&response);
        samples.push(sample);
        if phase == SamplePhase::Measured && succeeded {
            successes = successes.saturating_add(1);
            if successes == SECONDARY_SUCCESS_SAMPLES {
                break;
            }
        }
    }
    assert_eq!(
        successes, SECONDARY_SUCCESS_SAMPLES,
        "{tool} did not retain the secondary {condition_id} denominator"
    );
}

fn assert_truncated_locate(response: &Value) {
    assert_success(response, "truncated code.locate");
    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["truncated"], true);
    assert_eq!(structured["completeness"]["state"], "truncated");
}

fn wait_until_connections_released(client: &Client) {
    let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
    let mut consecutive_released_samples = 0_u8;
    loop {
        let health = client.health().expect("daemon health remains available");
        if health.active_connections <= 1 {
            consecutive_released_samples = consecutive_released_samples.saturating_add(1);
            if consecutive_released_samples == 3 {
                return;
            }
        } else {
            consecutive_released_samples = 0;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not release context-pack provider connections"
        );
        thread::sleep(POLL_INTERVAL);
    }
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
                condition_id: "cold-small-complete".to_owned(),
                process_state: ProcessState::Cold,
                fixture_scale: FixtureScale::Small,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Cold,
                concurrency: 1,
            },
            PerformanceCondition {
                condition_id: "warm-small-complete".to_owned(),
                process_state: ProcessState::Warm,
                fixture_scale: FixtureScale::Small,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Warm,
                concurrency: 1,
            },
            PerformanceCondition {
                condition_id: "warm-large-complete".to_owned(),
                process_state: ProcessState::Warm,
                fixture_scale: FixtureScale::Large,
                completeness: ResultCompleteness::Complete,
                cache_state: CacheState::Warm,
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
        thresholds: performance_thresholds(),
        exclusion_reason_codes: vec![
            "host_interference".to_owned(),
            "thermal_or_power_state_changed".to_owned(),
        ],
    }
}

fn performance_thresholds() -> Vec<PerformanceThreshold> {
    let primary_thresholds_per_tool = if cfg!(target_os = "linux") { 5 } else { 4 };
    let mut thresholds =
        Vec::with_capacity(PUBLIC_MCP_TOOLS.len() * primary_thresholds_per_tool + 12);
    for tool in PUBLIC_MCP_TOOLS {
        let latency_budget = wall_latency_p95_budget_ns(tool);
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("{tool}-warm-p50"),
            subject_id: tool.to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP50Ns,
            upper_bound: latency_budget,
            unavailable_policy: UnavailablePolicy::Block,
        });
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("{tool}-warm-p95"),
            subject_id: tool.to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP95Ns,
            upper_bound: latency_budget,
            unavailable_policy: UnavailablePolicy::Block,
        });
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("{tool}-warm-p99"),
            subject_id: tool.to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP99Ns,
            upper_bound: latency_budget.saturating_mul(2),
            unavailable_policy: UnavailablePolicy::Block,
        });
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("{tool}-warm-reliability"),
            subject_id: tool.to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::ReliabilityFailureRatePpm,
            // One failed attempt in this protocol is at least 10,000 ppm, so
            // the smallest valid positive threshold enforces zero failures.
            upper_bound: 1,
            unavailable_policy: UnavailablePolicy::Block,
        });
        #[cfg(target_os = "linux")]
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("{tool}-warm-rss-p99"),
            subject_id: tool.to_owned(),
            condition_id: "warm-small-complete".to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::PeakRssP99Bytes,
            upper_bound: PROCESS_TREE_RSS_P99_BYTES,
            unavailable_policy: UnavailablePolicy::Block,
        });
    }
    for (condition_id, budget) in [
        ("cold-small-complete", 300_000_000),
        ("warm-large-complete", CODE_LOCATE_P95_NS),
        ("warm-small-truncated", CODE_LOCATE_P95_NS.saturating_mul(2)),
    ] {
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("code-locate-{condition_id}-p50"),
            subject_id: "code.locate".to_owned(),
            condition_id: condition_id.to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP50Ns,
            upper_bound: budget,
            unavailable_policy: UnavailablePolicy::Block,
        });
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("code-locate-{condition_id}-p95"),
            subject_id: "code.locate".to_owned(),
            condition_id: condition_id.to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP95Ns,
            upper_bound: budget,
            unavailable_policy: UnavailablePolicy::Block,
        });
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("code-locate-{condition_id}-p99"),
            subject_id: "code.locate".to_owned(),
            condition_id: condition_id.to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::WallLatencyP99Ns,
            upper_bound: budget.saturating_mul(2),
            unavailable_policy: UnavailablePolicy::Block,
        });
        thresholds.push(PerformanceThreshold {
            threshold_id: format!("code-locate-{condition_id}-reliability"),
            subject_id: "code.locate".to_owned(),
            condition_id: condition_id.to_owned(),
            class: ThresholdClass::Gate,
            metric: ThresholdMetric::ReliabilityFailureRatePpm,
            upper_bound: 1,
            unavailable_policy: UnavailablePolicy::Block,
        });
    }
    thresholds
}

fn wall_latency_p95_budget_ns(tool: &str) -> u64 {
    match tool {
        "repo.status" => REPO_STATUS_P95_NS,
        "code.locate" => CODE_LOCATE_P95_NS,
        "source.read" => SOURCE_READ_P95_NS,
        "symbol.relationships" => RELATIONSHIPS_P95_NS,
        "operation.status" | "repo.list" => FAST_P95_NS,
        "flow.trace" => FLOW_P95_NS,
        "history.compare" | "plan.change" | "query.advanced" | "query.batch" | "symbol.explain" => {
            INTERACTIVE_P95_NS
        }
        "tests.select" => TEST_SELECTION_P95_NS,
        "change.impact" | "code.dead" | "repo.index" => IMPACT_P95_NS,
        "architecture.overview" => ARCHITECTURE_P95_NS,
        "context.pack" => CONTEXT_PACK_P95_NS,
        "architecture.cycles" => CYCLES_P95_NS,
        _ => panic!("public performance tool {tool:?} has no latency budget"),
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
        json!({"root": root, "mode": "auto", "detached": true}),
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
    for attempt in 0..1_800 {
        let status = mcp.call_success(
            &format!("publication-{operation_id}-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 0}),
        );
        assert_success(&status, "operation.status");
        match status["result"]["structuredContent"]["data"]["operation"]["state"].as_str() {
            Some("published") => return,
            Some("failed" | "cancelled") => {
                panic!("fixture indexing terminated without publication: {status:#}")
            }
            _ => {}
        }
        thread::sleep(Duration::from_millis(100));
    }
    panic!("fixture indexing did not publish within the three-minute bounded wait");
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

fn write_large_repository(root: &Path) {
    fs::create_dir_all(root.join("src")).expect("large fixture source directory is created");
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"large_performance_fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .expect("large fixture manifest is written");
    let lines_per_file = LARGE_FIXTURE_PHYSICAL_LOC / LARGE_FIXTURE_SOURCE_FILES;
    for file_index in 0..LARGE_FIXTURE_SOURCE_FILES {
        let mut source = String::with_capacity(lines_per_file.saturating_mul(32));
        let mut written_lines = 0;
        if file_index == 0 {
            source.push_str("pub fn answer() -> usize { helper_000() }\n");
            written_lines += 1;
        }
        let first_helper = file_index.saturating_mul(4_096) / LARGE_FIXTURE_SOURCE_FILES;
        let helper_end =
            (file_index.saturating_add(1)).saturating_mul(4_096) / LARGE_FIXTURE_SOURCE_FILES;
        for function_index in first_helper..helper_end {
            let next = (function_index + 1) % 4_096;
            source.push_str(&format!(
                "pub fn helper_{function_index:03}() -> usize {{ if false {{ helper_{next:03}() }} else {{ {function_index} }} }}\n"
            ));
            written_lines += 1;
        }
        if file_index != 0 {
            source.push_str(&format!(
                "pub fn shard_{file_index:03}() -> usize {{ {file_index} }}\n"
            ));
            written_lines += 1;
        }
        let retained_lines = lines_per_file.saturating_sub(written_lines);
        if retained_lines >= 2 {
            source.push_str("/*\n");
            for _ in 2..retained_lines {
                source.push_str("retained physical source line\n");
            }
            source.push_str("*/\n");
            written_lines = written_lines.saturating_add(retained_lines);
        }
        assert_eq!(written_lines, lines_per_file);
        fs::write(
            root.join("src").join(format!("shard_{file_index:03}.rs")),
            source,
        )
        .expect("large fixture source file is written");
    }
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
    large_repository_root: &Path,
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
        build_profile: if cfg!(debug_assertions) {
            "development-test".to_owned()
        } else {
            "release".to_owned()
        },
        features: vec!["default".to_owned()],
        binary_sha256: BTreeMap::from([
            ("rootlight-daemon".to_owned(), sha256_file(daemon_binary)),
            ("rootlight-mcp".to_owned(), sha256_file(mcp_binary)),
        ]),
        fixture_sha256: BTreeMap::from([
            (
                "large".to_owned(),
                sha256_regular_tree(large_repository_root),
            ),
            ("small".to_owned(), sha256_regular_tree(repository_root)),
        ]),
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
            "cancellation_samples_are_retained_by_separate_process_artifact".to_owned(),
        ]
    }
    #[cfg(not(target_os = "linux"))]
    {
        vec![
            "cpu_rss_unavailable_without_safe_process_tree_api".to_owned(),
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
    stderr_reader: Option<JoinHandle<String>>,
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
        let stderr = child.stderr.take().expect("daemon stderr is piped");
        let stderr_reader = thread::spawn(move || {
            let mut output = String::new();
            BufReader::new(stderr)
                .read_to_string(&mut output)
                .expect("daemon stderr reads");
            output
        });
        Self {
            child: Some(child),
            input: Some(input),
            stderr_reader: Some(stderr_reader),
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
        self.child.take();
        let stderr = self
            .stderr_reader
            .take()
            .expect("daemon stderr reader is retained")
            .join()
            .expect("daemon stderr reader joins");
        assert!(
            status.success(),
            "daemon process exits successfully: {stderr}"
        );
    }
}

impl Drop for DaemonProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
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

        let mut failures = Vec::with_capacity(3);
        for attempt in 1..=MAX_ATTEMPTS {
            let response = self.call(&format!("{id}-attempt-{attempt}"), tool, arguments.clone());
            if matches!(
                response_outcome(&response),
                PerformanceSampleOutcome::Succeeded
            ) {
                return response;
            }
            if failures.len() < failures.capacity() {
                failures.push(response);
            }
            thread::yield_now();
        }
        panic!("{tool} did not return a successful setup response: {failures:#?}");
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
