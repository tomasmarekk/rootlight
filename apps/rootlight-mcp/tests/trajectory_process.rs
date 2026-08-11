//! Retained trajectory evidence across the real daemon and MCP processes.
//!
//! The optional report contains only the source-free package produced by
//! `rootlight-bench`; raw JSON-RPC and source frames remain process-local.

mod process_support;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_bench::{
    AblationBlindingKey, AblationDecision, AblationVariant, BlindedAblationCandidate,
    BlindedCandidateMetrics, BlindedRunOutcome, BoundedFileExplorationAdapter,
    BoundedFileObservation, CandidateRubricEvidence, O200kTrajectoryTokenizer,
    RawTrajectoryAttempt, RawTrajectoryCall, RestrictedPairingMap, RubricDimension,
    RubricObservation, TrajectoryAdapter, TrajectoryAttemptOutcome, TrajectoryClaimSignals,
    TrajectoryCondition, TrajectoryEvidencePackage, TrajectoryExecutionBoundary,
    TrajectoryExecutionInput, TrajectoryExposureProfile, TrajectoryOperationStatus,
    TrajectoryProtocol, TrajectorySharedBounds, TrajectoryTokenizer, TrajectoryToolIdentity,
    TrajectoryWorkflowFamily, UnavailableTrajectoryAdapter, UnsupportedClaimAssessment,
    UnsupportedClaimCategory, WorkflowQualityCandidateMeasurement, WorkflowQualityDimension,
    WorkflowQualityDimensionScore, WorkflowQualityPairMeasurement, WorkflowQualityProtocol,
    WorkflowQualityTaskRegistration, build_workflow_quality_evidence, encode_context_pack_ablation,
    encode_trajectory_evidence, encode_workflow_quality_evidence, prepare_blinded_ablation,
    preregister_context_pack_ablation, preregister_workflow_quality_protocol,
    preregistered_trajectory_protocol, run_trajectory_suite, sha256_hex, trajectory_task_prompt,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const REPORT_ENV: &str = "ROOTLIGHT_TRAJECTORY_REPORT";
const ABLATION_REPORT_ENV: &str = "ROOTLIGHT_ABLATION_REPORT";
const QUALITY_REPORT_ENV: &str = "ROOTLIGHT_WORKFLOW_QUALITY_REPORT";
const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn consumer_task_uses_preregistered_metadata_and_attempt_seed() {
    let first = fixture_consumer_task(17);
    let second = fixture_consumer_task(43);

    assert_eq!(first.task_id, second.task_id);
    assert_eq!(first.task_sha256, second.task_sha256);
    assert_eq!(first.fixture_sha256, second.fixture_sha256);
    assert_eq!(first.seed, 17);
    assert_eq!(second.seed, 43);
    assert!(first.prompt.contains("budget_entry"));
    assert!(first.prompt.contains("budget_helper"));
    assert!(second.prompt.contains("budget_helper"));
    assert!(second.prompt.contains("budget_entry"));
    assert_eq!(
        first.rootlight_tools,
        vec!["code.locate".to_owned(), "context.pack".to_owned()]
    );
    assert_eq!(
        first.expected_evidence,
        BTreeSet::from([
            "context_roles".to_owned(),
            "implementation_identity".to_owned(),
            "test_identity".to_owned(),
        ])
    );
}

#[test]
fn every_preregistered_prompt_drives_the_intended_consumer_plan() {
    let protocol = preregistered_trajectory_protocol("ab".repeat(32))
        .expect("fixture trajectory protocol is valid");
    for workflow in protocol.workflows {
        for seed in [17, 43] {
            let prompt = trajectory_task_prompt(workflow.family, seed);
            assert_eq!(
                classify_task_prompt(&prompt),
                Some(workflow.family),
                "the controlled consumer must derive its plan from the task prompt"
            );
        }
    }
}

#[test]
fn consumer_answer_contains_only_observed_evidence() {
    let task = fixture_consumer_task(17);
    let observed = ObservableCall {
        tool: "context.pack".to_owned(),
        response: json!({
            "result": {
                "isError": false,
                "structuredContent": {
                    "data": {
                        "symbol_id": "sym1_observed_entry",
                        "source_ref": {"generation": "observed"},
                        "content": "pub fn budget_entry(value: usize) -> usize { value }"
                    }
                }
            }
        }),
        source_frame: b"pub fn budget_entry(value: usize) -> usize { value }".to_vec(),
        truncated: false,
        continuation_available: false,
    };

    let answer = synthesize_answer(&task, &[observed]);

    assert_eq!(answer.task_sha256, task.task_sha256);
    assert_eq!(answer.seed, 17);
    assert_eq!(
        answer.observed_symbol_ids,
        BTreeSet::from(["sym1_observed_entry".to_owned()])
    );
    assert!(
        !answer
            .observed_symbol_ids
            .contains("sym1_unobserved_helper")
    );
    assert!(answer.source_text.contains("budget_entry"));
    assert!(answer_contains_semantic_term(&answer, "budget_entry"));
    assert!(!answer_contains_semantic_term(&answer, "budget_helper"));
    assert!(answer.source_reference_observed);
    assert!(answer.all_calls_succeeded);
    assert!(answer.partial_result_disclosed);
}

#[test]
fn structured_field_names_without_expected_values_do_not_earn_facts() {
    let mut task = fixture_consumer_task(17);
    task.family = TrajectoryWorkflowFamily::CallRelationships;
    task.rootlight_tools = vec!["symbol.relationships".to_owned()];
    let key = WorkflowAnswerKey {
        workflow_id: task.workflow_id.clone(),
        attempt_index: 0,
        deterministic_seed: task.seed,
        entity_ids: BTreeMap::from([
            ("budget_entry".to_owned(), "sym1_expected_entry".to_owned()),
            (
                "budget_helper".to_owned(),
                "sym1_expected_helper".to_owned(),
            ),
        ]),
        generation_ids: BTreeMap::new(),
        repository_ids: BTreeMap::new(),
        required_facts: facts(&[
            edge_fact("budget_entry", "budget_helper").as_str(),
            entity_fact("budget_entry").as_str(),
            entity_fact("budget_helper").as_str(),
        ]),
        actionable_facts: BTreeSet::new(),
    };
    let wrong_call = ObservableCall {
        tool: "symbol.relationships".to_owned(),
        response: json!({
            "groups": [{
                "seed": "sym1_wrong_entry",
                "relation": "calls",
                "direction": "outbound",
                "items": [{"symbol_id": "sym1_wrong_helper"}]
            }]
        }),
        source_frame: Vec::new(),
        truncated: false,
        continuation_available: false,
    };
    let wrong_execution = ObservableExecution {
        attempt_index: 0,
        task: task.clone(),
        answer: synthesize_answer(&task, std::slice::from_ref(&wrong_call)),
        calls: vec![wrong_call],
    };

    assert!(
        observed_answer_facts(&wrong_execution, &key).is_empty(),
        "field names and schema shape cannot substitute for preregistered values"
    );

    let exact_call = ObservableCall {
        tool: "symbol.relationships".to_owned(),
        response: json!({
            "groups": [{
                "seed": "sym1_expected_entry",
                "relation": "calls",
                "direction": "outbound",
                "items": [{"symbol_id": "sym1_expected_helper"}]
            }]
        }),
        source_frame: Vec::new(),
        truncated: false,
        continuation_available: false,
    };
    let exact_execution = ObservableExecution {
        attempt_index: 0,
        task: task.clone(),
        answer: synthesize_answer(&task, std::slice::from_ref(&exact_call)),
        calls: vec![exact_call],
    };
    let observed = observed_answer_facts(&exact_execution, &key);

    assert!(observed.contains(&edge_fact("budget_entry", "budget_helper")));
    assert!(observed.contains(&entity_fact("budget_entry")));
    assert!(observed.contains(&entity_fact("budget_helper")));

    let inbound_call = ObservableCall {
        tool: "symbol.relationships".to_owned(),
        response: json!({
            "groups": [{
                "seed": "sym1_expected_helper",
                "relation": "calls",
                "direction": "inbound",
                "items": [{"symbol_id": "sym1_expected_entry"}]
            }]
        }),
        source_frame: Vec::new(),
        truncated: false,
        continuation_available: false,
    };
    let inbound_execution = ObservableExecution {
        attempt_index: 0,
        task: task.clone(),
        answer: synthesize_answer(&task, std::slice::from_ref(&inbound_call)),
        calls: vec![inbound_call],
    };
    let observed = observed_answer_facts(&inbound_execution, &key);

    assert!(observed.contains(&edge_fact("budget_entry", "budget_helper")));
    assert!(observed.contains(&entity_fact("budget_entry")));
    assert!(observed.contains(&entity_fact("budget_helper")));
}

#[test]
fn test_rationale_requires_the_exact_test_and_a_nonempty_reason() {
    assert!(object_with_identity_has_nonempty_reason(
        &json!({
            "test_id": "test_expected",
            "why": ["direct_test_edge"]
        }),
        "test_expected"
    ));
    assert!(!object_with_identity_has_nonempty_reason(
        &json!({
            "test_id": "test_expected",
            "why": []
        }),
        "test_expected"
    ));
    assert!(!object_with_identity_has_nonempty_reason(
        &json!({
            "tests": [
                {"test_id": "test_expected"},
                {"test_id": "test_other", "why": ["direct_test_edge"]}
            ]
        }),
        "test_expected"
    ));
}

#[test]
fn architecture_connection_requires_exact_file_components_and_supported_edge() {
    let response = json!({
        "data": {
            "components": [
                {"id": "file-lib", "kind": "file", "name": "src/lib.rs"},
                {"id": "file-service", "kind": "file", "name": "src/service.rs"}
            ],
            "connections": [{
                "from": "file-lib",
                "to": "file-service",
                "kind": "calls",
                "weight": 1,
                "confidence": 900
            }]
        }
    });

    assert!(architecture_response_contains_connection(
        &response,
        "src/lib.rs",
        "src/service.rs",
        "calls"
    ));
    assert!(!architecture_response_contains_connection(
        &response,
        "src/service.rs",
        "src/lib.rs",
        "calls"
    ));
    assert!(!architecture_response_contains_connection(
        &response,
        "src/lib.rs",
        "src/service.rs",
        "imports"
    ));
}

#[test]
fn concrete_test_path_is_accepted_as_evidence_reference() {
    assert!(contains_concrete_evidence_reference(&json!({
        "test_id": "sym1_test",
        "path": "src/lib.rs"
    })));
    assert!(!contains_concrete_evidence_reference(&json!({
        "test_id": "sym1_test",
        "path": ""
    })));
}

#[test]
#[ignore = "retained real-process trajectory evidence is generated by its dedicated CI job"]
fn preregistered_trajectories_run_through_daemon_and_mcp_processes() {
    let fixture = fixture_root();
    let isolated = process_support::private_process_tempdir("rl-trajectory-");
    let fixture_workspace = isolated.path().join("fixture");
    let repository_root = fixture_workspace.join("runtime-service");
    let consumer_root = fixture_workspace.join("consumer-service");
    copy_regular_tree(&fixture, &repository_root);
    augment_runtime_service_fixture(&repository_root);
    create_consumer_service_fixture(&consumer_root, &repository_root);
    let base_fixture_sha256 = fixture_digest(&repository_root);
    let consumer_fixture_sha256 = fixture_digest(&consumer_root);
    let state_dir = isolated.path().join("state");
    let runtime_dir = isolated.path().join("runtime");

    let mut daemon = DaemonProcess::spawn(&state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&state_dir, &runtime_dir);
    let first = index_repository(&mut mcp, &repository_root, "trajectory-index-v1");
    fs::OpenOptions::new()
        .append(true)
        .open(repository_root.join("src").join("lib.rs"))
        .and_then(|mut file| {
            file.write_all(
                b"\npub fn trajectory_added(value: usize) -> usize {\n    value.saturating_add(1)\n}\n",
            )
        })
        .expect("fixture generation mutation is written");
    let fixture_sha256 = trajectory_fixture_digest(&[
        &base_fixture_sha256,
        &fixture_digest(&repository_root),
        &consumer_fixture_sha256,
    ]);
    let second = index_repository(&mut mcp, &repository_root, "trajectory-index-v2");
    let consumer = index_repository(&mut mcp, &consumer_root, "trajectory-consumer-index-v1");
    let entry = locate(&mut mcp, &second, "budget_entry", "trajectory-entry");
    let helper = locate(&mut mcp, &second, "budget_helper", "trajectory-helper");
    let unused = locate(&mut mcp, &second, "budget_unused", "trajectory-unused");
    let added = locate(&mut mcp, &second, "trajectory_added", "trajectory-added");
    let transform = locate(&mut mcp, &second, "transform", "trajectory-transform");
    let gateway = locate(
        &mut mcp,
        &second,
        "submit_budget_request",
        "trajectory-gateway",
    );
    let worker = locate(
        &mut mcp,
        &second,
        "handle_budget_message",
        "trajectory-worker",
    );
    let cycle_alpha = locate(&mut mcp, &second, "cycle_alpha", "trajectory-cycle-alpha");
    let cycle_beta = locate(&mut mcp, &second, "cycle_beta", "trajectory-cycle-beta");
    let unit_test = locate(
        &mut mcp,
        &second,
        "entry_combines_bounded_helpers",
        "trajectory-unit-test",
    );
    let consumer_migration = locate(
        &mut mcp,
        &consumer,
        "migrate_budget_api",
        "trajectory-consumer-migration",
    );
    let consumer_helper = locate(
        &mut mcp,
        &consumer,
        "client_transform",
        "trajectory-consumer-helper",
    );

    let protocol = preregistered_trajectory_protocol(fixture_sha256)
        .expect("protocol preregistration is valid");
    let source_revision = source_revision();
    let mut rootlight = RootlightProcessAdapter {
        mcp: &mut mcp,
        first,
        second,
        consumer,
        entry,
        helper,
        unused,
        added,
        transform,
        gateway,
        worker,
        cycle_alpha,
        cycle_beta,
        unit_test,
        consumer_migration,
        consumer_helper,
        workflow_observations: Vec::new(),
    };
    let held_out_keys = held_out_answer_keys(&protocol, &rootlight);
    let quality_registrations = quality_task_registrations(&protocol, &held_out_keys);
    let quality_protocol =
        preregister_workflow_quality_protocol(&protocol, &source_revision, quality_registrations)
            .expect("workflow quality protocol is frozen before candidate execution");
    let mut codebase_memory = UnavailableTrajectoryAdapter::new(
        TrajectoryCondition::CodebaseMemory,
        "codebase_memory_process_v1",
        "executable_not_available",
    )
    .expect("unavailable executable adapter is valid");
    let mut bounded_files = BoundedFileExplorationAdapter::new(&fixture_workspace);
    let tokenizer = O200kTrajectoryTokenizer::new().expect("pinned tokenizer initializes");
    let package = run_trajectory_suite(
        protocol,
        &mut rootlight,
        &mut codebase_memory,
        &mut bounded_files,
        &tokenizer,
    )
    .expect("complete trajectory package validates");
    let bounded_observations = bounded_files.take_observations();
    let unsuccessful_rootlight = package
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.condition == TrajectoryCondition::Rootlight
                && !matches!(attempt.outcome, TrajectoryAttemptOutcome::Succeeded)
        })
        .map(|attempt| {
            (
                &attempt.workflow_id,
                attempt.attempt_index,
                &attempt.outcome,
            )
        })
        .collect::<Vec<_>>();
    let public_errors = rootlight
        .workflow_observations
        .iter()
        .flat_map(|execution| &execution.calls)
        .filter(|call| {
            call.response["result"]["isError"] == true
                || call.response.get("error").is_some_and(Value::is_object)
        })
        .map(|call| (&call.tool, &call.response))
        .collect::<Vec<_>>();
    assert!(
        unsuccessful_rootlight.is_empty(),
        "every preregistered Rootlight workflow must succeed: attempts={unsuccessful_rootlight:#?}; errors={public_errors:#?}"
    );
    let quality_measurements = workflow_quality_measurements(
        &package,
        &quality_protocol,
        &rootlight.workflow_observations,
        &bounded_observations,
        &held_out_keys,
        regular_files(&fixture_workspace).len(),
    );
    let quality = build_workflow_quality_evidence(&package, quality_protocol, quality_measurements)
        .expect("candidate-bound all-workflow quality evidence validates");
    assert_eq!(quality.denominator.expected_workflows, 14);
    assert_eq!(quality.denominator.observed_workflows, 14);
    assert_eq!(quality.denominator.expected_pairs, 28);
    assert_eq!(quality.denominator.observed_pairs, 28);
    assert_eq!(quality.denominator.rootlight_graded, 28);
    assert_eq!(quality.denominator.bounded_file_graded, 28);
    let rootlight_missing_facts = rootlight
        .workflow_observations
        .iter()
        .filter_map(|execution| {
            let key = held_out_keys
                .get(&(execution.task.workflow_id.clone(), execution.attempt_index))
                .expect("Rootlight observation has a held-out key");
            let observed = observed_answer_facts(execution, key);
            let missing = key
                .required_facts
                .difference(&observed)
                .cloned()
                .collect::<BTreeSet<_>>();
            (!missing.is_empty()).then_some((
                execution.task.workflow_id.clone(),
                execution.attempt_index,
                missing,
            ))
        })
        .collect::<Vec<_>>();
    let cross_service_flow_responses = rootlight
        .workflow_observations
        .iter()
        .filter(|execution| execution.task.workflow_id == "cross-service-trace")
        .flat_map(|execution| &execution.calls)
        .filter(|call| call.tool == "flow.trace")
        .map(|call| &call.response)
        .collect::<Vec<_>>();
    assert!(
        quality.threshold_passed,
        "every workflow must retain Rootlight quality within two points of the task-driven baseline: summaries={:#?}; missing_facts={rootlight_missing_facts:#?}; cross_service_flow_responses={cross_service_flow_responses:#?}",
        quality.workflows,
    );
    assert!(
        quality.workflows.iter().all(
            |workflow| workflow.rootlight_quality_loss_centipoints <= 200
                && workflow.maximum_pair_loss_centipoints <= 200
        )
    );
    let failed_rootlight_attempts = package
        .attempts
        .iter()
        .filter(|attempt| {
            attempt.condition == TrajectoryCondition::Rootlight
                && !matches!(attempt.outcome, TrajectoryAttemptOutcome::Succeeded)
        })
        .map(|attempt| (&attempt.attempt_id, &attempt.outcome))
        .collect::<Vec<_>>();
    let context_failure_responses = rootlight
        .workflow_observations
        .iter()
        .flat_map(|execution| &execution.calls)
        .filter(|call| {
            call.response["result"]["isError"] == true
                || call.response.get("error").is_some_and(Value::is_object)
        })
        .map(|call| &call.response)
        .collect::<Vec<_>>();

    assert_eq!(package.denominator.expected_attempts, 84);
    assert_eq!(package.denominator.observed_attempts, 84);
    assert_eq!(
        package.denominator.succeeded, 56,
        "unexpected Rootlight failures: {failed_rootlight_attempts:#?}; \
         responses: {context_failure_responses:#?}"
    );
    assert_eq!(package.denominator.not_available, 28);
    assert_eq!(package.denominator.failed, 0);
    assert_eq!(package.denominator.timed_out, 0);
    assert_eq!(package.denominator.cancelled, 0);
    assert_eq!(package.denominator.excluded, 0);
    assert_eq!(package.denominator.redundant_status_preflights, 0);
    assert_eq!(
        package
            .attempts
            .iter()
            .filter(|attempt| attempt.condition == TrajectoryCondition::Rootlight)
            .count(),
        28
    );
    assert!(
        package
            .attempts
            .iter()
            .flat_map(|attempt| &attempt.calls)
            .all(|call| {
                call.accounting.request.actual_tokens.is_some()
                    && call.accounting.response.actual_tokens.is_some()
                    && call.accounting.source.actual_tokens.is_some()
                    && call.accounting.total.actual_tokens.is_some()
            })
    );
    let blinding_key = AblationBlindingKey::new([0x48; 32]);
    let ablation_protocol =
        preregister_context_pack_ablation(&package, &blinding_key, &source_revision)
            .expect("ablation protocol is preregistered before paired measurements");
    let mut prepared = prepare_blinded_ablation(&package, &ablation_protocol, &blinding_key)
        .expect("trajectory package prepares blinded candidates");
    let context_observations = rootlight
        .workflow_observations
        .iter()
        .filter(|observation| observation.task.family == TrajectoryWorkflowFamily::BugFixContext)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        context_observations.len(),
        ablation_protocol.attempt_seeds.len()
    );
    let mut direct_observations = Vec::with_capacity(ablation_protocol.attempt_seeds.len());
    for (attempt_index, seed) in ablation_protocol.attempt_seeds.iter().copied().enumerate() {
        let attempt_index = u16::try_from(attempt_index).expect("attempt index fits u16");
        let task = &context_observations[usize::from(attempt_index)].task;
        assert_eq!(
            task.seed, seed,
            "paired direct retrieval uses the preregistered attempt seed"
        );
        let (raw, observation) = rootlight.execute_direct_sequence(attempt_index, task);
        assert_attempt_within_bounds(&raw, ablation_protocol.shared_bounds, &tokenizer);
        let mut metrics = measured_metrics(&raw, &tokenizer);
        metrics.unsupported_claims = unsupported_claim_categories(&observation)
            .values()
            .copied()
            .try_fold(0_u32, u32::checked_add)
            .expect("unsupported-claim count fits u32");
        let tools = raw
            .calls
            .iter()
            .map(|call| call.tool.tool_id.clone())
            .collect::<Vec<_>>();
        prepared
            .add_direct_sequence_measurement(
                &ablation_protocol,
                &blinding_key,
                attempt_index,
                BlindedRunOutcome::from(&raw.outcome),
                metrics,
                &tools,
            )
            .expect("measured direct sequence completes its preregistered pair");
        direct_observations.push(observation);
    }
    let rubric_evidence = primary_rubric_evidence(
        &prepared.candidates,
        &prepared.pairing_map,
        &context_observations,
        &direct_observations,
        &rootlight.entry,
        &rootlight.helper,
    );
    let ablation = prepared
        .evaluate(ablation_protocol, rubric_evidence)
        .expect("complete paired ablation evidence evaluates");
    assert!(
        matches!(ablation.aggregate.decision, AblationDecision::Pass),
        "the context-pack candidate must pass the preregistered quality threshold: aggregate={:#?}, rubric={:#?}",
        ablation.aggregate,
        ablation.rubric_evidence,
    );
    assert!(
        ablation
            .aggregate
            .quality_loss_centipoints
            .is_some_and(|loss| loss <= 200),
        "context-pack quality loss must stay within the two-point product limit"
    );
    assert_eq!(ablation.aggregate.expected_pairs, 2);
    assert_eq!(ablation.aggregate.complete_quality_pairs, 2);
    assert!(ablation.aggregate.quality_retention_ppm.is_some());
    assert!(ablation.aggregate.uncertainty.is_some());
    assert_eq!(ablation.aggregate.sensitivity.context_ungraded, 0);
    assert_eq!(ablation.aggregate.sensitivity.direct_ungraded, 0);
    for variant in [
        AblationVariant::ContextPack,
        AblationVariant::DirectSequence,
    ] {
        let aggregate = ablation
            .aggregate
            .variants
            .iter()
            .find(|aggregate| aggregate.variant == variant)
            .expect("primary variant aggregate is retained");
        assert_eq!(aggregate.observed_attempts, 2);
        assert_eq!(aggregate.quality_graded, 2);
        assert!(aggregate.task_success_rate_ppm.is_some());
        assert!(aggregate.unsupported_claim_rate_ppm.is_some());
        assert!(aggregate.resource_totals.calls > 0);
        assert!(aggregate.resource_totals.tokens > 0);
        assert!(aggregate.resource_totals.elapsed_ns > 0);
        if variant == AblationVariant::ContextPack {
            assert!(
                aggregate.resource_totals.source_tokens > 0,
                "context.pack must retain source material in the measured response"
            );
        }
    }

    if let Some(path) = std::env::var_os(REPORT_ENV) {
        let encoded =
            encode_trajectory_evidence(&package).expect("retained trajectory evidence encodes");
        fs::write(path, encoded).expect("trajectory evidence report is written");
    }
    if let Some(path) = std::env::var_os(ABLATION_REPORT_ENV) {
        let encoded = encode_context_pack_ablation(&ablation).expect("ablation evidence encodes");
        fs::write(path, encoded).expect("ablation evidence report is written");
    }
    if let Some(path) = std::env::var_os(QUALITY_REPORT_ENV) {
        let encoded = encode_workflow_quality_evidence(&quality, &package)
            .expect("workflow quality evidence encodes");
        fs::write(path, encoded).expect("workflow quality evidence report is written");
    }
    drop(rootlight);
    mcp.finish();
    daemon.finish();
}

