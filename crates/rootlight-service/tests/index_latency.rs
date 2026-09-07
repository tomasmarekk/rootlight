//! Release-only latency observations over deterministic single-repository workloads.
//!
//! Correctness and work counters accompany raw timings; filesystem caches are
//! not flushed, so an empty Rootlight state is not an OS-cold measurement.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash};
use rootlight_query::LocateMode;
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceIndexCommit, FirstSliceIndexOperationStrategy, FirstSliceService,
};
use serde_json::{Value, json};
use tempfile::TempDir;

const SAMPLES: usize = 3;
const FILE_COUNTS: [usize; 2] = [16, 128];
const OPERATION_WATCHDOG_SECONDS: u64 = 300;

#[derive(Clone, Copy, Debug)]
enum Workload {
    Rust,
    Ruby,
    Lua,
    Tsx,
    Json,
}

impl Workload {
    fn label(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Ruby => "ruby",
            Self::Lua => "lua",
            Self::Tsx => "tsx",
            Self::Json => "json",
        }
    }

    fn language(self) -> &'static str {
        match self {
            Self::Tsx => "typescript",
            _ => self.label(),
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Rust => "rs",
            Self::Ruby => "rb",
            Self::Lua => "lua",
            Self::Tsx => "tsx",
            Self::Json => "json",
        }
    }

    fn source(self, ordinal: usize, body: u32) -> String {
        match self {
            Self::Rust => format!("pub fn value_{ordinal}() -> u32 {{ {body} }}\n"),
            Self::Ruby => {
                format!("class Item{ordinal}\n def value_{ordinal}\n  {body}\n end\nend\n")
            }
            Self::Lua => format!("local function value_{ordinal}() return {body} end\n"),
            Self::Tsx => {
                format!("export function value_{ordinal}() {{ return <span>{{{body}}}</span>; }}\n")
            }
            Self::Json => format!("{{\"marker\":\"value_{ordinal}\",\"value\":{body}}}\n"),
        }
    }

    fn path(self, ordinal: usize) -> String {
        format!("item_{ordinal:03}.{}", self.extension())
    }
}

const WORKLOADS: [Workload; 5] = [
    Workload::Rust,
    Workload::Ruby,
    Workload::Lua,
    Workload::Tsx,
    Workload::Json,
];

#[test]
fn latency_workloads_change_only_the_selected_body() {
    for workload in WORKLOADS {
        for ordinal in [0, 127] {
            let before = workload.source(ordinal, 1);
            let after = workload.source(ordinal, 2);
            assert_ne!(
                content_hash(before.as_bytes()),
                content_hash(after.as_bytes())
            );
            assert!(before.contains(&format!("value_{ordinal}")));
            assert!(after.contains(&format!("value_{ordinal}")));
            assert_eq!(before.len(), after.len());
            assert_eq!(
                before
                    .bytes()
                    .zip(after.bytes())
                    .filter(|(a, b)| a != b)
                    .count(),
                1
            );
        }
    }
}

#[test]
#[ignore = "runs release-only durable indexing latency observations with raw evidence"]
fn durable_index_latency_observations() -> Result<(), &'static str> {
    if cfg!(debug_assertions) {
        return Err("latency observations require --release");
    }
    for file_count in FILE_COUNTS {
        for workload in WORKLOADS {
            // One explicit warm-up per shape is retained but excluded from comparisons.
            for ordinal in 0..=SAMPLES {
                let observation = observe(workload, file_count, ordinal);
                println!(
                    "INDEX_LATENCY {}",
                    serde_json::to_string(&observation).expect("observation serializes")
                );
            }
        }
    }
    Ok(())
}

