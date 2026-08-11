//! Candidate-binary cold-index and semantic query-truth release evidence.
//!
//! Both gates cross the real MCP, daemon, and isolated project-adapter processes.

mod process_support;

use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt::Write as _,
    fs,
    io::{BufRead as _, BufReader, Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use rootlight_bench::{
    COLD_INDEX_EVIDENCE_SCHEMA, ColdIndexEvidence, ColdIndexInterruptedRecoveryEvidence,
    ColdIndexLocateEvidence, ColdIndexOperationEvidence, ColdIndexProgressEvidence,
    ColdIndexResourceEvidence, ColdIndexRestartEvidence, ColdIndexTierEvidence,
    cold_index_corpus_sha256, encode_cold_index_evidence, load_cold_index_corpus,
    verify_cold_index_evidence,
};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(40);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_PROGRESS_SAMPLES: usize = 64;

const TERMINAL_SEMANTIC_LANGUAGES: [&str; 5] = ["go", "javascript", "python", "rust", "typescript"];

#[test]
#[ignore = "runs the installed-process semantic completion and query-truth release gate"]
fn installed_process_semantics_and_query_truth_are_release_ready() {
    let fixture = process_support::private_process_tempdir("rl-semantic-truth-");
    let repositories_root = fixture.path().join("repositories");
    write_semantic_truth_repositories(&repositories_root);
    let state_dir = fixture.path().join("state");
    let runtime_dir = fixture.path().join("runtime");
    let (daemon_binary, mcp_binary) = semantic_process_binaries();
    let mut daemon = DaemonProcess::spawn(&daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&mcp_binary, &state_dir, &runtime_dir);

    let mut indexes = BTreeMap::new();
    for language in TERMINAL_SEMANTIC_LANGUAGES {
        let index = index_terminal_semantics(&mut mcp, &repositories_root.join(language), language);
        indexes.insert(language, index);
    }
    for workflow in semantic_truth_workflows() {
        let index = indexes
            .get(workflow.language)
            .expect("every truth workflow has terminal language evidence");
        assert_semantic_truth_workflow(&mut mcp, index, workflow);
    }

    mcp.finish();
    daemon.finish();
}

struct SemanticIndex {
    repository_id: String,
    generation: String,
}

fn index_terminal_semantics(
    mcp: &mut McpProcess,
    repository_root: &Path,
    language: &str,
) -> SemanticIndex {
    let admission = mcp.call_success(
        &format!("semantic-truth-{language}-index"),
        "repo.index",
        json!({
            "root": repository_root,
            "mode": "auto",
            "detached": true
        }),
    );
    assert_success(&admission, "repo.index");
    let admission_data = data(&admission);
    let repository_id = required_string(&admission_data["repository_id"], "repository identity");
    let structural_id = required_string(
        &admission_data["operation_id"],
        "structural operation identity",
    );
    let deadline = Instant::now() + Duration::from_secs(2 * 60);
    let structural = wait_for_terminal_operation(mcp, &structural_id, deadline);
    let semantic_id = wait_for_semantic_operation(
        mcp,
        &structural_id,
        structural.semantic_operation_id.clone(),
        deadline,
    );
    assert_ne!(
        structural_id, semantic_id,
        "{language} semantic refinement must have its own durable operation identity"
    );
    let semantic =
        wait_for_release_semantic_operation(mcp, &repository_id, language, &semantic_id, deadline);
    assert_eq!(semantic.operation.state, "published");
    assert_eq!(semantic.operation.stage, "complete");
    assert!(
        semantic.operation.resources.files_examined >= 2,
        "{language} semantic refinement must retain visible work accounting"
    );
    let generation = semantic
        .published_generation
        .expect("semantic refinement publishes its generation");

    let durable = mcp.call_success(
        &format!("semantic-truth-{language}-durable-operation"),
        "operation.status",
        json!({"operation_id": semantic_id, "wait_ms": 0}),
    );
    assert_success(&durable, "operation.status");
    assert_eq!(data(&durable)["operation"]["state"], "published");
    assert_eq!(data(&durable)["published_generation"], generation);
    assert_eq!(data(&durable)["index_stage"], "complete");

    let status = mcp.call_until_not_busy(
        &format!("semantic-truth-{language}-status"),
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": generation,
            "coverage_detail": "language",
            "include_operations": true,
            "require_freshness": "semantic",
            "response_profile": "compact"
        }),
        deadline,
    );
    assert_read_success(&status, "repo.status", &generation);
    assert_terminal_language(data(&status), language);
    assert!(
        data(&status)["operations"]
            .as_array()
            .expect("repo.status returns durable operations")
            .iter()
            .any(|operation| {
                operation["operation_id"] == semantic_id && operation["state"] == "published"
            }),
        "{language} repo.status must expose the terminal semantic refinement operation"
    );
    SemanticIndex {
        repository_id,
        generation,
    }
}

struct SemanticTruthWorkflow {
    label: &'static str,
    language: &'static str,
    definition: &'static str,
    definition_path: &'static str,
    caller: &'static str,
    caller_path: &'static str,
    test: &'static str,
    test_path: &'static str,
}

fn semantic_truth_workflows() -> [SemanticTruthWorkflow; 4] {
    [
        SemanticTruthWorkflow {
            label: "typescript-component-parser",
            language: "typescript",
            definition: "parseComponent",
            definition_path: "src/compiler.ts",
            caller: "compileTemplate",
            caller_path: "src/build.ts",
            test: "parseComponentRegression",
            test_path: "tests/compiler.test.ts",
        },
        SemanticTruthWorkflow {
            label: "python-recursive-tree-builder",
            language: "python",
            definition: "build_kdtree",
            definition_path: "kdtree.py",
            caller: "nearest_tree",
            caller_path: "kdtree.py",
            test: "test_build_kdtree_recursion",
            test_path: "test_kdtree.py",
        },
        SemanticTruthWorkflow {
            label: "go-handler-registration",
            language: "go",
            definition: "GenerateHandler",
            definition_path: "server/handlers.go",
            caller: "RegisterRoutes",
            caller_path: "server/handlers.go",
            test: "TestGenerateHandler",
            test_path: "server/handlers_test.go",
        },
        SemanticTruthWorkflow {
            label: "rust-command-consumer",
            language: "rust",
            definition: "DenoSubcommand",
            definition_path: "src/commands.rs",
            caller: "run_subcommand",
            caller_path: "src/commands.rs",
            test: "deno_subcommand_routes_run",
            test_path: "tests/commands.rs",
        },
    ]
}