fn fixture_consumer_task(seed: u64) -> ConsumerTask {
    let protocol = preregistered_trajectory_protocol("ab".repeat(32))
        .expect("fixture trajectory protocol is valid");
    let workflow = protocol
        .workflows
        .iter()
        .find(|workflow| workflow.family == TrajectoryWorkflowFamily::BugFixContext)
        .expect("context task is preregistered");
    let task_sha256 = protocol
        .task_digest(&workflow.workflow_id)
        .expect("fixture task digest is available");
    ConsumerTask::from_execution(TrajectoryExecutionInput {
        workflow,
        task_sha256: &task_sha256,
        fixture_sha256: &protocol.fixture_sha256,
        attempt_index: 0,
        seed,
        bounds: protocol.bounds,
        stopping: protocol.stopping,
        retry: &protocol.retry,
    })
}

fn source_revision() -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .output()
        .expect("git source revision command starts");
    assert!(
        output.status.success(),
        "git source revision command succeeds"
    );
    let revision = String::from_utf8(output.stdout)
        .expect("git source revision is UTF-8")
        .trim()
        .to_owned();
    assert!(
        matches!(revision.len(), 40 | 64)
            && revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "git source revision is canonical"
    );
    revision
}

#[derive(Debug, Clone)]
struct WorkflowAnswerKey {
    workflow_id: String,
    attempt_index: u16,
    deterministic_seed: u64,
    entity_ids: BTreeMap<String, String>,
    generation_ids: BTreeMap<String, String>,
    repository_ids: BTreeMap<String, String>,
    required_facts: BTreeSet<String>,
    actionable_facts: BTreeSet<String>,
}