fn observe(workload: Workload, file_count: usize, ordinal: usize) -> Value {
    let fixture = TempDir::new().expect("private fixture root exists");
    let root = fixture.path().join("repository");
    fs::create_dir(&root).expect("repository directory creates");
    let mut hasher = blake3::Hasher::new();
    let mut source_bytes = 0usize;
    for file in 0..file_count {
        let path = workload.path(file);
        let source = workload.source(file, 1);
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(source.as_bytes());
        hasher.update(&[0]);
        source_bytes = source_bytes
            .checked_add(source.len())
            .expect("bounded fixture size fits");
        fs::write(root.join(path), source).expect("fixture source writes");
    }
    let paths = RuntimePaths::new(fixture.path().join("state"), fixture.path().join("runtime"))
        .expect("private runtime paths are valid");
    paths.prepare_owner().expect("private runtime prepares");
    let started = Instant::now();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &deadline())
        .expect("production durable service opens");
    let open_micros = micros(started);
    let (initial, initial_timing) = index(&mut service, &root);
    let (noop, noop_timing) = index(&mut service, &root);
    assert_eq!(
        noop.receipt(),
        initial.receipt(),
        "no-op retains the exact generation receipt"
    );
    assert_eq!(noop.evidence().changed_files, 0);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    assert_eq!(
        noop.evidence().reused_files,
        initial.receipt().indexed_files
    );
    // Retention still publishes durable activation metadata; report those bytes.
    assert_eq!(
        noop.evidence().strategy,
        FirstSliceIndexOperationStrategy::RetainedGeneration
    );

    let changed_source = workload.source(0, 2);
    fs::write(root.join(workload.path(0)), &changed_source).expect("body mutation writes");
    let (changed, changed_timing) = index(&mut service, &root);
    assert_eq!(changed.receipt().parent, Some(initial.receipt().generation));
    assert_ne!(changed.receipt().generation, initial.receipt().generation);
    assert_eq!(changed.evidence().changed_files, 1);
    if !matches!(workload, Workload::Json) {
        assert_eq!(changed.evidence().rebuilt_files, 1);
        assert_eq!(
            changed.evidence().reused_files,
            initial.receipt().indexed_files - 1
        );
    }
    assert_eq!(
        changed.evidence().rebuilt_facts + changed.evidence().reused_facts,
        initial.evidence().rebuilt_facts,
        "a same-shape body edit conserves normalized fact work"
    );
    for commit in [&initial, &changed] {
        assert!(commit.receipt().discovery_complete);
        assert_eq!(commit.receipt().excluded_inputs, 0);
        assert_eq!(
            commit.receipt().discovered_inputs,
            u64::try_from(file_count).expect("count fits")
        );
        assert_eq!(
            commit.receipt().indexed_files,
            u64::try_from(file_count).expect("count fits")
        );
    }
    let original_symbol = verify_source(
        &service,
        initial.receipt().generation,
        workload,
        &workload.source(0, 1),
    );
    let changed_symbol = verify_source(
        &service,
        changed.receipt().generation,
        workload,
        &changed_source,
    );
    assert_eq!(
        original_symbol, changed_symbol,
        "body edits preserve the declaration identity"
    );
    assert_eq!(
        original_symbol.is_some(),
        !matches!(workload, Workload::Json)
    );
    let status = service
        .repository_status(changed.receipt().repository, None)
        .expect("per-language coverage remains available");
    assert_eq!(status.coverage.len(), 1);
    assert_eq!(status.coverage[0].language, workload.language());
    assert_eq!(
        status.coverage[0].discovered_files,
        u64::try_from(file_count).expect("count fits")
    );
    assert_eq!(
        status.coverage[0].indexed_files,
        if matches!(workload, Workload::Json) {
            0
        } else {
            u64::try_from(file_count).expect("count fits")
        }
    );
    json!({
        "schema": "rootlight.index-latency-observation/1",
        "workload": workload.label(), "files": file_count, "source_bytes": source_bytes,
        "fixture_blake3": hasher.finalize().to_hex().to_string(),
        "sample": ordinal, "phase": if ordinal == 0 { "warmup" } else { "measured" },
        "profile": "release", "storage": "durable", "analysis": "structural",
        "os_cache": "not_flushed", "initial_state": "empty", "service_open_micros": open_micros,
        "operation_watchdog_seconds": OPERATION_WATCHDOG_SECONDS,
        "coverage": {
            "language": status.coverage[0].language, "tier": status.coverage[0].tier,
            "status": status.coverage[0].status, "discovered_files": status.coverage[0].discovered_files,
            "indexed_files": status.coverage[0].indexed_files,
        },
        "initial": stage(&initial, initial_timing),
        "noop": stage(&noop, noop_timing),
        "body_edit": stage(&changed, changed_timing),
        "exact_old_and_new_source_verified": true,
        "symbol_identity_preserved": original_symbol.map(|_| true),
        "installed_mcp_measurement": false, "full_language_support_claimed": false,
    })
}

struct StageTiming {
    wall_micros: u64,
    preparation_micros: u64,
    publication_micros: u64,
}

fn index(service: &mut FirstSliceService, root: &Path) -> (FirstSliceIndexCommit, StageTiming) {
    let cancellation = deadline();
    let started = Instant::now();
    let prepared = service
        .prepare_repository(root, &cancellation)
        .expect("repository prepares");
    let preparation_micros = micros(started);
    let commit = service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .expect("generation publishes");
    let wall_micros = micros(started);
    (
        commit,
        StageTiming {
            wall_micros,
            preparation_micros,
            publication_micros: wall_micros - preparation_micros,
        },
    )
}

fn verify_source(
    service: &FirstSliceService,
    generation: GenerationId,
    workload: Workload,
    expected: &str,
) -> Option<SymbolId> {
    let located = service
        .code_locate(
            generation,
            "value_0".to_owned(),
            LocateMode::Exact,
            8,
            0,
            &deadline(),
        )
        .expect("anchor is queryable");
    let hit = located
        .data
        .hits
        .iter()
        .find(|hit| hit.path == workload.path(0))
        .expect("anchor belongs to the expected file");
    assert_eq!(hit.language, workload.language());
    let reference = hit.source.clone().expect("anchor has source evidence");
    assert_eq!(reference.generation(), generation);
    let source = service
        .source_read(generation, vec![reference], &deadline())
        .expect("anchor source reads");
    assert_eq!(source.data.chunks[0].bytes, expected.as_bytes());
    hit.symbol
}

fn stage(commit: &FirstSliceIndexCommit, timing: StageTiming) -> Value {
    let evidence = commit.evidence();
    json!({
        "wall_micros": timing.wall_micros,
        "preparation_micros": timing.preparation_micros,
        "publication_micros": timing.publication_micros,
        "strategy": format!("{:?}", evidence.strategy),
        "fallback_reason": evidence.fallback_reason.map(|reason| format!("{reason:?}")),
        "generation_receipt_elapsed_micros": commit.receipt().elapsed_micros,
        "changed_files": evidence.changed_files, "rebuilt_files": evidence.rebuilt_files,
        "reused_files": evidence.reused_files, "rebuilt_facts": evidence.rebuilt_facts,
        "reused_facts": evidence.reused_facts, "newly_written_bytes": evidence.newly_written_bytes,
        "referenced_bytes": evidence.referenced_bytes, "retained_durable_bytes": evidence.retained_durable_bytes,
        "reserved_memory_bytes": evidence.reserved_memory_bytes, "owned_memory_bytes": evidence.owned_memory_bytes,
    })
}

fn micros(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).expect("bounded observation duration fits")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(OPERATION_WATCHDOG_SECONDS))
            .expect("bounded watchdog deadline fits"),
    )
}