fn assert_semantic_truth_workflow(
    mcp: &mut McpProcess,
    index: &SemanticIndex,
    workflow: SemanticTruthWorkflow,
) {
    let definition = locate_truth_symbol(
        mcp,
        &index.repository_id,
        &index.generation,
        workflow.definition,
        workflow.definition_path,
        &format!("{}-definition", workflow.label),
    );
    let caller = locate_truth_symbol(
        mcp,
        &index.repository_id,
        &index.generation,
        workflow.caller,
        workflow.caller_path,
        &format!("{}-caller", workflow.label),
    );
    let test = locate_truth_symbol(
        mcp,
        &index.repository_id,
        &index.generation,
        workflow.test,
        workflow.test_path,
        &format!("{}-test", workflow.label),
    );
    let definition_id = required_string(&definition["symbol_id"], "definition identity");
    let caller_id = required_string(&caller["symbol_id"], "caller identity");
    let test_id = required_string(&test["symbol_id"], "test identity");

    let relationships = mcp.call_success(
        &format!("{}-relationships", workflow.label),
        "symbol.relationships",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation,
            "symbol_ids": [definition_id],
            "relations": ["calls", "references", "types"],
            "direction": "both",
            "min_confidence": 0,
            "max_results": 200,
            "response_profile": "evidence"
        }),
    );
    assert_read_success(&relationships, "symbol.relationships", &index.generation);
    assert!(
        relationship_items(data(&relationships)).iter().any(|item| {
            item["symbol_id"] == caller_id
                && item["source_refs"]
                    .as_array()
                    .is_some_and(|references| !references.is_empty())
        }),
        "{} omitted the known caller/consumer evidence: {relationships:#}",
        workflow.label
    );

    let selected = mcp.call_success(
        &format!("{}-tests", workflow.label),
        "tests.select",
        json!({
            "repository": {"repository_id": index.repository_id},
            "generation": index.generation,
            "seeds": {"symbols": [definition_id]},
            "test_kinds": ["unit", "integration"],
            "max_tests": 50,
            "include_commands": true,
            "profile": "evidence"
        }),
    );
    assert_read_success(&selected, "tests.select", &index.generation);
    assert!(
        data(&selected)["tests"]
            .as_array()
            .expect("tests.select returns ranked tests")
            .iter()
            .any(|candidate| {
                candidate["test_id"] == test_id
                    && candidate["path"] == workflow.test_path
                    && candidate["why"]
                        .as_array()
                        .is_some_and(|reasons| !reasons.is_empty())
            }),
        "{} omitted its focused test evidence: {selected:#}",
        workflow.label
    );
}

fn locate_truth_symbol(
    mcp: &mut McpProcess,
    repository_id: &str,
    generation: &str,
    query: &str,
    path: &str,
    sample: &str,
) -> Value {
    let (located, _) = locate_complete(mcp, repository_id, generation, query, path, sample);
    assert!(
        located["source_ref"].is_object(),
        "{sample} must retain generation-pinned source evidence"
    );
    located
}

fn relationship_items(output: &Value) -> Vec<&Value> {
    output["groups"]
        .as_array()
        .expect("symbol.relationships returns groups")
        .iter()
        .flat_map(|group| {
            group["items"]
                .as_array()
                .expect("a relationship group returns items")
        })
        .collect()
}

fn assert_terminal_language(status: &Value, expected: &str) {
    let languages = status["coverage"]["languages"]
        .as_array()
        .expect("repo.status returns language coverage");
    let observed = languages
        .iter()
        .find(|entry| entry["language"] == expected)
        .unwrap_or_else(|| panic!("repo.status omitted semantic language {expected}: {status:#}"));
    assert_eq!(observed["tier"], "B", "{expected} must terminate at Tier B");
    assert!(
        observed["files_indexed"]
            .as_u64()
            .is_some_and(|files| files > 0),
        "{expected} must publish indexed semantic files"
    );
}

fn write_semantic_truth_repositories(root: &Path) {
    for path in [
        "rust/src",
        "rust/tests",
        "typescript/src",
        "typescript/tests",
        "javascript/src",
        "javascript/tests",
        "python",
        "go/server",
    ] {
        fs::create_dir_all(root.join(path)).expect("semantic fixture directory is created");
    }
    for (path, source) in [
        ("rust/src/lib.rs", "pub mod commands;\n"),
        (
            "rust/src/commands.rs",
            concat!(
                "#[derive(Clone, Copy)]\n",
                "pub enum DenoSubcommand { Run, Check }\n",
                "pub fn run_subcommand(command: DenoSubcommand) -> bool {\n",
                "    matches!(command, DenoSubcommand::Run)\n",
                "}\n",
            ),
        ),
        (
            "rust/tests/commands.rs",
            concat!(
                "use crate::commands::DenoSubcommand;\n",
                "#[test]\n",
                "fn deno_subcommand_routes_run() {\n",
                "    assert!(matches!(DenoSubcommand::Run, DenoSubcommand::Run));\n",
                "}\n",
            ),
        ),
        (
            "typescript/src/compiler.ts",
            concat!(
                "export interface ComponentDescriptor { source: string }\n",
                "export function parseComponent(source: string): ComponentDescriptor {\n",
                "  return { source };\n",
                "}\n",
            ),
        ),
        (
            "typescript/src/build.ts",
            concat!(
                "import { parseComponent, type ComponentDescriptor } from \"./compiler\";\n",
                "export function compileTemplate(source: string): ComponentDescriptor {\n",
                "  return parseComponent(source);\n",
                "}\n",
            ),
        ),
        (
            "typescript/tests/compiler.test.ts",
            concat!(
                "import { parseComponent } from \"../src/compiler\";\n",
                "export function parseComponentRegression() {\n",
                "  return parseComponent(\"<template />\");\n",
                "}\n",
            ),
        ),
        (
            "javascript/src/runtime.js",
            concat!(
                "export function normalizeOptions(options) { return options; }\n",
                "export function createRuntime(options) { return normalizeOptions(options); }\n",
            ),
        ),
        (
            "javascript/tests/runtime.test.js",
            concat!(
                "import { normalizeOptions } from \"../src/runtime.js\";\n",
                "export function normalizeOptionsRegression() {\n",
                "  return normalizeOptions({ mode: \"strict\" });\n",
                "}\n",
            ),
        ),
        (
            "python/kdtree.py",
            concat!(
                "def build_kdtree(points):\n",
                "    if len(points) <= 1:\n",
                "        return points\n",
                "    return build_kdtree(points[:-1])\n",
                "\n",
                "def nearest_tree(points):\n",
                "    return build_kdtree(points)\n",
            ),
        ),
        (
            "python/test_kdtree.py",
            concat!(
                "from kdtree import build_kdtree\n",
                "\n",
                "def test_build_kdtree_recursion():\n",
                "    return build_kdtree([3, 2, 1])\n",
            ),
        ),
        (
            "go/server/handlers.go",
            concat!(
                "package server\n",
                "import \"net/http\"\n",
                "func GenerateHandler(w http.ResponseWriter, r *http.Request) {}\n",
                "func RegisterRoutes(router *http.ServeMux) {\n",
                "    router.HandleFunc(\"/generate\", GenerateHandler)\n",
                "}\n",
            ),
        ),
        (
            "go/server/handlers_test.go",
            concat!(
                "package server\n",
                "import (\n",
                "    \"net/http/httptest\"\n",
                "    \"testing\"\n",
                ")\n",
                "func TestGenerateHandler(t *testing.T) {\n",
                "    request := httptest.NewRequest(\"GET\", \"/generate\", nil)\n",
                "    GenerateHandler(httptest.NewRecorder(), request)\n",
                "}\n",
            ),
        ),
    ] {
        fs::write(root.join(path), source).expect("semantic fixture source is written");
    }
}