fn held_out_answer_keys(
    protocol: &TrajectoryProtocol,
    fixture: &RootlightProcessAdapter<'_>,
) -> BTreeMap<(String, u16), WorkflowAnswerKey> {
    let entity_ids = BTreeMap::from([
        ("budget_entry".to_owned(), fixture.entry.symbol_id.clone()),
        ("budget_helper".to_owned(), fixture.helper.symbol_id.clone()),
        ("budget_unused".to_owned(), fixture.unused.symbol_id.clone()),
        (
            "trajectory_added".to_owned(),
            fixture.added.symbol_id.clone(),
        ),
        ("transform".to_owned(), fixture.transform.symbol_id.clone()),
        (
            "submit_budget_request".to_owned(),
            fixture.gateway.symbol_id.clone(),
        ),
        (
            "handle_budget_message".to_owned(),
            fixture.worker.symbol_id.clone(),
        ),
        (
            "cycle_alpha".to_owned(),
            fixture.cycle_alpha.symbol_id.clone(),
        ),
        (
            "cycle_beta".to_owned(),
            fixture.cycle_beta.symbol_id.clone(),
        ),
        (
            "entry_combines_bounded_helpers".to_owned(),
            fixture.unit_test.symbol_id.clone(),
        ),
        (
            "migrate_budget_api".to_owned(),
            fixture.consumer_migration.symbol_id.clone(),
        ),
        (
            "client_transform".to_owned(),
            fixture.consumer_helper.symbol_id.clone(),
        ),
    ]);
    let repository_ids = BTreeMap::from([
        (
            "runtime-service".to_owned(),
            fixture.second.repository_id.clone(),
        ),
        (
            "consumer-service".to_owned(),
            fixture.consumer.repository_id.clone(),
        ),
    ]);
    let generation_ids = BTreeMap::from([
        ("base".to_owned(), fixture.first.generation_id.clone()),
        ("head".to_owned(), fixture.second.generation_id.clone()),
    ]);
    let mut keys = BTreeMap::new();
    for workflow in &protocol.workflows {
        for (attempt_index, seed) in protocol.attempt_seeds.iter().copied().enumerate() {
            let attempt_index = u16::try_from(attempt_index).expect("attempt index fits u16");
            let (primary, secondary) = seeded_target_names(seed);
            let required_facts = match workflow.family {
                TrajectoryWorkflowFamily::LocateImplementation => facts(&[
                    definition_fact(primary).as_str(),
                    entity_fact(primary).as_str(),
                    evidence_fact("src/lib.rs").as_str(),
                ]),
                TrajectoryWorkflowFamily::ExplainSymbol => facts(&[
                    definition_fact(primary).as_str(),
                    entity_fact(primary).as_str(),
                    reference_fact(primary).as_str(),
                ]),
                TrajectoryWorkflowFamily::CallRelationships => facts(&[
                    edge_fact("budget_entry", "budget_helper").as_str(),
                    entity_fact(primary).as_str(),
                    entity_fact(secondary).as_str(),
                ]),
                TrajectoryWorkflowFamily::BugFixContext => facts(&[
                    context_fact(primary).as_str(),
                    entity_fact(secondary).as_str(),
                    test_fact("entry_combines_bounded_helpers").as_str(),
                ]),
                TrajectoryWorkflowFamily::AssessChangeImpact => facts(&[
                    impact_fact("budget_entry", "budget_helper").as_str(),
                    plan_fact(primary).as_str(),
                    test_fact("entry_combines_bounded_helpers").as_str(),
                ]),
                TrajectoryWorkflowFamily::SelectTests => facts(&[
                    rationale_fact("entry_combines_bounded_helpers").as_str(),
                    test_fact("entry_combines_bounded_helpers").as_str(),
                ]),
                TrajectoryWorkflowFamily::ArchitectureOverview => facts(&[
                    component_fact("src/lib.rs").as_str(),
                    component_fact("src/service.rs").as_str(),
                    architecture_connection_fact("src/lib.rs", "src/service.rs").as_str(),
                ]),
                TrajectoryWorkflowFamily::CycleInvestigation => facts(&[
                    cycle_fact("cycle_alpha", "cycle_beta").as_str(),
                    flow_fact("cycle_alpha", "cycle_beta").as_str(),
                    plan_fact("cycle_alpha").as_str(),
                ]),
                TrajectoryWorkflowFamily::DeadCodeInvestigation => facts(&[
                    dead_fact("budget_unused").as_str(),
                    definition_fact("budget_unused").as_str(),
                    reference_fact("budget_unused").as_str(),
                ]),
                TrajectoryWorkflowFamily::CrossServiceTrace => facts(&[
                    context_fact("submit_budget_request").as_str(),
                    flow_fact("handle_budget_message", "transform").as_str(),
                    flow_fact("submit_budget_request", "handle_budget_message").as_str(),
                ]),
                TrajectoryWorkflowFamily::RefactoringBoundary => facts(&[
                    context_fact(primary).as_str(),
                    impact_fact("budget_entry", "budget_helper").as_str(),
                    plan_fact(primary).as_str(),
                    edge_fact("budget_entry", "budget_helper").as_str(),
                ]),
                TrajectoryWorkflowFamily::HistoryComparison => facts(&[
                    generation_fact("base").as_str(),
                    generation_fact("head").as_str(),
                    history_fact("trajectory_added").as_str(),
                    impact_fact("trajectory_added", "trajectory_added").as_str(),
                ]),
                TrajectoryWorkflowFamily::ApiMigrationBatch => facts(&[
                    batch_operation_fact("locate").as_str(),
                    batch_operation_fact("impact").as_str(),
                    batch_operation_fact("plan").as_str(),
                    entity_fact(primary).as_str(),
                ]),
                TrajectoryWorkflowFamily::MultiRepositoryMigration => facts(&[
                    context_fact("budget_entry").as_str(),
                    context_fact("migrate_budget_api").as_str(),
                    repository_fact("consumer-service").as_str(),
                    repository_fact("runtime-service").as_str(),
                    cross_repository_fact("migrate_budget_api", "budget_entry").as_str(),
                ]),
            };
            let actionable_facts = required_facts
                .iter()
                .filter(|fact| {
                    [
                        "batch:",
                        "context:",
                        "connection:",
                        "cross_repository:",
                        "cycle:",
                        "dead:",
                        "edge:",
                        "flow:",
                        "history:",
                        "impact:",
                        "plan:",
                        "test:",
                    ]
                    .iter()
                    .any(|prefix| fact.starts_with(prefix))
                })
                .cloned()
                .collect();
            let key = WorkflowAnswerKey {
                workflow_id: workflow.workflow_id.clone(),
                attempt_index,
                deterministic_seed: seed,
                entity_ids: entity_ids.clone(),
                generation_ids: generation_ids.clone(),
                repository_ids: repository_ids.clone(),
                required_facts,
                actionable_facts,
            };
            assert!(
                keys.insert((workflow.workflow_id.clone(), attempt_index), key)
                    .is_none(),
                "held-out answer key is unique"
            );
        }
    }
    keys
}

fn facts(values: &[&str]) -> BTreeSet<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn entity_fact(name: &str) -> String {
    format!("entity:{name}")
}

fn definition_fact(name: &str) -> String {
    format!("definition:{name}")
}

fn evidence_fact(path: &str) -> String {
    format!("evidence:{path}")
}

fn reference_fact(name: &str) -> String {
    format!("reference:{name}")
}

fn edge_fact(source: &str, target: &str) -> String {
    format!("edge:{source}:calls:{target}")
}

fn context_fact(name: &str) -> String {
    format!("context:{name}")
}

fn impact_fact(source: &str, target: &str) -> String {
    format!("impact:{source}:{target}")
}

fn test_fact(name: &str) -> String {
    format!("test:{name}")
}

fn rationale_fact(name: &str) -> String {
    format!("rationale:{name}")
}

fn component_fact(path: &str) -> String {
    format!("component:{path}")
}

fn architecture_connection_fact(source: &str, target: &str) -> String {
    format!("connection:{source}:calls:{target}")
}

fn cycle_fact(first: &str, second: &str) -> String {
    format!("cycle:{first}:{second}")
}

fn flow_fact(source: &str, target: &str) -> String {
    format!("flow:{source}:{target}")
}

fn plan_fact(name: &str) -> String {
    format!("plan:{name}")
}

fn dead_fact(name: &str) -> String {
    format!("dead:{name}")
}

fn generation_fact(state: &str) -> String {
    format!("generation:{state}")
}

fn history_fact(name: &str) -> String {
    format!("history:{name}")
}

fn batch_operation_fact(operation: &str) -> String {
    format!("batch:{operation}")
}

fn repository_fact(name: &str) -> String {
    format!("repository:{name}")
}

fn cross_repository_fact(source: &str, target: &str) -> String {
    format!("cross_repository:{source}:{target}")
}

fn seeded_target_names(seed: u64) -> (&'static str, &'static str) {
    if seed % 5 >= 3 {
        ("budget_helper", "budget_entry")
    } else {
        ("budget_entry", "budget_helper")
    }
}

fn quality_task_registrations(
    protocol: &TrajectoryProtocol,
    answer_keys: &BTreeMap<(String, u16), WorkflowAnswerKey>,
) -> Vec<WorkflowQualityTaskRegistration> {
    let mut registrations = Vec::new();
    for workflow in &protocol.workflows {
        for (attempt_index, seed) in protocol.attempt_seeds.iter().copied().enumerate() {
            let attempt_index = u16::try_from(attempt_index).expect("attempt index fits u16");
            let answer_key = answer_keys
                .get(&(workflow.workflow_id.clone(), attempt_index))
                .expect("held-out key exists for every task");
            assert_eq!(answer_key.deterministic_seed, seed);
            let prompt = trajectory_task_prompt(workflow.family, seed);
            registrations.push(WorkflowQualityTaskRegistration {
                workflow_id: workflow.workflow_id.clone(),
                attempt_index,
                prompt_sha256: sha256_hex(prompt.as_bytes()),
                answer_key_sha256: held_out_key_sha256(answer_key),
            });
        }
    }
    registrations
}

fn held_out_key_sha256(key: &WorkflowAnswerKey) -> String {
    let bytes = serde_json::to_vec(&json!({
        "workflow_id": key.workflow_id,
        "attempt_index": key.attempt_index,
        "deterministic_seed": key.deterministic_seed,
        "entity_ids": key.entity_ids,
        "generation_ids": key.generation_ids,
        "repository_ids": key.repository_ids,
        "required_facts": key.required_facts,
        "actionable_facts": key.actionable_facts,
    }))
    .expect("held-out answer key serializes");
    sha256_hex(&bytes)
}

fn workflow_quality_measurements(
    package: &TrajectoryEvidencePackage,
    quality_protocol: &WorkflowQualityProtocol,
    rootlight_observations: &[ObservableExecution],
    bounded_observations: &[BoundedFileObservation],
    answer_keys: &BTreeMap<(String, u16), WorkflowAnswerKey>,
    fixture_file_count: usize,
) -> Vec<WorkflowQualityPairMeasurement> {
    assert_eq!(rootlight_observations.len(), quality_protocol.tasks.len());
    assert_eq!(bounded_observations.len(), quality_protocol.tasks.len());
    let rootlight = rootlight_observations
        .iter()
        .map(|observation| {
            (
                (
                    observation.task.workflow_id.clone(),
                    observation.attempt_index,
                ),
                observation,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let bounded = bounded_observations
        .iter()
        .map(|observation| {
            (
                (observation.workflow_id.clone(), observation.attempt_index),
                observation,
            )
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(rootlight.len(), quality_protocol.tasks.len());
    assert_eq!(bounded.len(), quality_protocol.tasks.len());

    let distinct_selections = bounded_observations
        .iter()
        .map(|observation| observation.selected_paths.clone())
        .collect::<BTreeSet<_>>();
    assert!(
        distinct_selections.len() >= 3,
        "bounded exploration must select files according to the task family"
    );
    assert!(bounded_observations.iter().all(|observation| {
        !observation.selected_paths.is_empty()
            && observation.selected_paths.len() < fixture_file_count
    }));

    quality_protocol
        .tasks
        .iter()
        .map(|task| {
            let key = answer_keys
                .get(&(task.workflow_id.clone(), task.attempt_index))
                .expect("held-out answer key remains available only to the grader");
            assert_eq!(held_out_key_sha256(key), task.answer_key_sha256);
            let rootlight_observation = rootlight
                .get(&(task.workflow_id.clone(), task.attempt_index))
                .copied()
                .expect("Rootlight observation covers every task");
            assert_eq!(
                sha256_hex(rootlight_observation.task.prompt.as_bytes()),
                task.prompt_sha256,
                "Rootlight executes the exact preregistered prompt"
            );
            let bounded_observation = bounded
                .get(&(task.workflow_id.clone(), task.attempt_index))
                .copied()
                .expect("bounded observation covers every task");
            let bounded_execution = bounded_execution(package, task, bounded_observation);
            let rootlight_attempt = quality_attempt(
                package,
                &task.workflow_id,
                task.attempt_index,
                TrajectoryCondition::Rootlight,
            );
            let bounded_attempt = quality_attempt(
                package,
                &task.workflow_id,
                task.attempt_index,
                TrajectoryCondition::BoundedFileExploration,
            );
            assert_eq!(
                bounded_attempt.calls[0].accounting.source.input_sha256,
                sha256_hex(&bounded_observation.source_frame),
                "bounded grade consumes the exact source measured by the trajectory"
            );
            WorkflowQualityPairMeasurement {
                workflow_id: task.workflow_id.clone(),
                attempt_index: task.attempt_index,
                rootlight: grade_workflow_candidate(
                    TrajectoryCondition::Rootlight,
                    &rootlight_attempt.attempt_id,
                    rootlight_observation,
                    key,
                ),
                bounded_file_exploration: grade_workflow_candidate(
                    TrajectoryCondition::BoundedFileExploration,
                    &bounded_attempt.attempt_id,
                    &bounded_execution,
                    key,
                ),
            }
        })
        .collect()
}

fn bounded_execution(
    package: &TrajectoryEvidencePackage,
    task: &rootlight_bench::WorkflowQualityTaskProtocol,
    observation: &BoundedFileObservation,
) -> ObservableExecution {
    let workflow = package
        .protocol
        .workflows
        .iter()
        .find(|workflow| workflow.workflow_id == task.workflow_id)
        .expect("quality workflow exists in the trajectory protocol");
    assert_eq!(observation.task_sha256, task.task_sha256);
    assert_eq!(observation.fixture_sha256, task.fixture_sha256);
    assert_eq!(observation.deterministic_seed, task.deterministic_seed);
    assert_eq!(observation.prompt_sha256, task.prompt_sha256);
    let consumer_task = ConsumerTask::from_execution(TrajectoryExecutionInput {
        workflow,
        task_sha256: &task.task_sha256,
        fixture_sha256: &task.fixture_sha256,
        attempt_index: task.attempt_index,
        seed: task.deterministic_seed,
        bounds: package.protocol.bounds,
        stopping: package.protocol.stopping,
        retry: &package.protocol.retry,
    });
    let calls = vec![ObservableCall {
        tool: "bounded_file.explore".to_owned(),
        response: observation.response.clone(),
        source_frame: observation.source_frame.clone(),
        truncated: observation.response["truncated"] == true,
        continuation_available: false,
    }];
    let answer = synthesize_answer(&consumer_task, &calls);
    ObservableExecution {
        attempt_index: task.attempt_index,
        task: consumer_task,
        calls,
        answer,
    }
}

fn quality_attempt<'a>(
    package: &'a TrajectoryEvidencePackage,
    workflow_id: &str,
    attempt_index: u16,
    condition: TrajectoryCondition,
) -> &'a rootlight_bench::TrajectoryAttemptRecord {
    package
        .attempts
        .iter()
        .find(|attempt| {
            attempt.workflow_id == workflow_id
                && attempt.attempt_index == attempt_index
                && attempt.condition == condition
        })
        .expect("quality candidate is retained in the trajectory denominator")
}

fn grade_workflow_candidate(
    condition: TrajectoryCondition,
    attempt_id: &str,
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
) -> WorkflowQualityCandidateMeasurement {
    assert_eq!(execution.task.workflow_id, key.workflow_id);
    assert_eq!(execution.attempt_index, key.attempt_index);
    assert_eq!(execution.task.seed, key.deterministic_seed);
    let observed_facts = observed_answer_facts(execution, key);
    let correctness = key.required_facts.is_subset(&observed_facts);
    let completeness = correctness
        && execution.answer.all_calls_succeeded
        && execution.answer.partial_result_disclosed;
    let evidence_support = correctness && evidence_reference_values_observed(execution, key);
    let uncertainty = execution.answer.partial_result_disclosed;
    let actionability = key.actionable_facts.is_subset(&observed_facts);
    let source_relevance = evidence_support
        && observed_facts
            .iter()
            .any(|fact| key.required_facts.contains(fact));
    let expected_tools = match condition {
        TrajectoryCondition::Rootlight => execution.task.rootlight_tools.clone(),
        TrajectoryCondition::BoundedFileExploration => {
            vec!["bounded_file.explore".to_owned()]
        }
        TrajectoryCondition::CodebaseMemory => unreachable!("quality compares required candidates"),
    };
    let task_adherence = execution.answer.observed_tools == expected_tools
        && execution.answer.task_sha256 == execution.task.task_sha256
        && execution.answer.fixture_sha256 == execution.task.fixture_sha256
        && execution.answer.seed == execution.task.seed;
    let dimensions = [
        (WorkflowQualityDimension::Correctness, 2_500, correctness),
        (WorkflowQualityDimension::Completeness, 2_000, completeness),
        (
            WorkflowQualityDimension::EvidenceSupport,
            2_000,
            evidence_support,
        ),
        (
            WorkflowQualityDimension::UncertaintyHandling,
            1_000,
            uncertainty,
        ),
        (
            WorkflowQualityDimension::Actionability,
            1_000,
            actionability,
        ),
        (
            WorkflowQualityDimension::SourceRelevance,
            1_000,
            source_relevance,
        ),
        (WorkflowQualityDimension::TaskAdherence, 500, task_adherence),
    ]
    .into_iter()
    .map(
        |(dimension, maximum_centipoints, passed)| WorkflowQualityDimensionScore {
            dimension,
            earned_centipoints: if passed { maximum_centipoints } else { 0 },
            maximum_centipoints,
        },
    )
    .collect();
    WorkflowQualityCandidateMeasurement {
        condition,
        attempt_id: attempt_id.to_owned(),
        candidate_sha256: candidate_sha256(execution, &observed_facts),
        dimensions,
    }
}

fn evidence_reference_values_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
) -> bool {
    if !execution.answer.source_text.is_empty() {
        return true;
    }
    execution.calls.iter().any(|call| {
        let response_has_expected_value = key
            .entity_ids
            .values()
            .chain(key.generation_ids.values())
            .chain(key.repository_ids.values())
            .any(|expected| value_contains_exact_string(&call.response, expected))
            || key.required_facts.iter().any(|fact| {
                fact.strip_prefix("evidence:")
                    .is_some_and(|path| value_contains_string_suffix(&call.response, path))
            });
        response_has_expected_value && contains_concrete_evidence_reference(&call.response)
    })
}

fn contains_concrete_evidence_reference(value: &Value) -> bool {
    match value {
        Value::Object(fields) => {
            let concrete = fields.iter().any(|(name, value)| {
                matches!(
                    name.as_str(),
                    "source_ref"
                        | "source_refs"
                        | "source_references"
                        | "references"
                        | "evidence_refs"
                        | "path"
                        | "provenance"
                        | "definition"
                ) && value_contains_nonempty_string(value)
            });
            concrete || fields.values().any(contains_concrete_evidence_reference)
        }
        Value::Array(values) => values.iter().any(contains_concrete_evidence_reference),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn value_contains_nonempty_string(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.is_empty(),
        Value::Array(values) => values.iter().any(value_contains_nonempty_string),
        Value::Object(fields) => fields.values().any(value_contains_nonempty_string),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn candidate_sha256(execution: &ObservableExecution, observed_facts: &BTreeSet<String>) -> String {
    let call_digests = execution
        .calls
        .iter()
        .map(|call| {
            let response = serde_json::to_vec(&call.response)
                .expect("observable candidate response serializes");
            json!({
                "tool": call.tool,
                "response_sha256": sha256_hex(&response),
                "source_sha256": sha256_hex(&call.source_frame),
                "truncated": call.truncated,
                "continuation_available": call.continuation_available,
            })
        })
        .collect::<Vec<_>>();
    let candidate = serde_json::to_vec(&json!({
        "task_id": execution.task.task_id,
        "workflow_id": execution.task.workflow_id,
        "task_sha256": execution.task.task_sha256,
        "fixture_sha256": execution.task.fixture_sha256,
        "attempt_index": execution.attempt_index,
        "seed": execution.task.seed,
        "prompt_sha256": sha256_hex(execution.task.prompt.as_bytes()),
        "observed_facts": observed_facts,
        "calls": call_digests,
    }))
    .expect("source-free candidate binding serializes");
    sha256_hex(&candidate)
}

fn observed_answer_facts(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
) -> BTreeSet<String> {
    let source = execution.answer.source_text.to_ascii_lowercase();
    let mut observed = BTreeSet::new();

    for (name, symbol_id) in &key.entity_ids {
        if execution
            .calls
            .iter()
            .any(|call| value_contains_exact_string(&call.response, symbol_id))
            || source_declares(&source, name)
        {
            observed.insert(entity_fact(name));
        }
    }
    for fact in &key.required_facts {
        let Some((kind, value)) = fact.split_once(':') else {
            continue;
        };
        let matched = match kind {
            "definition" => definition_observed(execution, key, value, &source),
            "evidence" => evidence_path_observed(execution, value, &source),
            "reference" => reference_observed(execution, key, value, &source),
            "edge" => parse_three_part(value).is_some_and(|(from, relation, to)| {
                relation == "calls" && relationship_observed(execution, key, from, to, &source)
            }),
            "context" => context_observed(execution, key, value, &source),
            "impact" => parse_pair(value)
                .is_some_and(|(from, to)| impact_observed(execution, key, from, to, &source)),
            "test" => test_observed(execution, key, value, &source),
            "rationale" => test_rationale_observed(execution, key, value, &source),
            "component" => component_observed(execution, value, &source),
            "connection" => parse_three_part(value).is_some_and(|(from, relation, to)| {
                architecture_connection_observed(execution, from, to, relation)
            }),
            "cycle" => parse_pair(value).is_some_and(|(first, second)| {
                cycle_observed(execution, key, first, second, &source)
            }),
            "flow" => parse_pair(value)
                .is_some_and(|(from, to)| flow_observed(execution, key, from, to, &source)),
            "plan" => plan_observed(execution, key, value, &source),
            "dead" => dead_candidate_observed(execution, key, value, &source),
            "generation" => generation_observed(execution, key, value),
            "history" => history_observed(execution, key, value, &source),
            "batch" => batch_operation_observed(execution, value),
            "repository" => repository_observed(execution, key, value, &source),
            "cross_repository" => parse_pair(value).is_some_and(|(from, to)| {
                cross_repository_observed(execution, key, from, to, &source)
            }),
            "entity" => observed.contains(fact),
            _ => false,
        };
        if matched {
            observed.insert(fact.clone());
        }
    }
    observed
}

fn parse_pair(value: &str) -> Option<(&str, &str)> {
    value.split_once(':')
}

fn parse_three_part(value: &str) -> Option<(&str, &str, &str)> {
    let (first, remainder) = value.split_once(':')?;
    let (second, third) = remainder.split_once(':')?;
    Some((first, second, third))
}

fn source_declares(source: &str, name: &str) -> bool {
    source.contains(&format!("fn {name}("))
}

fn source_calls(source: &str, caller: &str, callee: &str) -> bool {
    let Some(start) = source.find(&format!("fn {caller}(")) else {
        return false;
    };
    let remainder = &source[start..];
    let end = remainder.find("\n}").unwrap_or(remainder.len());
    remainder[..end].contains(&format!("{callee}("))
}

fn entity_id<'a>(key: &'a WorkflowAnswerKey, name: &str) -> Option<&'a str> {
    key.entity_ids.get(name).map(String::as_str)
}

fn tool_responses<'a>(
    execution: &'a ObservableExecution,
    tool: &'a str,
) -> impl Iterator<Item = &'a Value> {
    execution
        .calls
        .iter()
        .filter(move |call| call.tool == tool)
        .map(|call| &call.response)
}

fn value_contains_exact_string(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(actual) => actual == expected,
        Value::Array(values) => values
            .iter()
            .any(|value| value_contains_exact_string(value, expected)),
        Value::Object(fields) => fields
            .values()
            .any(|value| value_contains_exact_string(value, expected)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn definition_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "symbol.explain")
        .any(|response| value_contains_exact_string(response, symbol_id))
        || source_declares(source, name)
}

fn evidence_path_observed(execution: &ObservableExecution, path: &str, source: &str) -> bool {
    execution.calls.iter().any(|call| {
        value_contains_exact_string(&call.response, path)
            || value_contains_string_suffix(&call.response, path)
    }) || source.contains(&path.to_ascii_lowercase())
}

fn reference_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    execution
        .calls
        .iter()
        .any(|call| object_contains_expected_and_reference(&call.response, symbol_id))
        || source_declares(source, name)
}