fn semantic_process_binaries() -> (PathBuf, PathBuf) {
    let mcp = PathBuf::from(env!("CARGO_BIN_EXE_rootlight-mcp"));
    let profile_dir = mcp
        .parent()
        .expect("MCP binary has a Cargo profile directory");
    let daemon = profile_dir.join(format!("rootlight-daemon{}", std::env::consts::EXE_SUFFIX));
    let adapter = profile_dir.join(format!(
        "rootlight-adapter-host{}",
        std::env::consts::EXE_SUFFIX
    ));
    if daemon.is_file() && adapter.is_file() {
        return (daemon, mcp);
    }

    let target_dir = profile_dir
        .parent()
        .expect("Cargo profile belongs to a target directory");
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut command = Command::new(cargo);
    command.current_dir(workspace).args([
        OsStr::new("build"),
        OsStr::new("--locked"),
        OsStr::new("--package"),
        OsStr::new("rootlight-daemon"),
        OsStr::new("--package"),
        OsStr::new("rootlight-adapter-host"),
        OsStr::new("--target-dir"),
    ]);
    command.arg(target_dir);
    if profile_dir.file_name() == Some(OsStr::new("release")) {
        command.arg("--release");
    }
    let output = command
        .output()
        .expect("test-only semantic process build starts");
    assert!(
        output.status.success(),
        "test-only semantic process build failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(daemon.is_file(), "daemon build did not produce {daemon:?}");
    assert!(
        adapter.is_file(),
        "adapter build did not produce {adapter:?}"
    );
    (daemon, mcp)
}

#[test]
#[ignore = "runs one pinned real-repository cold-index release gate"]
fn real_repository_cold_index_is_release_bounded() {
    let environment = Environment::load();
    let corpus_bytes = fs::read(&environment.corpus).expect("cold-index corpus reads");
    let corpus = load_cold_index_corpus(&environment.corpus).expect("cold-index corpus validates");
    let corpus_sha256 = cold_index_corpus_sha256(&corpus_bytes);
    let spec = corpus
        .repository(&environment.repository_id)
        .expect("selected repository is preregistered");
    let checkout = checkout_identity(&environment.repository_root);
    assert_eq!(checkout.revision, spec.revision);
    assert_eq!(checkout.tracked_files, spec.tracked_files);
    assert!(checkout.clean, "cold-index checkout must be clean");

    let fixture = process_support::private_process_tempdir("rl-cold-index-");
    let state_dir = padded_state_dir(fixture.path());
    let runtime_dir = fixture.path().join("runtime");
    let mut daemon = DaemonProcess::spawn(&environment.daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let empty_state_bytes = directory_bytes(&state_dir);
    let mut mcp = McpProcess::spawn(&environment.mcp_binary, &state_dir, &runtime_dir);

    let recovery_admission = mcp.call_success(
        "cold-index-recovery-admit",
        "repo.index",
        json!({
            "root": environment.repository_root,
            "mode": "structural",
            "detached": true
        }),
    );
    assert_success(&recovery_admission, "repo.index");
    let recovery_data = data(&recovery_admission);
    let repository_id = required_string(&recovery_data["repository_id"], "repository identity");
    let interrupted_id = required_string(&recovery_data["operation_id"], "interrupted operation");
    let interrupted_before =
        wait_for_inflight_operation(&mut mcp, &interrupted_id, Instant::now() + STARTUP_TIMEOUT);

    daemon.terminate_now();
    mcp.terminate_now();
    remove_stale_discovery(&runtime_dir);

    let mut daemon = DaemonProcess::spawn(&environment.daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&environment.mcp_binary, &state_dir, &runtime_dir);
    let interrupted_after = read_interrupted_operation(&mut mcp, &interrupted_id);

    let indexing_started = Instant::now();
    let indexing_deadline = indexing_started + Duration::from_millis(spec.maximum_elapsed_ms);
    let admitted = mcp.call_success(
        "cold-index-readmit",
        "repo.index",
        json!({
            "root": environment.repository_root,
            "mode": "auto",
            "detached": true
        }),
    );
    assert_success(&admitted, "repo.index");
    let index_data = data(&admitted);
    assert_eq!(
        required_string(&index_data["repository_id"], "reused repository identity"),
        repository_id
    );
    let repository_id = required_string(&index_data["repository_id"], "repository identity");
    let structural_id = required_string(&index_data["operation_id"], "structural operation");
    assert_ne!(structural_id, interrupted_id);
    let structural = wait_for_terminal_operation(&mut mcp, &structural_id, indexing_deadline);
    let semantic_id = wait_for_semantic_operation(
        &mut mcp,
        &structural_id,
        structural.semantic_operation_id.clone(),
        indexing_deadline,
    );
    let semantic = wait_for_terminal_operation(&mut mcp, &semantic_id, indexing_deadline);
    let generation = semantic
        .published_generation
        .clone()
        .expect("semantic refinement publishes a generation");
    let projected_journal = state_dir
        .join("first-slice/repositories")
        .join(&repository_id)
        .join(format!("stage-{generation}-0000000000000000"))
        .join("oracle.sqlite3-journal");
    let projected_journal_utf16_units =
        u64::try_from(windows_utf16_units(&projected_journal)).expect("path length fits u64");
    assert!(
        projected_journal_utf16_units > 260,
        "candidate package path fixture must cross the legacy Windows path limit"
    );
    let elapsed_ms = duration_ms(indexing_started.elapsed());
    let state_root_delta_bytes = directory_bytes(&state_dir)
        .checked_sub(empty_state_bytes)
        .expect("durable state does not shrink below its empty baseline");

    mcp.finish();
    daemon.finish();

    let mut daemon = DaemonProcess::spawn(&environment.daemon_binary, &state_dir, &runtime_dir);
    daemon.wait_until_ready(&runtime_dir);
    let mut mcp = McpProcess::spawn(&environment.mcp_binary, &state_dir, &runtime_dir);
    // IPC readiness precedes rebuilding retained search indexes, so large
    // repositories need a separate bounded recovery window.
    let status = mcp.call_until_not_busy(
        "cold-index-restart-status",
        "repo.status",
        json!({
            "repository": {"repository_id": repository_id},
            "generation": "active",
            "coverage_detail": "language",
            "include_operations": true,
            "require_freshness": "semantic",
            "response_profile": "compact"
        }),
        Instant::now() + RECOVERY_TIMEOUT,
    );
    assert_success(&status, "repo.status");
    let status_data = data(&status);
    let generation_after_restart =
        required_string(&status_data["resolved_generation"], "active generation");
    let retained_durable_bytes = required_u64(
        &status_data["retained_durable_bytes"],
        "active generation retained durable bytes",
    );
    assert_eq!(
        retained_durable_bytes, semantic.operation.resources.retained_durable_bytes,
        "repository status and the published semantic operation must identify the same retained bytes"
    );
    let primary_language_tier = language_tier(status_data, &spec.primary_language);
    let recovered_structural =
        read_terminal_operation(&mut mcp, "restart-structural", &structural_id);
    let recovered_semantic = read_terminal_operation(&mut mcp, "restart-semantic", &semantic_id);

    let located = measure_locate(
        &mut mcp,
        &repository_id,
        &generation,
        spec.lookup_query.as_str(),
        spec.expected_path.as_str(),
        spec.sample_count,
    );
    let workflow_latency_ns = measure_workflows(
        &mut mcp,
        &repository_id,
        &generation,
        &located.symbol_id,
        &located.source_ref,
        spec.sample_count,
    );
    let structural_write_amplification_milli = structural
        .operation
        .resources
        .write_amplification_milli()
        .expect("structural operation examined source bytes");
    let semantic_write_amplification_milli = semantic
        .operation
        .resources
        .write_amplification_milli()
        .expect("semantic operation examined source bytes");

    let evidence = ColdIndexEvidence {
        schema: COLD_INDEX_EVIDENCE_SCHEMA.to_owned(),
        source_revision: environment.source_revision,
        candidate_version: environment.candidate_version,
        candidate_archive_sha256: environment.candidate_sha256,
        daemon_sha256: file_sha256(&environment.daemon_binary),
        mcp_sha256: file_sha256(&environment.mcp_binary),
        corpus_sha256: corpus_sha256.clone(),
        corpus_repository_id: spec.id.clone(),
        repository_revision: checkout.revision,
        tracked_files: checkout.tracked_files,
        repository_id: repository_id.clone(),
        structural_operation: structural.operation,
        semantic_operation: semantic.operation,
        structural_write_amplification_milli,
        semantic_write_amplification_milli,
        elapsed_ms,
        state_root_delta_bytes,
        retained_durable_bytes,
        primary_language_tier,
        restart: ColdIndexRestartEvidence {
            interrupted: ColdIndexInterruptedRecoveryEvidence {
                operation_id: interrupted_id,
                repository_id: repository_id.clone(),
                state_before_restart: interrupted_before.state,
                revision_before_restart: interrupted_before.revision,
                resources_before_restart: interrupted_before.resources,
                state_after_restart: interrupted_after.state,
                revision_after_restart: interrupted_after.revision,
                resources_after_restart: interrupted_after.resources,
                repository_id_reused: true,
            },
            generation_before_restart: generation,
            generation_after_restart,
            repository_ready: status_data["repository_state"] == "ready",
            structural_operation_recovered: recovered_structural.complete,
            semantic_operation_recovered: recovered_semantic.complete,
            semantic_revision_after_restart: recovered_semantic.revision,
            semantic_resources_after_restart: recovered_semantic.resources,
            projected_journal_utf16_units,
        },
        locate: ColdIndexLocateEvidence {
            query: spec.lookup_query.clone(),
            matched_path: located.path,
            symbol_id: located.symbol_id,
            latency_ns: located.latency_ns,
        },
        workflow_latency_ns,
        repository_content_executed: false,
    };
    if let Err(error) = verify_cold_index_evidence(
        &corpus,
        &corpus_sha256,
        &evidence,
        &evidence.source_revision,
        &evidence.candidate_archive_sha256,
    ) {
        panic!("cold-index evidence violates checked release policy ({error:?}): {evidence:#?}");
    }
    let encoded = encode_cold_index_evidence(&evidence).expect("cold-index evidence encodes");
    fs::write(&environment.evidence, encoded).expect("cold-index evidence writes");

    mcp.finish();
    daemon.finish();
}

struct Environment {
    corpus: PathBuf,
    repository_id: String,
    repository_root: PathBuf,
    evidence: PathBuf,
    daemon_binary: PathBuf,
    mcp_binary: PathBuf,
    candidate_sha256: String,
    candidate_version: String,
    source_revision: String,
}

impl Environment {
    fn load() -> Self {
        Self {
            corpus: required_path("ROOTLIGHT_COLD_INDEX_CORPUS"),
            repository_id: required_env("ROOTLIGHT_COLD_INDEX_REPOSITORY_ID"),
            repository_root: required_path("ROOTLIGHT_COLD_INDEX_REPOSITORY_ROOT"),
            evidence: required_path("ROOTLIGHT_COLD_INDEX_EVIDENCE"),
            daemon_binary: required_file("ROOTLIGHT_COLD_INDEX_DAEMON_BIN"),
            mcp_binary: required_file("ROOTLIGHT_COLD_INDEX_MCP_BIN"),
            candidate_sha256: required_env("ROOTLIGHT_CANDIDATE_ARCHIVE_SHA256"),
            candidate_version: required_env("ROOTLIGHT_RELEASE_VERSION"),
            source_revision: required_env("SOURCE_REVISION"),
        }
    }
}

fn required_env(name: &str) -> String {
    let value = std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set"));
    assert!(!value.is_empty() && value.len() <= 16 * 1024, "{name}");
    value
}

fn required_path(name: &str) -> PathBuf {
    PathBuf::from(required_env(name))
}

fn required_file(name: &str) -> PathBuf {
    let path = required_path(name);
    assert!(path.is_file(), "{name} must name a regular file");
    path
}

struct CheckoutIdentity {
    revision: String,
    tracked_files: u64,
    clean: bool,
}

fn checkout_identity(root: &Path) -> CheckoutIdentity {
    let revision = git_output(root, &["rev-parse", "HEAD"]);
    let tracked = git_output_bytes(root, &["ls-files", "-z"]);
    CheckoutIdentity {
        revision,
        tracked_files: u64::try_from(tracked.iter().filter(|byte| **byte == 0).count())
            .expect("tracked-file count fits u64"),
        clean: git_output(root, &["status", "--porcelain=v1"]).is_empty(),
    }
}

fn git_output(root: &Path, arguments: &[&str]) -> String {
    String::from_utf8(git_output_bytes(root, arguments))
        .expect("git output is UTF-8")
        .trim()
        .to_owned()
}

fn git_output_bytes(root: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .expect("git checkout identity command starts");
    assert!(
        output.status.success(),
        "git checkout identity command succeeds: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

struct TerminalOperation {
    operation: ColdIndexOperationEvidence,
    published_generation: Option<String>,
    semantic_operation_id: Option<String>,
}

struct RecoveredOperation {
    complete: bool,
    revision: u64,
    resources: ColdIndexResourceEvidence,
}

struct InflightOperation {
    state: String,
    revision: u64,
    resources: ColdIndexResourceEvidence,
}

fn wait_for_inflight_operation(
    mcp: &mut McpProcess,
    operation_id: &str,
    deadline: Instant,
) -> InflightOperation {
    let mut attempt = 0_u64;
    loop {
        assert!(
            Instant::now() < deadline,
            "operation {operation_id} did not enter durable execution"
        );
        let response = mcp.call_success(
            &format!("cold-index-inflight-{attempt}"),
            "operation.status",
            json!({"operation_id": operation_id, "wait_ms": 0}),
        );
        assert_success(&response, "operation.status");
        let operation = &data(&response)["operation"];
        let state = required_string(&operation["state"], "in-flight operation state");
        let revision = required_u64(&operation["revision"], "in-flight operation revision");
        if matches!(state.as_str(), "queued" | "running")
            && (revision > 1
                || operation["progress"]["completed_units"]
                    .as_u64()
                    .is_some_and(|completed| completed > 0))
        {
            return InflightOperation {
                state,
                revision,
                resources: operation_resources(operation),
            };
        }
        assert!(
            !matches!(state.as_str(), "published" | "failed" | "cancelled"),
            "recovery probe terminated before forced restart: {response:#}"
        );
        attempt = attempt.saturating_add(1);
        thread::sleep(POLL_INTERVAL);
    }
}

fn read_interrupted_operation(mcp: &mut McpProcess, operation_id: &str) -> InflightOperation {
    let response = mcp.call_success(
        "cold-index-interrupted-after-restart",
        "operation.status",
        json!({"operation_id": operation_id, "wait_ms": 0}),
    );
    assert_success(&response, "operation.status");
    let operation_data = data(&response);
    let operation = &operation_data["operation"];
    assert_eq!(operation["state"], "failed");
    assert!(
        !operation_data["error"].is_null(),
        "interrupted operation must retain a typed terminal error"
    );
    InflightOperation {
        state: required_string(&operation["state"], "interrupted operation state"),
        revision: required_u64(&operation["revision"], "interrupted operation revision"),
        resources: operation_resources(operation),
    }
}

fn wait_for_terminal_operation(
    mcp: &mut McpProcess,
    operation_id: &str,
    deadline: Instant,
) -> TerminalOperation {
    wait_for_terminal_operation_with_diagnostics(mcp, operation_id, deadline, None)
}

fn wait_for_release_semantic_operation(
    mcp: &mut McpProcess,
    repository_id: &str,
    language: &str,
    operation_id: &str,
    deadline: Instant,
) -> TerminalOperation {
    wait_for_terminal_operation_with_diagnostics(
        mcp,
        operation_id,
        deadline,
        Some((repository_id, language)),
    )
}

fn wait_for_terminal_operation_with_diagnostics(
    mcp: &mut McpProcess,
    operation_id: &str,
    deadline: Instant,
    diagnostic_repository: Option<(&str, &str)>,
) -> TerminalOperation {
    let mut revision = None;
    let mut attempt = 0_u64;
    let mut progress_samples = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "operation {operation_id} exceeded its preregistered deadline"
        );
        let response = mcp.call_success(
            &format!("cold-index-operation-{attempt}"),
            "operation.status",
            operation_status_arguments(operation_id, revision),
        );
        assert_success(&response, "operation.status");
        let operation_data = data(&response);
        let operation = &operation_data["operation"];
        revision = operation["revision"].as_u64();
        match operation["state"].as_str() {
            Some("published") => {
                return terminal_operation(operation_id, operation_data, progress_samples);
            }
            Some("failed" | "cancelled" | "waiting_for_context") => {
                if let Some((repository_id, language)) = diagnostic_repository {
                    let state =
                        required_string(&operation["state"], "terminal semantic operation state");
                    let completed_units = required_u64(
                        &operation["progress"]["completed_units"],
                        "terminal semantic completed units",
                    );
                    let total_units = required_u64(
                        &operation["progress"]["total_units"],
                        "terminal semantic total units",
                    );
                    let status = mcp.call_until_not_busy(
                        &format!("semantic-truth-{language}-terminal-status"),
                        "repo.status",
                        json!({
                            "repository": {"repository_id": repository_id},
                            "generation": "active",
                            "coverage_detail": "language",
                            "include_operations": true,
                            "require_freshness": "none",
                            "response_profile": "compact"
                        }),
                        Instant::now() + STARTUP_TIMEOUT,
                    );
                    panic!(
                        "{language} semantic operation {operation_id} terminated in state \
                         {state} at {completed_units}/{total_units} units without publication:\n\
                         operation.status = {response:#}\nrepo.status = {status:#}"
                    );
                }
                panic!("operation {operation_id} terminated without publication: {response:#}")
            }
            Some("queued" | "running") => {
                if let Some(sample) = operation_progress_sample(operation) {
                    record_progress_sample(&mut progress_samples, sample);
                }
            }
            _ => panic!("operation returned an unknown state: {response:#}"),
        }
        attempt = attempt.saturating_add(1);
    }
}

fn wait_for_semantic_operation(
    mcp: &mut McpProcess,
    structural_operation_id: &str,
    initial: Option<String>,
    indexing_deadline: Instant,
) -> String {
    if let Some(operation_id) = initial {
        return operation_id;
    }
    let admission_deadline = indexing_deadline.min(Instant::now() + STARTUP_TIMEOUT);
    let mut attempt = 0_u64;
    loop {
        assert!(
            Instant::now() < admission_deadline,
            "auto index did not durably register its semantic operation"
        );
        let response = mcp.call_success(
            &format!("cold-index-semantic-admission-{attempt}"),
            "operation.status",
            json!({"operation_id": structural_operation_id, "wait_ms": 0}),
        );
        assert_success(&response, "operation.status");
        let operation_data = data(&response);
        if let Some(operation_id) = operation_data["semantic_operation_id"].as_str() {
            return operation_id.to_owned();
        }
        assert_eq!(
            operation_data["operation"]["state"], "published",
            "structural operation changed state while awaiting semantic admission"
        );
        attempt = attempt.saturating_add(1);
        thread::sleep(POLL_INTERVAL);
    }
}

fn operation_status_arguments(operation_id: &str, revision: Option<u64>) -> Value {
    let mut arguments = json!({
        "operation_id": operation_id,
        "wait_ms": revision.map_or(0, |_| 5_000)
    });
    if let Some(after_revision) = revision {
        arguments["after_revision"] = json!(after_revision);
    }
    arguments
}

#[test]
fn initial_operation_status_poll_omits_after_revision() {
    assert_eq!(
        operation_status_arguments("op1_fixture", None),
        json!({"operation_id": "op1_fixture", "wait_ms": 0})
    );
    assert_eq!(
        operation_status_arguments("op1_fixture", Some(7)),
        json!({
            "operation_id": "op1_fixture",
            "wait_ms": 5_000,
            "after_revision": 7
        })
    );
}

fn terminal_operation(
    operation_id: &str,
    data: &Value,
    progress_samples: Vec<ColdIndexProgressEvidence>,
) -> TerminalOperation {
    let operation = &data["operation"];
    let progress = &operation["progress"];
    TerminalOperation {
        operation: ColdIndexOperationEvidence {
            operation_id: operation_id.to_owned(),
            state: required_string(&operation["state"], "terminal operation state"),
            stage: required_string(&data["index_stage"], "terminal index stage"),
            revision: required_u64(&operation["revision"], "terminal operation revision"),
            completed_units: required_u64(
                &progress["completed_units"],
                "terminal completed progress",
            ),
            total_units: required_u64(&progress["total_units"], "terminal total progress"),
            resources: operation_resources(operation),
            progress_samples,
        },
        published_generation: data["published_generation"].as_str().map(str::to_owned),
        semantic_operation_id: data["semantic_operation_id"].as_str().map(str::to_owned),
    }
}

fn operation_progress_sample(operation: &Value) -> Option<ColdIndexProgressEvidence> {
    let progress = &operation["progress"];
    let total_units = progress["total_units"].as_u64()?;
    Some(ColdIndexProgressEvidence {
        revision: operation["revision"].as_u64()?,
        completed_units: progress["completed_units"].as_u64()?,
        total_units,
        resources: operation_resources(operation),
    })
}

fn record_progress_sample(
    samples: &mut Vec<ColdIndexProgressEvidence>,
    sample: ColdIndexProgressEvidence,
) {
    if let Some(previous) = samples.last_mut()
        && previous.revision == sample.revision
    {
        *previous = sample;
        return;
    }
    if samples.len() < MAX_PROGRESS_SAMPLES {
        samples.push(sample);
    } else if let Some(last) = samples.last_mut() {
        *last = sample;
    }
}

fn operation_resources(operation: &Value) -> ColdIndexResourceEvidence {
    let resources = &operation["resources"];
    ColdIndexResourceEvidence {
        peak_rss_bytes: required_u64(&resources["peak_rss_bytes"], "peak RSS"),
        written_bytes: required_u64(&resources["written_bytes"], "written bytes"),
        files_examined: required_u64(&resources["files_examined"], "files examined"),
        bytes_examined: required_u64(&resources["bytes_examined"], "bytes examined"),
        retained_durable_bytes: required_u64(
            &resources["retained_durable_bytes"],
            "retained durable bytes",
        ),
    }
}

fn read_terminal_operation(
    mcp: &mut McpProcess,
    request_id: &str,
    operation_id: &str,
) -> RecoveredOperation {
    let response = mcp.call_success(
        request_id,
        "operation.status",
        json!({"operation_id": operation_id, "wait_ms": 0}),
    );
    assert_success(&response, "operation.status");
    let operation_data = data(&response);
    let operation = &operation_data["operation"];
    RecoveredOperation {
        complete: operation["state"] == "published" && operation_data["index_stage"] == "complete",
        revision: required_u64(&operation["revision"], "recovered operation revision"),
        resources: operation_resources(operation),
    }
}

fn language_tier(status: &Value, language: &str) -> ColdIndexTierEvidence {
    status["coverage"]["languages"]
        .as_array()
        .and_then(|languages| languages.iter().find(|entry| entry["language"] == language))
        .map(|entry| ColdIndexTierEvidence {
            language: language.to_owned(),
            tier: required_string(&entry["tier"], "primary language tier"),
            indexed_files: required_u64(&entry["files_indexed"], "primary language file count"),
        })
        .unwrap_or_else(|| panic!("repo.status omitted primary language {language}: {status:#}"))
}

struct Located {
    path: String,
    symbol_id: String,
    source_ref: Value,
    latency_ns: Vec<u64>,
}

fn measure_locate(
    mcp: &mut McpProcess,
    repository_id: &str,
    generation: &str,
    query: &str,
    expected_path: &str,
    sample_count: usize,
) -> Located {
    let warmup = locate_complete(
        mcp,
        repository_id,
        generation,
        query,
        expected_path,
        "warmup",
    );
    let mut latency_ns = Vec::with_capacity(sample_count);
    let mut selected = warmup.0;
    for ordinal in 0..sample_count {
        let started = Instant::now();
        let measured = locate_complete(
            mcp,
            repository_id,
            generation,
            query,
            expected_path,
            &format!("measured-{ordinal}"),
        );
        latency_ns.push(duration_ns(started.elapsed()).max(1));
        selected = measured.0;
    }
    Located {
        path: required_string(&selected["path"], "located path"),
        symbol_id: required_string(&selected["symbol_id"], "located symbol identity"),
        source_ref: selected["source_ref"].clone(),
        latency_ns,
    }
}

fn locate_complete(
    mcp: &mut McpProcess,
    repository_id: &str,
    generation: &str,
    query: &str,
    expected_path: &str,
    sample: &str,
) -> (Value, usize) {
    let mut cursor = None;
    let mut selected = None;
    let mut observed_paths = BTreeMap::<String, usize>::new();
    let mut pages = 0_usize;
    loop {
        assert!(pages < 128, "code.locate pagination must remain bounded");
        let response = mcp.call_success(
            &format!("cold-index-locate-{sample}-{pages}"),
            "code.locate",
            locate_arguments(repository_id, generation, query, cursor.as_deref()),
        );
        assert_read_success(&response, "code.locate", generation);
        for item in data(&response)["matches"]
            .as_array()
            .expect("code.locate matches are an array")
        {
            if let Some(path) = item["path"].as_str() {
                *observed_paths.entry(path.to_owned()).or_default() += 1;
            }
            if item["path"] == expected_path && item["symbol_id"].is_string() {
                selected = Some(item.clone());
            }
        }
        pages = pages.saturating_add(1);
        cursor = response["result"]["structuredContent"]["next_cursor"]
            .as_str()
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    (
        selected.unwrap_or_else(|| {
            panic!(
                "code.locate did not return preregistered path {expected_path}; observed paths: {observed_paths:#?}"
            )
        }),
        pages,
    )
}

fn locate_arguments(
    repository_id: &str,
    generation: &str,
    query: &str,
    cursor: Option<&str>,
) -> Value {
    let mut arguments = json!({
        "repository": {"repository_id": repository_id},
        "generation": generation,
        "query": query,
        "search_modes": ["exact"],
        "max_results": 200,
        "response_profile": "compact"
    });
    if let Some(cursor) = cursor {
        arguments["cursor"] = json!(cursor);
    }
    arguments
}

#[test]
fn initial_locate_page_omits_cursor() {
    assert_eq!(
        locate_arguments("repo1_fixture", "gen1_fixture", "Symbol", None),
        json!({
            "repository": {"repository_id": "repo1_fixture"},
            "generation": "gen1_fixture",
            "query": "Symbol",
            "search_modes": ["exact"],
            "max_results": 200,
            "response_profile": "compact"
        })
    );
    assert_eq!(
        locate_arguments(
            "repo1_fixture",
            "gen1_fixture",
            "Symbol",
            Some("cursor1_fixture")
        )["cursor"],
        "cursor1_fixture"
    );
}

fn measure_workflows(
    mcp: &mut McpProcess,
    repository_id: &str,
    generation: &str,
    symbol_id: &str,
    source_ref: &Value,
    sample_count: usize,
) -> BTreeMap<String, Vec<u64>> {
    let cases = [
        (
            "architecture.overview",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": generation,
                "response_profile": "compact"
            }),
        ),
        (
            "context.pack",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": generation,
                "task": "explain the selected symbol",
                "seeds": {"symbols": [symbol_id]},
                "token_budget": 4_500,
                "response_profile": "compact"
            }),
        ),
        (
            "source.read",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": generation,
                "references": [{"source_ref": source_ref}],
                "include_line_numbers": false,
                "encoding": "utf8_lossless_when_valid",
                "response_profile": "compact"
            }),
        ),
        (
            "symbol.explain",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": generation,
                "symbol_ids": [symbol_id],
                "response_profile": "compact"
            }),
        ),
        (
            "symbol.relationships",
            json!({
                "repository": {"repository_id": repository_id},
                "generation": generation,
                "symbol_ids": [symbol_id],
                "relations": ["calls"],
                "direction": "both",
                "max_results": 20,
                "response_profile": "compact"
            }),
        ),
    ];
    let mut evidence = BTreeMap::new();
    for (tool, arguments) in cases {
        let warmup = mcp.call_success(
            &format!("cold-index-{tool}-warmup"),
            tool,
            arguments.clone(),
        );
        assert_read_success(&warmup, tool, generation);
        let mut samples = Vec::with_capacity(sample_count);
        for ordinal in 0..sample_count {
            let started = Instant::now();
            let response = mcp.call_success(
                &format!("cold-index-{tool}-{ordinal}"),
                tool,
                arguments.clone(),
            );
            let elapsed = duration_ns(started.elapsed()).max(1);
            assert_read_success(&response, tool, generation);
            samples.push(elapsed);
        }
        evidence.insert(tool.to_owned(), samples);
    }
    evidence
}