fn object_contains_expected_and_reference(value: &Value, expected: &str) -> bool {
    match value {
        Value::Object(fields) => {
            let concrete = value_contains_exact_string(value, expected)
                && fields.values().any(contains_concrete_evidence_reference);
            concrete
                || fields
                    .values()
                    .any(|value| object_contains_expected_and_reference(value, expected))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| object_contains_expected_and_reference(value, expected)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn value_contains_string_suffix(value: &Value, expected: &str) -> bool {
    match value {
        Value::String(actual) => actual.replace('\\', "/").ends_with(expected),
        Value::Array(values) => values
            .iter()
            .any(|value| value_contains_string_suffix(value, expected)),
        Value::Object(fields) => fields
            .values()
            .any(|value| value_contains_string_suffix(value, expected)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn relationship_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    from: &str,
    to: &str,
    source: &str,
) -> bool {
    let (Some(from_id), Some(to_id)) = (entity_id(key, from), entity_id(key, to)) else {
        return false;
    };
    tool_responses(execution, "symbol.relationships")
        .any(|response| relationship_group_contains(response, from_id, to_id))
        || tool_responses(execution, "architecture.overview").any(|response| {
            value_contains_exact_string(response, from_id)
                && value_contains_exact_string(response, to_id)
                && value_contains_exact_string(response, "calls")
        })
        || source_calls(source, from, to)
}

fn relationship_group_contains(value: &Value, from_id: &str, to_id: &str) -> bool {
    match value {
        Value::Object(fields) => {
            let seed = fields.get("seed").and_then(Value::as_str);
            let direction = fields.get("direction").and_then(Value::as_str);
            let items = fields.get("items");
            let direct_group = fields.get("relation").and_then(Value::as_str) == Some("calls")
                && match direction {
                    Some("outbound") => {
                        seed == Some(from_id)
                            && items.is_some_and(|items| value_contains_exact_string(items, to_id))
                    }
                    Some("inbound") => {
                        seed == Some(to_id)
                            && items
                                .is_some_and(|items| value_contains_exact_string(items, from_id))
                    }
                    _ => false,
                };
            direct_group
                || fields
                    .values()
                    .any(|value| relationship_group_contains(value, from_id, to_id))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| relationship_group_contains(value, from_id, to_id)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn context_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "context.pack")
        .any(|response| value_contains_exact_string(response, symbol_id))
        || source_declares(source, name)
}

fn impact_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    from: &str,
    to: &str,
    source: &str,
) -> bool {
    let (Some(from_id), Some(to_id)) = (entity_id(key, from), entity_id(key, to)) else {
        return false;
    };
    tool_responses(execution, "change.impact").any(|response| {
        value_contains_exact_string(response, from_id)
            && (from_id == to_id || value_contains_exact_string(response, to_id))
    }) || source_calls(source, from, to)
        || from == to && source_declares(source, from)
}

fn test_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "tests.select")
        .any(|response| value_contains_exact_string(response, symbol_id))
        || tool_responses(execution, "context.pack")
            .any(|response| value_contains_exact_string(response, symbol_id))
        || source_declares(source, name)
}

fn test_rationale_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "tests.select")
        .any(|response| object_with_identity_has_nonempty_reason(response, symbol_id))
        || source_declares(source, name)
}

fn object_with_identity_has_nonempty_reason(value: &Value, identity: &str) -> bool {
    match value {
        Value::Object(fields) => {
            let identifies = fields
                .values()
                .any(|value| value_contains_exact_string(value, identity));
            let has_reason = fields.iter().any(|(name, value)| {
                matches!(name.as_str(), "reason" | "rationale" | "why")
                    && match value {
                        Value::String(text) => !text.is_empty(),
                        Value::Array(reasons) => reasons
                            .iter()
                            .any(|reason| reason.as_str().is_some_and(|text| !text.is_empty())),
                        _ => false,
                    }
            });
            identifies && has_reason
                || fields
                    .values()
                    .any(|value| object_with_identity_has_nonempty_reason(value, identity))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| object_with_identity_has_nonempty_reason(value, identity)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn component_observed(execution: &ObservableExecution, path: &str, source: &str) -> bool {
    tool_responses(execution, "architecture.overview")
        .any(|response| value_contains_string_suffix(response, path))
        || source.contains(&path.to_ascii_lowercase())
}

fn architecture_connection_observed(
    execution: &ObservableExecution,
    from_path: &str,
    to_path: &str,
    relation: &str,
) -> bool {
    tool_responses(execution, "architecture.overview").any(|response| {
        architecture_response_contains_connection(response, from_path, to_path, relation)
    })
}

fn architecture_response_contains_connection(
    value: &Value,
    from_path: &str,
    to_path: &str,
    relation: &str,
) -> bool {
    let mut from_ids = BTreeSet::new();
    let mut to_ids = BTreeSet::new();
    collect_architecture_component_ids(value, from_path, &mut from_ids);
    collect_architecture_component_ids(value, to_path, &mut to_ids);

    from_ids.iter().any(|from_id| {
        to_ids
            .iter()
            .any(|to_id| architecture_connection_contains(value, from_id, to_id, relation))
    })
}

fn collect_architecture_component_ids(
    value: &Value,
    path: &str,
    component_ids: &mut BTreeSet<String>,
) {
    match value {
        Value::Object(fields) => {
            let is_expected_component = fields.get("kind").and_then(Value::as_str) == Some("file")
                && fields
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| name.replace('\\', "/").ends_with(path));
            if is_expected_component
                && let Some(id) = fields.get("id").and_then(Value::as_str)
                && !id.is_empty()
            {
                component_ids.insert(id.to_owned());
            }
            fields.values().for_each(|value| {
                collect_architecture_component_ids(value, path, component_ids);
            });
        }
        Value::Array(values) => values.iter().for_each(|value| {
            collect_architecture_component_ids(value, path, component_ids);
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn architecture_connection_contains(
    value: &Value,
    from_id: &str,
    to_id: &str,
    relation: &str,
) -> bool {
    match value {
        Value::Object(fields) => {
            let direct_connection = fields.get("from").and_then(Value::as_str) == Some(from_id)
                && fields.get("to").and_then(Value::as_str) == Some(to_id)
                && fields.get("kind").and_then(Value::as_str) == Some(relation)
                && fields
                    .get("weight")
                    .and_then(Value::as_u64)
                    .is_some_and(|weight| weight > 0)
                && fields
                    .get("confidence")
                    .and_then(Value::as_u64)
                    .is_some_and(|confidence| confidence > 0);
            direct_connection
                || fields
                    .values()
                    .any(|value| architecture_connection_contains(value, from_id, to_id, relation))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| architecture_connection_contains(value, from_id, to_id, relation)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn cycle_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    first: &str,
    second: &str,
    source: &str,
) -> bool {
    let (Some(first_id), Some(second_id)) = (entity_id(key, first), entity_id(key, second)) else {
        return false;
    };
    tool_responses(execution, "architecture.cycles")
        .any(|response| same_array_item_contains(response, first_id, second_id))
        || source_calls(source, first, second) && source_calls(source, second, first)
}

fn same_array_item_contains(value: &Value, first: &str, second: &str) -> bool {
    match value {
        Value::Array(values) => values.iter().any(|value| {
            value_contains_exact_string(value, first) && value_contains_exact_string(value, second)
        }),
        Value::Object(fields) => fields
            .values()
            .any(|value| same_array_item_contains(value, first, second)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn flow_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    from: &str,
    to: &str,
    source: &str,
) -> bool {
    let (Some(from_id), Some(to_id)) = (entity_id(key, from), entity_id(key, to)) else {
        return false;
    };
    tool_responses(execution, "flow.trace")
        .any(|response| same_array_item_contains(response, from_id, to_id))
        || source_calls(source, from, to)
}

fn plan_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "plan.change")
        .any(|response| value_contains_exact_string(response, symbol_id))
        || tool_responses(execution, "query.batch")
            .any(|response| value_contains_exact_string(response, symbol_id))
        || source_declares(source, name)
}

fn dead_candidate_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "code.dead")
        .any(|response| value_contains_exact_string(response, symbol_id))
        || source.matches(&format!("fn {name}(")).count() == 1
}

fn generation_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    state: &str,
) -> bool {
    let Some(generation_id) = key.generation_ids.get(state) else {
        return false;
    };
    tool_responses(execution, "history.compare")
        .any(|response| value_contains_exact_string(response, generation_id))
}

fn history_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(symbol_id) = entity_id(key, name) else {
        return false;
    };
    tool_responses(execution, "history.compare")
        .any(|response| value_contains_exact_string(response, symbol_id))
        || source_declares(source, name)
}

fn batch_operation_observed(execution: &ObservableExecution, operation: &str) -> bool {
    tool_responses(execution, "query.batch")
        .any(|response| object_with_id_and_success(response, operation))
}

fn object_with_id_and_success(value: &Value, operation: &str) -> bool {
    match value {
        Value::Object(fields) => {
            let matching_id = fields.get("id").and_then(Value::as_str) == Some(operation);
            let succeeded = fields
                .get("status")
                .and_then(Value::as_str)
                .is_some_and(|status| matches!(status, "ok" | "succeeded"));
            matching_id && succeeded
                || fields
                    .values()
                    .any(|value| object_with_id_and_success(value, operation))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| object_with_id_and_success(value, operation)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn repository_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    name: &str,
    source: &str,
) -> bool {
    let Some(repository_id) = key.repository_ids.get(name) else {
        return false;
    };
    tool_responses(execution, "repo.list")
        .any(|response| value_contains_exact_string(response, repository_id))
        || source.contains(name)
}

fn cross_repository_observed(
    execution: &ObservableExecution,
    key: &WorkflowAnswerKey,
    from: &str,
    to: &str,
    source: &str,
) -> bool {
    let (Some(from_id), Some(to_id)) = (entity_id(key, from), entity_id(key, to)) else {
        return false;
    };
    tool_responses(execution, "flow.trace")
        .any(|response| same_array_item_contains(response, from_id, to_id))
        || source.contains("rootlight_budget_runtime_fixture") && source_calls(source, from, to)
}

#[derive(Clone)]
struct ObservableCall {
    tool: String,
    response: Value,
    source_frame: Vec<u8>,
    truncated: bool,
    continuation_available: bool,
}

#[derive(Clone)]
struct ObservableExecution {
    attempt_index: u16,
    task: ConsumerTask,
    calls: Vec<ObservableCall>,
    answer: ConsumerAnswer,
}

fn classify_task_prompt(prompt: &str) -> Option<TrajectoryWorkflowFamily> {
    let family = if prompt.starts_with("locate the implementation of the concept ") {
        TrajectoryWorkflowFamily::LocateImplementation
    } else if prompt.starts_with("explain the unfamiliar symbol ") {
        TrajectoryWorkflowFamily::ExplainSymbol
    } else if prompt.starts_with("find the exact callers and callees ") {
        TrajectoryWorkflowFamily::CallRelationships
    } else if prompt.starts_with("prepare the minimal context needed to fix ") {
        TrajectoryWorkflowFamily::BugFixContext
    } else if prompt.starts_with("assess the impact of changing ") {
        TrajectoryWorkflowFamily::AssessChangeImpact
    } else if prompt.starts_with("select the exact tests required after editing ") {
        TrajectoryWorkflowFamily::SelectTests
    } else if prompt.starts_with("build a repository architecture overview ") {
        TrajectoryWorkflowFamily::ArchitectureOverview
    } else if prompt.starts_with("find the cycle between ") {
        TrajectoryWorkflowFamily::CycleInvestigation
    } else if prompt.starts_with("identify budget_unused as a dead-code candidate ") {
        TrajectoryWorkflowFamily::DeadCodeInvestigation
    } else if prompt.starts_with("trace submit_budget_request through ") {
        TrajectoryWorkflowFamily::CrossServiceTrace
    } else if prompt.starts_with("prepare a refactoring boundary around ") {
        TrajectoryWorkflowFamily::RefactoringBoundary
    } else if prompt.starts_with("compare the two indexed states ") {
        TrajectoryWorkflowFamily::HistoryComparison
    } else if prompt.starts_with("create an API migration plan for ") {
        TrajectoryWorkflowFamily::ApiMigrationBatch
    } else if prompt.starts_with("coordinate the migrate_budget_api migration across ") {
        TrajectoryWorkflowFamily::MultiRepositoryMigration
    } else {
        return None;
    };
    Some(family)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConsumerTask {
    workflow_id: String,
    task_id: String,
    family: TrajectoryWorkflowFamily,
    task_sha256: String,
    fixture_sha256: String,
    seed: u64,
    expected_evidence: BTreeSet<String>,
    rootlight_tools: Vec<String>,
    prompt: String,
}

impl ConsumerTask {
    fn from_execution(input: TrajectoryExecutionInput<'_>) -> Self {
        assert_canonical_sha256(input.task_sha256, "task digest");
        assert_canonical_sha256(input.fixture_sha256, "fixture digest");
        assert!(
            !input.workflow.task_id.is_empty(),
            "preregistered task identity is present"
        );
        let expected_evidence = input
            .workflow
            .expected_evidence
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let rootlight_tools = input.workflow.rootlight_tools.to_vec();
        assert!(
            !expected_evidence.is_empty() && !rootlight_tools.is_empty(),
            "preregistered task retains evidence and tool contracts"
        );
        let prompt = trajectory_task_prompt(input.workflow.family, input.seed);
        Self {
            workflow_id: input.workflow.workflow_id.clone(),
            task_id: input.workflow.task_id.clone(),
            family: input.workflow.family,
            task_sha256: input.task_sha256.to_owned(),
            fixture_sha256: input.fixture_sha256.to_owned(),
            seed: input.seed,
            expected_evidence,
            rootlight_tools,
            prompt,
        }
    }

    fn assert_rootlight_plan(&self, calls: &[(&'static str, Value)]) {
        let planned = calls
            .iter()
            .map(|(tool, _)| (*tool).to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            planned, self.rootlight_tools,
            "consumer plan must execute the exact preregistered task sequence"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ConsumerAnswer {
    task_id: String,
    task_sha256: String,
    fixture_sha256: String,
    seed: u64,
    observed_tools: Vec<String>,
    observed_symbol_ids: BTreeSet<String>,
    semantic_text: String,
    source_text: String,
    structured_keys: BTreeSet<String>,
    source_reference_observed: bool,
    all_calls_succeeded: bool,
    partial_result_disclosed: bool,
}

fn assert_canonical_sha256(value: &str, field: &str) {
    assert!(
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{field} is canonical SHA-256"
    );
}

fn synthesize_answer(task: &ConsumerTask, calls: &[ObservableCall]) -> ConsumerAnswer {
    let mut observed_symbol_ids = BTreeSet::new();
    let mut semantic_text = String::new();
    let mut source_text = String::new();
    let mut structured_keys = BTreeSet::new();
    for call in calls {
        collect_symbol_ids(&call.response, &mut observed_symbol_ids);
        collect_semantic_text(&call.response, None, &mut semantic_text);
        collect_structured_keys(&call.response, &mut structured_keys);
        if !call.source_frame.is_empty() {
            source_text.push_str(&String::from_utf8_lossy(&call.source_frame));
        }
    }
    let all_calls_succeeded = calls.iter().all(|call| {
        call.response["result"]["isError"] != true
            && !call.response.get("error").is_some_and(Value::is_object)
    });
    let partial_result_disclosed = calls
        .iter()
        .filter(|call| call.truncated || call.continuation_available)
        .all(|call| {
            has_present_field(
                &call.response,
                &["completeness", "omitted", "truncated", "continuation"],
            )
        });
    ConsumerAnswer {
        task_id: task.task_id.clone(),
        task_sha256: task.task_sha256.clone(),
        fixture_sha256: task.fixture_sha256.clone(),
        seed: task.seed,
        observed_tools: calls.iter().map(|call| call.tool.clone()).collect(),
        observed_symbol_ids,
        semantic_text,
        source_text,
        structured_keys,
        source_reference_observed: calls.iter().any(|call| {
            has_present_field(
                &call.response,
                &[
                    "source_ref",
                    "source_refs",
                    "source_references",
                    "references",
                    "evidence_refs",
                    "symbol_id",
                    "coverage",
                    "provenance",
                    "definition",
                    "matched_states",
                ],
            )
        }),
        all_calls_succeeded,
        partial_result_disclosed,
    }
}

fn collect_semantic_text(value: &Value, key: Option<&str>, output: &mut String) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                collect_semantic_text(value, Some(name), output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_semantic_text(value, key, output);
            }
        }
        Value::String(text) if !matches!(key, Some("repository_id" | "generation_id")) => {
            output.push_str(text);
            output.push('\n');
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn collect_structured_keys(value: &Value, output: &mut BTreeSet<String>) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                output.insert(name.clone());
                collect_structured_keys(value, output);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_structured_keys(value, output);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn collect_symbol_ids(value: &Value, output: &mut BTreeSet<String>) {
    match value {
        Value::String(text) if text.starts_with("sym1_") => {
            output.insert(text.clone());
        }
        Value::Array(values) => {
            for value in values {
                collect_symbol_ids(value, output);
            }
        }
        Value::Object(fields) => {
            for value in fields.values() {
                collect_symbol_ids(value, output);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

struct RootlightProcessAdapter<'a> {
    mcp: &'a mut McpProcess,
    first: IndexReceipt,
    second: IndexReceipt,
    consumer: IndexReceipt,
    entry: LocatedSymbol,
    helper: LocatedSymbol,
    unused: LocatedSymbol,
    added: LocatedSymbol,
    transform: LocatedSymbol,
    gateway: LocatedSymbol,
    worker: LocatedSymbol,
    cycle_alpha: LocatedSymbol,
    cycle_beta: LocatedSymbol,
    unit_test: LocatedSymbol,
    consumer_migration: LocatedSymbol,
    consumer_helper: LocatedSymbol,
    workflow_observations: Vec<ObservableExecution>,
}

impl TrajectoryAdapter for RootlightProcessAdapter<'_> {
    fn condition(&self) -> TrajectoryCondition {
        TrajectoryCondition::Rootlight
    }

    fn execution_boundary(&self) -> TrajectoryExecutionBoundary {
        TrajectoryExecutionBoundary::DaemonMcpProcess
    }

    fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt {
        let task = ConsumerTask::from_execution(input);
        let planned_family =
            classify_task_prompt(&task.prompt).expect("the task prompt has a supported intent");
        assert_eq!(
            planned_family, task.family,
            "task text and preregistered workflow family must agree"
        );
        let calls = self.tool_calls(&task, planned_family);
        task.assert_rootlight_plan(&calls);
        let (raw, observation) = self.execute_observed(
            &task,
            &format!("{}-{}", task.task_id, task.seed),
            input.attempt_index,
            calls,
        );
        self.workflow_observations.push(observation);
        raw
    }
}

impl RootlightProcessAdapter<'_> {
    fn execute_direct_sequence(
        &mut self,
        attempt_index: u16,
        task: &ConsumerTask,
    ) -> (RawTrajectoryAttempt, ObservableExecution) {
        assert_eq!(
            task.family,
            TrajectoryWorkflowFamily::BugFixContext,
            "direct retrieval only pairs with the context task"
        );
        let calls = self.direct_tool_calls(task);
        let execution_id = format!("{}-direct-{}", task.task_id, task.seed);
        self.execute_observed(task, &execution_id, attempt_index, calls)
    }

    fn execute_observed(
        &mut self,
        task: &ConsumerTask,
        execution_id: &str,
        attempt_index: u16,
        calls: Vec<(&'static str, Value)>,
    ) -> (RawTrajectoryAttempt, ObservableExecution) {
        let mut records = Vec::with_capacity(calls.len());
        let mut observable_calls = Vec::with_capacity(calls.len());
        let mut outcome = TrajectoryAttemptOutcome::Succeeded;
        for (call_index, (tool, arguments)) in calls.into_iter().enumerate() {
            let id = format!("{execution_id}-{attempt_index}-{call_index}");
            let request = tool_call(&id, tool, arguments.clone());
            let request_frame = serde_json::to_vec(&request)
                .unwrap_or_else(|_| b"{\"error\":\"serialization_failed\"}".to_vec());
            let started = Instant::now();
            let response = self.mcp.call_result(&id, tool, arguments);
            let elapsed_ns = elapsed_nanos(started);
            match response {
                Ok(response) => {
                    let response_frame = serde_json::to_vec(&response)
                        .unwrap_or_else(|_| b"{\"error\":\"serialization_failed\"}".to_vec());
                    let source_frame = extract_source_frame(&response);
                    let result_items = result_items(&response);
                    let truncated = contains_true(&response, "truncated");
                    let continuation_available = contains_present(
                        &response,
                        &["continuation", "continuation_token", "next_cursor"],
                    );
                    observable_calls.push(ObservableCall {
                        tool: tool.to_owned(),
                        response: response.clone(),
                        source_frame: source_frame.clone(),
                        truncated,
                        continuation_available,
                    });
                    let public_error = response["result"]["isError"] == true
                        || response.get("error").is_some_and(Value::is_object);
                    let error_code = normalized_error_code(&response);
                    let operation_status = if public_error {
                        TrajectoryOperationStatus::Failed {
                            error_code: error_code.clone(),
                        }
                    } else {
                        TrajectoryOperationStatus::Succeeded
                    };
                    if public_error {
                        outcome = if error_code.contains("unsupported") {
                            TrajectoryAttemptOutcome::Unsupported {
                                error_code: error_code.clone(),
                            }
                        } else {
                            TrajectoryAttemptOutcome::Failed {
                                error_code: error_code.clone(),
                            }
                        };
                    }
                    records.push(RawTrajectoryCall {
                        operation_id: format!("operation-{call_index:02}"),
                        tool: TrajectoryToolIdentity {
                            tool_id: tool.to_owned(),
                            tool_version: format!("rootlight-mcp-{}", env!("CARGO_PKG_VERSION")),
                        },
                        exposure_profile: TrajectoryExposureProfile::Developer,
                        operation_status,
                        retry_ordinal: 0,
                        request_frame,
                        response_frame,
                        source_frame,
                        elapsed_ns,
                        result_items,
                        truncated,
                        continuation_available,
                        claim_signals: TrajectoryClaimSignals::default(),
                    });
                    if public_error {
                        break;
                    }
                }
                Err(()) => {
                    let error_code = "response_timeout".to_owned();
                    observable_calls.push(ObservableCall {
                        tool: tool.to_owned(),
                        response: json!({"error": {"code": error_code.clone()}}),
                        source_frame: Vec::new(),
                        truncated: false,
                        continuation_available: false,
                    });
                    records.push(RawTrajectoryCall {
                        operation_id: format!("operation-{call_index:02}"),
                        tool: TrajectoryToolIdentity {
                            tool_id: tool.to_owned(),
                            tool_version: format!("rootlight-mcp-{}", env!("CARGO_PKG_VERSION")),
                        },
                        exposure_profile: TrajectoryExposureProfile::Developer,
                        operation_status: TrajectoryOperationStatus::TimedOut {
                            error_code: error_code.clone(),
                        },
                        retry_ordinal: 0,
                        request_frame,
                        response_frame: b"{\"error\":\"response_timeout\"}".to_vec(),
                        source_frame: Vec::new(),
                        elapsed_ns,
                        result_items: 0,
                        truncated: false,
                        continuation_available: false,
                        claim_signals: TrajectoryClaimSignals::default(),
                    });
                    outcome = TrajectoryAttemptOutcome::TimedOut { error_code };
                    break;
                }
            }
        }
        let answer = synthesize_answer(task, &observable_calls);
        (
            RawTrajectoryAttempt {
                outcome,
                calls: records,
            },
            ObservableExecution {
                attempt_index,
                task: task.clone(),
                calls: observable_calls,
                answer,
            },
        )
    }

    fn direct_tool_calls(&self, task: &ConsumerTask) -> Vec<(&'static str, Value)> {
        let repository = || json!({"repository_id": self.second.repository_id});
        let generation = || Value::String(self.second.generation_id.clone());
        let [primary, secondary] = self.seeded_targets(task.seed);
        vec![
            (
                "code.locate",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "query": primary.query,
                    "search_modes": ["exact"],
                    "max_results": 20
                }),
            ),
            (
                "symbol.explain",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "symbol_ids": [primary.symbol.symbol_id, secondary.symbol.symbol_id]
                }),
            ),
            (
                "source.read",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "references": [
                        {"source_ref": primary.symbol.source_ref},
                        {"source_ref": secondary.symbol.source_ref}
                    ],
                    "include_line_numbers": true,
                    "encoding": "utf8_lossless_when_valid"
                }),
            ),
            (
                "symbol.relationships",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "symbol_ids": [primary.symbol.symbol_id],
                    "relations": ["calls", "references"],
                    "direction": "both",
                    "max_results": 20
                }),
            ),
        ]
    }

    fn tool_calls(
        &self,
        task: &ConsumerTask,
        planned_family: TrajectoryWorkflowFamily,
    ) -> Vec<(&'static str, Value)> {
        let repository = || json!({"repository_id": self.second.repository_id});
        let generation = || Value::String(self.second.generation_id.clone());
        let [primary, secondary] = self.seeded_targets(task.seed);
        match planned_family {
            TrajectoryWorkflowFamily::LocateImplementation => vec![
                (
                    "code.locate",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "query": primary.query,
                        "search_modes": ["exact"],
                        "max_results": 20
                    }),
                ),
                (
                    "symbol.explain",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "symbol_ids": [primary.symbol.symbol_id]
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::ExplainSymbol => vec![(
                "symbol.explain",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "symbol_ids": [primary.symbol.symbol_id]
                }),
            )],
            TrajectoryWorkflowFamily::CallRelationships => vec![(
                "symbol.relationships",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "symbol_ids": [primary.symbol.symbol_id],
                    "relations": ["calls"],
                    "direction": "both",
                    "max_results": 20
                }),
            )],
            TrajectoryWorkflowFamily::BugFixContext => vec![
                (
                    "code.locate",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "query": primary.query,
                        "search_modes": ["exact"],
                        "max_results": 20
                    }),
                ),
                (
                    "context.pack",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "task": task.prompt,
                        "seeds": {
                            "symbols": [
                                primary.symbol.symbol_id
                            ]
                        },
                        "token_budget": 8_000,
                        "source_policy": "focused_snippets",
                        "sections": ["definitions", "callers", "callees", "tests", "source"],
                        "response_profile": "evidence"
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::AssessChangeImpact => vec![
                (
                    "change.impact",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "change": {
                            "symbol_ids": [primary.symbol.symbol_id, secondary.symbol.symbol_id]
                        },
                        "max_depth": 3,
                        "include_tests": true
                    }),
                ),
                (
                    "tests.select",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "seeds": {
                            "symbols": [primary.symbol.symbol_id, secondary.symbol.symbol_id]
                        }
                    }),
                ),
                (
                    "plan.change",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "objective": "bug_fix",
                        "objective_text": task.prompt,
                        "targets": [
                            {"symbol_id": primary.symbol.symbol_id},
                            {"symbol_id": secondary.symbol.symbol_id}
                        ]
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::SelectTests => vec![(
                "tests.select",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "seeds": {
                        "symbols": [primary.symbol.symbol_id, secondary.symbol.symbol_id]
                    },
                    "profile": "evidence"
                }),
            )],
            TrajectoryWorkflowFamily::ArchitectureOverview => vec![(
                "architecture.overview",
                json!({"repository": repository(), "generation": generation()}),
            )],
            TrajectoryWorkflowFamily::CycleInvestigation => vec![
                (
                    "architecture.cycles",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "projection": {"relations": ["calls"], "level": "symbol"},
                        "max_cycles": 20
                    }),
                ),
                (
                    "flow.trace",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "from": {"symbol_id": self.cycle_alpha.symbol_id},
                        "to": {"symbol_id": self.cycle_beta.symbol_id},
                        "relations": ["calls"],
                        "direction": "outbound",
                        "max_depth": 3,
                        "max_paths": 20
                    }),
                ),
                (
                    "plan.change",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "objective": "refactor",
                        "objective_text": task.prompt,
                        "targets": [
                            {"symbol_id": self.cycle_alpha.symbol_id},
                            {"symbol_id": self.cycle_beta.symbol_id}
                        ]
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::DeadCodeInvestigation => vec![
                (
                    "code.dead",
                    json!({"repository": repository(), "generation": generation()}),
                ),
                (
                    "symbol.explain",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "symbol_ids": [self.unused.symbol_id]
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::CrossServiceTrace => vec![
                (
                    "code.locate",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "query": "submit_budget_request",
                        "search_modes": ["exact"],
                        "max_results": 20
                    }),
                ),
                (
                    "flow.trace",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "from": {"symbol_id": self.gateway.symbol_id},
                        "to": {"symbol_id": self.transform.symbol_id},
                        "relations": ["calls"],
                        "direction": "outbound",
                        "max_depth": 5,
                        "max_paths": 20
                    }),
                ),
                (
                    "context.pack",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "task": task.prompt,
                        "seeds": {
                            "symbols": [
                                self.gateway.symbol_id
                            ]
                        },
                        "token_budget": 4_500,
                        "source_policy": "focused_snippets",
                        "response_profile": "compact"
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::RefactoringBoundary => vec![
                (
                    "symbol.relationships",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "symbol_ids": [primary.symbol.symbol_id],
                        "relations": ["calls"],
                        "direction": "both",
                        "max_results": 20
                    }),
                ),
                (
                    "change.impact",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "change": {"symbol_ids": [primary.symbol.symbol_id]},
                        "max_depth": 3,
                        "include_tests": true
                    }),
                ),
                (
                    "context.pack",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "task": task.prompt,
                        "seeds": {
                            "symbols": [
                                primary.symbol.symbol_id
                            ]
                        },
                        "token_budget": 4_500,
                        "source_policy": "focused_snippets",
                        "response_profile": "compact"
                    }),
                ),
                (
                    "plan.change",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "objective": "refactor",
                        "objective_text": task.prompt,
                        "targets": [
                            {"symbol_id": primary.symbol.symbol_id},
                            {"symbol_id": secondary.symbol.symbol_id}
                        ]
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::HistoryComparison => vec![
                (
                    "history.compare",
                    json!({
                        "repository": repository(),
                        "base": self.first.generation_id,
                        "head": self.second.generation_id,
                        "max_results": 20
                    }),
                ),
                (
                    "change.impact",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "change": {"symbol_ids": [self.added.symbol_id]},
                        "max_depth": 3,
                        "include_tests": true
                    }),
                ),
            ],
            TrajectoryWorkflowFamily::ApiMigrationBatch => vec![(
                "query.batch",
                json!({
                    "repository": repository(),
                    "generation": generation(),
                    "budget": {"max_tokens": 16_000},
                    "operations": [
                        {
                            "id": "locate",
                            "tool": "code.locate",
                            "arguments": {
                                "query": primary.query,
                                "search_modes": ["exact"],
                                "max_results": 20
                            }
                        },
                        {
                            "id": "impact",
                            "tool": "change.impact",
                            "depends_on": ["locate"],
                            "arguments": {
                                "change": {
                                    "symbol_ids": {
                                        "$from": "locate",
                                        "source": "symbol_id",
                                        "index": 0
                                    }
                                },
                                "max_depth": 3,
                                "include_tests": true
                            }
                        },
                        {
                            "id": "plan",
                            "tool": "plan.change",
                            "depends_on": ["locate", "impact"],
                            "arguments": {
                                "objective": "migration",
                                "objective_text": task.prompt,
                                "targets": [{
                                    "symbol_id": {
                                        "$from": "locate",
                                        "source": "symbol_id",
                                        "index": 0
                                    }
                                }]
                            }
                        }
                    ],
                    "failure_policy": "fail_fast"
                }),
            )],
            TrajectoryWorkflowFamily::MultiRepositoryMigration => vec![
                ("repo.list", json!({"max_results": 20})),
                (
                    "code.locate",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "query": "budget_entry",
                        "search_modes": ["exact"],
                        "max_results": 20
                    }),
                ),
                (
                    "code.locate",
                    json!({
                        "repository": {"repository_id": self.consumer.repository_id},
                        "generation": self.consumer.generation_id,
                        "query": "migrate_budget_api",
                        "search_modes": ["exact"],
                        "max_results": 20
                    }),
                ),
                (
                    "flow.trace",
                    json!({
                        "repository": {"repository_id": self.consumer.repository_id},
                        "generation": self.consumer.generation_id,
                        "from": {"symbol_id": self.consumer_migration.symbol_id},
                        "to": {"symbol_id": self.entry.symbol_id},
                        "relations": ["calls", "imports"],
                        "direction": "outbound",
                        "cross_repository": true,
                        "max_depth": 5,
                        "max_paths": 20
                    }),
                ),
                (
                    "change.impact",
                    json!({
                        "repository": {"repository_id": self.consumer.repository_id},
                        "generation": self.consumer.generation_id,
                        "change": {"symbol_ids": [self.consumer_migration.symbol_id]},
                        "max_depth": 3,
                        "include_tests": true
                    }),
                ),
                (
                    "context.pack",
                    json!({
                        "repository": repository(),
                        "generation": generation(),
                        "task": task.prompt,
                        "seeds": {"symbols": [self.entry.symbol_id]},
                        "token_budget": 4_500,
                        "source_policy": "focused_snippets",
                        "response_profile": "compact"
                    }),
                ),
                (
                    "context.pack",
                    json!({
                        "repository": {"repository_id": self.consumer.repository_id},
                        "generation": self.consumer.generation_id,
                        "task": task.prompt,
                        "seeds": {
                            "symbols": [
                                self.consumer_migration.symbol_id
                            ]
                        },
                        "token_budget": 4_500,
                        "source_policy": "focused_snippets",
                        "response_profile": "compact"
                    }),
                ),
            ],
        }
    }

    fn seeded_targets(&self, seed: u64) -> [ConsumerTarget<'_>; 2] {
        let entry = ConsumerTarget {
            query: "budget_entry",
            symbol: &self.entry,
        };
        let helper = ConsumerTarget {
            query: "budget_helper",
            symbol: &self.helper,
        };
        if seed % 5 >= 3 {
            [helper, entry]
        } else {
            [entry, helper]
        }
    }
}

#[derive(Clone, Copy)]
struct ConsumerTarget<'a> {
    query: &'static str,
    symbol: &'a LocatedSymbol,
}

fn measured_metrics(
    attempt: &RawTrajectoryAttempt,
    tokenizer: &dyn TrajectoryTokenizer,
) -> BlindedCandidateMetrics {
    let mut metrics = BlindedCandidateMetrics {
        calls: u64::try_from(attempt.calls.len()).expect("call count fits u64"),
        ..BlindedCandidateMetrics::default()
    };
    for call in &attempt.calls {
        let request_tokens = tokenizer
            .count(&call.request_frame)
            .expect("direct request has actual token accounting");
        let response_tokens = tokenizer
            .count(&call.response_frame)
            .expect("direct response has actual token accounting");
        metrics.tokens = metrics
            .tokens
            .checked_add(request_tokens)
            .and_then(|total| total.checked_add(response_tokens))
            .expect("direct token total fits u64");
        metrics.source_tokens = metrics
            .source_tokens
            .checked_add(
                tokenizer
                    .count(&call.source_frame)
                    .expect("direct source has actual token accounting"),
            )
            .expect("direct source-token total fits u64");
        metrics.elapsed_ns = metrics
            .elapsed_ns
            .checked_add(call.elapsed_ns)
            .expect("direct elapsed time fits u64");
    }
    metrics
}