fn assert_read_success(response: &Value, tool: &str, generation: &str) {
    assert_success(response, tool);
    let structured = &response["result"]["structuredContent"];
    assert_eq!(
        structured["generation"]["generation_id"], generation,
        "{tool} must remain pinned to the recovered generation"
    );
    assert_eq!(
        structured["trust"], "untrusted_repository_data",
        "{tool} must retain repository-data trust"
    );
    assert!(
        structured["data"].is_object(),
        "{tool} must return typed data"
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

fn data(response: &Value) -> &Value {
    &response["result"]["structuredContent"]["data"]
}

fn required_string(value: &Value, field: &str) -> String {
    value
        .as_str()
        .unwrap_or_else(|| panic!("{field} is absent: {value:#}"))
        .to_owned()
}

fn required_u64(value: &Value, field: &str) -> u64 {
    value
        .as_u64()
        .unwrap_or_else(|| panic!("{field} is absent: {value:#}"))
}

fn directory_bytes(root: &Path) -> u64 {
    let mut total = 0_u64;
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        if !path.exists() {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).expect("durable state metadata reads");
        if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .expect("durable state size fits u64");
        } else if metadata.is_dir() {
            pending.extend(
                fs::read_dir(path)
                    .expect("durable state directory reads")
                    .map(|entry| entry.expect("durable state entry reads").path()),
            );
        } else {
            panic!("durable state contains an unsupported filesystem entry");
        }
    }
    total
}