fn assert_attempt_within_bounds(
    attempt: &RawTrajectoryAttempt,
    bounds: TrajectorySharedBounds,
    tokenizer: &dyn TrajectoryTokenizer,
) {
    let metrics = measured_metrics(attempt, tokenizer);
    let result_items = attempt
        .calls
        .iter()
        .try_fold(0_u64, |total, call| total.checked_add(call.result_items))
        .expect("direct result-item total fits u64");
    let source_bytes = attempt
        .calls
        .iter()
        .try_fold(0_u64, |total, call| {
            total.checked_add(
                u64::try_from(call.source_frame.len()).expect("source length fits u64"),
            )
        })
        .expect("direct source-byte total fits u64");
    assert!(matches!(
        attempt.outcome,
        TrajectoryAttemptOutcome::Succeeded
    ));
    assert!(metrics.calls <= u64::from(bounds.tool_calls));
    assert!(metrics.tokens <= bounds.total_tokens);
    assert!(metrics.elapsed_ns <= bounds.elapsed_ns);
    assert!(result_items <= bounds.result_items);
    assert!(source_bytes <= bounds.source_bytes);
    assert!(attempt.calls.iter().all(|call| {
        !call.truncated
            || call.continuation_available
            || contains_present(
                &serde_json::from_slice::<Value>(&call.response_frame)
                    .expect("direct response frame remains structured JSON"),
                &["completeness", "omitted", "truncated"],
            )
    }));
}

fn primary_rubric_evidence(
    candidates: &[BlindedAblationCandidate],
    pairing_map: &RestrictedPairingMap,
    context: &[ObservableExecution],
    direct: &[ObservableExecution],
    entry: &LocatedSymbol,
    helper: &LocatedSymbol,
) -> Vec<CandidateRubricEvidence> {
    let answer_key = HeldOutAnswerKey {
        required_symbol_ids: [entry.symbol_id.clone(), helper.symbol_id.clone()],
        required_semantic_terms: ["budget_entry", "budget_helper"],
    };
    let mut evidence = Vec::with_capacity(context.len() + direct.len());
    for (variant, observations) in [
        (AblationVariant::ContextPack, context),
        (AblationVariant::DirectSequence, direct),
    ] {
        for observation in observations {
            let pair = pairing_map
                .pairs
                .get(usize::from(observation.attempt_index))
                .expect("observable execution has a preregistered pair");
            let mapping = pairing_map
                .entries
                .iter()
                .find(|mapping| mapping.pair_id == pair.pair_id && mapping.variant == variant)
                .expect("observable execution has a restricted mapping");
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.blind_id == mapping.blind_id)
                .expect("observable execution has a blinded candidate");
            evidence.push(observable_rubric_evidence(
                candidate,
                observation,
                &answer_key,
            ));
        }
    }
    evidence
}

struct HeldOutAnswerKey {
    required_symbol_ids: [String; 2],
    required_semantic_terms: [&'static str; 2],
}

fn observable_rubric_evidence(
    candidate: &BlindedAblationCandidate,
    execution: &ObservableExecution,
    answer_key: &HeldOutAnswerKey,
) -> CandidateRubricEvidence {
    let answer = &execution.answer;
    let task_bound = answer.task_id == execution.task.task_id
        && answer.task_sha256 == execution.task.task_sha256
        && answer.fixture_sha256 == execution.task.fixture_sha256
        && answer.seed == execution.task.seed;
    let contains_entry = answer
        .observed_symbol_ids
        .contains(&answer_key.required_symbol_ids[0]);
    let contains_helper = answer
        .observed_symbol_ids
        .contains(&answer_key.required_symbol_ids[1]);
    let semantic_terms = answer_key
        .required_semantic_terms
        .map(|term| answer_contains_semantic_term(answer, term));
    let has_source_material = !answer.source_text.is_empty();
    let task_adherent = !answer.observed_tools.is_empty()
        && answer
            .observed_tools
            .iter()
            .all(|tool| is_consumer_retrieval_tool(tool));
    let observations = [
        (
            RubricDimension::Correctness,
            vec![
                answer.all_calls_succeeded,
                task_bound,
                contains_entry,
                contains_helper,
                semantic_terms[0],
                semantic_terms[1],
            ],
        ),
        (
            RubricDimension::Completeness,
            vec![
                answer.all_calls_succeeded,
                contains_entry,
                contains_helper,
                has_source_material,
                semantic_terms[0],
                semantic_terms[1],
            ],
        ),
        (
            RubricDimension::EvidenceSupport,
            vec![answer.source_reference_observed, has_source_material],
        ),
        (
            RubricDimension::UncertaintyHandling,
            vec![answer.partial_result_disclosed],
        ),
        (
            RubricDimension::Actionability,
            vec![
                answer.source_reference_observed,
                has_source_material,
                contains_entry,
                contains_helper,
                semantic_terms[0],
                semantic_terms[1],
            ],
        ),
        (
            RubricDimension::SourceRelevance,
            vec![
                contains_entry,
                contains_helper,
                semantic_terms[0],
                semantic_terms[1],
            ],
        ),
        (
            RubricDimension::TaskAdherence,
            vec![answer.all_calls_succeeded, task_bound, task_adherent],
        ),
    ]
    .into_iter()
    .map(|(dimension, checks)| (dimension, RubricObservation::Checks { checks }))
    .collect();
    CandidateRubricEvidence {
        blind_id: candidate.blind_id.clone(),
        candidate_sha256: candidate.candidate_sha256.clone(),
        observations,
        unsupported_claims: UnsupportedClaimAssessment::Assessed {
            categories: unsupported_claim_categories(execution),
        },
    }
}

fn answer_contains_semantic_term(answer: &ConsumerAnswer, term: &str) -> bool {
    answer.semantic_text.contains(term) || answer.source_text.contains(term)
}

fn is_consumer_retrieval_tool(tool: &str) -> bool {
    matches!(
        tool,
        "code.locate" | "context.pack" | "source.read" | "symbol.explain" | "symbol.relationships"
    )
}

fn unsupported_claim_categories(
    execution: &ObservableExecution,
) -> BTreeMap<UnsupportedClaimCategory, u32> {
    let mut categories = BTreeMap::new();
    for call in &execution.calls {
        if (call.truncated || call.continuation_available)
            && !has_present_field(
                &call.response,
                &["completeness", "omitted", "truncated", "continuation"],
            )
        {
            categories
                .entry(UnsupportedClaimCategory::PartialOrTruncatedResult)
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }
        if has_nonempty_text_field(
            &call.response["result"]["structuredContent"],
            &["snippet", "content", "source", "source_text"],
        ) && !has_present_field(
            &call.response["result"]["structuredContent"],
            &["source_ref", "source_refs", "references"],
        ) {
            categories
                .entry(UnsupportedClaimCategory::FabricatedSourceSupport)
                .and_modify(|count| *count += 1)
                .or_insert(1);
        }
    }
    categories
}

fn has_present_field(value: &Value, keys: &[&str]) -> bool {
    match value {
        Value::Object(fields) => {
            fields
                .iter()
                .any(|(key, value)| keys.contains(&key.as_str()) && !value.is_null())
                || fields.values().any(|value| has_present_field(value, keys))
        }
        Value::Array(values) => values.iter().any(|value| has_present_field(value, keys)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn has_nonempty_text_field(value: &Value, keys: &[&str]) -> bool {
    match value {
        Value::Object(fields) => {
            fields.iter().any(|(key, value)| {
                keys.contains(&key.as_str()) && value.as_str().is_some_and(|text| !text.is_empty())
            }) || fields
                .values()
                .any(|value| has_nonempty_text_field(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .any(|value| has_nonempty_text_field(value, keys)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn extract_source_frame(value: &Value) -> Vec<u8> {
    fn visit(value: &Value, key: Option<&str>, output: &mut String) {
        match value {
            Value::Object(fields) => {
                for (name, value) in fields {
                    visit(value, Some(name), output);
                }
            }
            Value::Array(values) => {
                for value in values {
                    visit(value, key, output);
                }
            }
            Value::String(text)
                if matches!(key, Some("content" | "snippet" | "source" | "source_text")) =>
            {
                output.push_str(text);
                output.push('\n');
            }
            _ => {}
        }
    }
    let mut output = String::new();
    visit(value, None, &mut output);
    output.into_bytes()
}

fn result_items(value: &Value) -> u64 {
    fn largest_array(value: &Value) -> usize {
        match value {
            Value::Array(values) => values
                .iter()
                .map(largest_array)
                .max()
                .unwrap_or(0)
                .max(values.len()),
            Value::Object(fields) => fields.values().map(largest_array).max().unwrap_or(0),
            _ => 0,
        }
    }
    u64::try_from(largest_array(value))
        .unwrap_or(u64::MAX)
        .max(1)
}

fn contains_true(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(fields) => {
            fields.get(key) == Some(&Value::Bool(true))
                || fields.values().any(|value| contains_true(value, key))
        }
        Value::Array(values) => values.iter().any(|value| contains_true(value, key)),
        _ => false,
    }
}

fn contains_present(value: &Value, keys: &[&str]) -> bool {
    match value {
        Value::Object(fields) => {
            fields
                .iter()
                .any(|(key, value)| keys.contains(&key.as_str()) && !value.is_null())
                || fields.values().any(|value| contains_present(value, keys))
        }
        Value::Array(values) => values.iter().any(|value| contains_present(value, keys)),
        _ => false,
    }
}

fn normalized_error_code(response: &Value) -> String {
    let code = response["result"]["structuredContent"]["error"]["code"]
        .as_str()
        .or_else(|| response["error"]["message"].as_str())
        .unwrap_or("mcp_call_failed");
    let mut normalized = code
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    normalized.truncate(128);
    if normalized.is_empty() {
        "mcp_call_failed".to_owned()
    } else {
        normalized
    }
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/mcp/budget-runtime/repository")
}

fn augment_runtime_service_fixture(root: &Path) {
    fs::OpenOptions::new()
        .append(true)
        .open(root.join("src").join("lib.rs"))
        .and_then(|mut file| {
            file.write_all(b"\npub mod cycle;\npub mod gateway;\npub mod worker;\n")
        })
        .expect("runtime service module declarations are written");
    fs::write(
        root.join("src").join("gateway.rs"),
        "//! HTTP-style entry point for the cross-service trajectory fixture.\n\n\
         pub fn submit_budget_request(value: usize) -> usize {\n\
             crate::worker::handle_budget_message(value)\n\
         }\n",
    )
    .expect("gateway fixture is written");
    fs::write(
        root.join("src").join("worker.rs"),
        "//! Message-handler boundary for the cross-service trajectory fixture.\n\n\
         pub fn handle_budget_message(value: usize) -> usize {\n\
             crate::service::transform(value)\n\
         }\n",
    )
    .expect("worker fixture is written");
    fs::write(
        root.join("src").join("cycle.rs"),
        "//! Deliberate two-node call cycle used to verify cycle evidence.\n\n\
         pub fn cycle_alpha(value: usize) -> usize {\n\
             if value == 0 { 0 } else { cycle_beta(value - 1) }\n\
         }\n\n\
         pub fn cycle_beta(value: usize) -> usize {\n\
             if value == 0 { 0 } else { cycle_alpha(value - 1) }\n\
         }\n",
    )
    .expect("cycle fixture is written");
}

fn create_consumer_service_fixture(root: &Path, runtime_root: &Path) {
    fs::create_dir_all(root.join("src")).expect("consumer source directory is created");
    let dependency = runtime_root
        .strip_prefix(root.parent().expect("consumer fixture has a parent"))
        .expect("runtime and consumer fixtures share a parent")
        .to_string_lossy()
        .replace('\\', "/");
    fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"rootlight_budget_client_fixture\"\n\
             version = \"0.1.0\"\n\
             edition = \"2024\"\n\n\
             [dependencies]\n\
             rootlight_budget_runtime_fixture = {{ path = \"../{dependency}\" }}\n"
        ),
    )
    .expect("consumer fixture manifest is written");
    fs::write(
        root.join("src").join("lib.rs"),
        "//! Consumer service used to verify multi-repository migration evidence.\n\n\
         use rootlight_budget_runtime_fixture::budget_entry;\n\n\
         pub fn migrate_budget_api(value: usize) -> usize {\n\
             budget_entry(client_transform(value))\n\
         }\n\n\
         pub fn client_transform(value: usize) -> usize {\n\
             value.saturating_add(2)\n\
         }\n\n\
         #[cfg(test)]\n\
         mod tests {\n\
             use super::migrate_budget_api;\n\n\
             #[test]\n\
             fn migration_preserves_runtime_contract() {\n\
                 assert_eq!(migrate_budget_api(1), 10);\n\
             }\n\
         }\n",
    )
    .expect("consumer fixture source is written");
}

fn regular_files(root: &Path) -> Vec<PathBuf> {
    fn walk(root: &Path, directory: &Path, output: &mut Vec<PathBuf>) {
        let mut entries = fs::read_dir(directory)
            .expect("fixture directory reads")
            .collect::<Result<Vec<_>, _>>()
            .expect("fixture entries read");
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let kind = entry.file_type().expect("fixture file type reads");
            assert!(!kind.is_symlink(), "fixture cannot contain symbolic links");
            if kind.is_dir() {
                walk(root, &entry.path(), output);
            } else if kind.is_file() {
                output.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .expect("fixture path stays below root")
                        .to_path_buf(),
                );
            }
        }
    }
    let mut output = Vec::new();
    walk(root, root, &mut output);
    output
}

fn fixture_digest(root: &Path) -> String {
    let mut digest = Sha256::new();
    for relative in regular_files(root) {
        let normalized = relative.to_string_lossy().replace('\\', "/");
        let bytes = fs::read(root.join(&relative)).expect("fixture file reads");
        digest.update(normalized.as_bytes());
        digest.update([0]);
        digest.update(
            u64::try_from(bytes.len())
                .expect("fixture length fits")
                .to_le_bytes(),
        );
        digest.update(bytes);
    }
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn trajectory_fixture_digest(fixture_digests: &[&str]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"rootlight.trajectory.fixture.v2");
    for fixture_digest in fixture_digests {
        digest.update(fixture_digest.as_bytes());
        digest.update([0]);
    }
    let digest = digest.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn copy_regular_tree(source: &Path, destination: &Path) {
    fs::create_dir_all(destination).expect("fixture destination is created");
    for relative in regular_files(source) {
        let target = destination.join(&relative);
        fs::create_dir_all(target.parent().expect("fixture file has a parent"))
            .expect("fixture parent directory is created");
        fs::copy(source.join(&relative), target).expect("fixture file is copied");
    }
}

fn index_repository(mcp: &mut McpProcess, root: &Path, id: &str) -> IndexReceipt {
    let arguments = json!({"root": root, "mode": "auto", "detached": false});
    let response = process_support::retry_transient_busy(id, |attempt_id| {
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

fn locate(mcp: &mut McpProcess, index: &IndexReceipt, query: &str, id: &str) -> LocatedSymbol {
    let response = mcp.call(
        id,
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
    LocatedSymbol {
        symbol_id: required_string(&matches[0]["symbol_id"], "symbol identity"),
        source_ref: matches[0]["source_ref"].clone(),
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

fn tool_call(id: &str, tool: &str, arguments: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": {"name": tool, "arguments": arguments}
    })
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
    fn spawn(state_dir: &Path, runtime_dir: &Path) -> Self {
        let binary = daemon_binary();
        assert!(
            binary.is_file(),
            "trajectory evidence requires a prebuilt daemon at {binary:?}"
        );
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
    fn spawn(state_dir: &Path, runtime_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rootlight-mcp"))
            .current_dir(
                state_dir
                    .parent()
                    .expect("fixture state directory has a launch root"),
            )
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
                "clientInfo": {"name": "trajectory-process", "version": "1.0"},
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
        self.call_result(id, tool, arguments)
            .unwrap_or_else(|()| panic!("{tool} response timed out"))
    }

    fn call_result(&mut self, id: &str, tool: &str, arguments: Value) -> Result<Value, ()> {
        self.write(&tool_call(id, tool, arguments));
        let response = self.read_result()?;
        if response["id"] != id {
            return Err(());
        }
        Ok(response)
    }

    fn write(&mut self, message: &Value) {
        let input = self.input.as_mut().expect("MCP stdin is retained");
        serde_json::to_writer(&mut *input, message).expect("MCP request serializes");
        input.write_all(b"\n").expect("MCP request terminates");
        input.flush().expect("MCP request flushes");
    }

    fn read(&self) -> Value {
        self.read_result().expect("MCP response arrives")
    }

    fn read_result(&self) -> Result<Value, ()> {
        let line = match self.responses.recv_timeout(RESPONSE_TIMEOUT) {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return Err(()),
        };
        serde_json::from_str(&line).map_err(|_| ())
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

struct IndexReceipt {
    repository_id: String,
    generation_id: String,
}

struct LocatedSymbol {
    symbol_id: String,
    source_ref: Value,
}