fn file_sha256(path: &Path) -> String {
    let mut input = fs::File::open(path).expect("candidate binary opens");
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = input.read(&mut buffer).expect("candidate binary reads");
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    digest
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut encoded, byte| {
            write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
            encoded
        })
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis())
        .unwrap_or(u64::MAX)
        .max(1)
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn padded_state_dir(root: &Path) -> PathBuf {
    const TARGET_UTF16_UNITS: usize = 120;

    let state = root.join("state");
    let current = windows_utf16_units(&state);
    if current >= TARGET_UTF16_UNITS {
        return state;
    }
    root.join(
        "s".repeat(
            TARGET_UTF16_UNITS
                .checked_sub(windows_utf16_units(root) + 1)
                .expect("temporary path leaves room for the padded state component"),
        ),
    )
}

#[cfg(windows)]
fn windows_utf16_units(path: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt as _;

    path.as_os_str().encode_wide().count()
}

#[cfg(not(windows))]
fn windows_utf16_units(path: &Path) -> usize {
    path.to_string_lossy().encode_utf16().count()
}

fn remove_stale_discovery(runtime_dir: &Path) {
    let discovery = runtime_dir.join("daemon.json");
    match fs::remove_file(discovery) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("stale daemon discovery is removed: {error}"),
    }
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
            .expect("isolated candidate daemon starts");
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

    fn wait_until_ready(&mut self, runtime_dir: &Path) {
        let discovery = runtime_dir.join("daemon.json");
        let deadline = Instant::now() + STARTUP_TIMEOUT;
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
                "candidate daemon exited before readiness"
            );
            thread::sleep(POLL_INTERVAL);
        }
        panic!("candidate daemon did not publish readiness within the startup bound");
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
            "candidate daemon exits successfully: {stderr}"
        );
    }

    fn terminate_now(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
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
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<String>>,
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
            .expect("candidate MCP process starts");
        let output = child.stdout.take().expect("MCP stdout is piped");
        let stderr = child.stderr.take().expect("MCP stderr is piped");
        let input = child.stdin.take();
        let (responses_tx, responses) = mpsc::sync_channel(64);
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let Ok(line) = line else {
                    return;
                };
                if responses_tx.send(line).is_err() {
                    return;
                }
            }
        });
        let stderr_reader = thread::spawn(move || {
            let mut output = String::new();
            BufReader::new(stderr)
                .read_to_string(&mut output)
                .expect("MCP stderr reads");
            output
        });
        let mut process = Self {
            child: Some(child),
            input,
            responses,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
        };
        process.write(&json!({
            "jsonrpc": "2.0",
            "id": "initialize",
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "cold-index-release", "version": "1.0"},
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
        assert_eq!(response["id"], id, "MCP response identity differs");
        response
    }

    fn call_success(&mut self, id: &str, tool: &str, arguments: Value) -> Value {
        process_support::retry_transient_busy(id, |attempt| {
            self.call(attempt, tool, arguments.clone())
        })
    }

    fn call_until_not_busy(
        &mut self,
        id: &str,
        tool: &str,
        arguments: Value,
        deadline: Instant,
    ) -> Value {
        let mut attempt = 1_u64;
        loop {
            assert!(
                Instant::now() < deadline,
                "{tool} remained busy beyond its recovery deadline"
            );
            let response = self.call(&format!("{id}-attempt-{attempt}"), tool, arguments.clone());
            let error = &response["result"]["structuredContent"]["error"];
            if error["code"] != "BUSY" || error["retryable"] != true {
                return response;
            }
            let retry_after = Duration::from_millis(
                error["retry_after_ms"]
                    .as_u64()
                    .unwrap_or(u64::try_from(POLL_INTERVAL.as_millis()).unwrap_or(100)),
            );
            thread::sleep(retry_after.min(deadline.saturating_duration_since(Instant::now())));
            attempt = attempt.saturating_add(1);
        }
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
        self.child.take();
        self.stdout_reader
            .take()
            .expect("MCP stdout reader is retained")
            .join()
            .expect("MCP stdout reader joins");
        let stderr = self
            .stderr_reader
            .take()
            .expect("MCP stderr reader is retained")
            .join()
            .expect("MCP stderr reader joins");
        assert!(
            status.success(),
            "candidate MCP exits successfully: {stderr}"
        );
        assert!(stderr.is_empty(), "candidate MCP wrote stderr: {stderr}");
    }

    fn terminate_now(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        self.input.take();
        terminate(&mut self.child);
        if let Some(reader) = self.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = self.stderr_reader.take() {
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
