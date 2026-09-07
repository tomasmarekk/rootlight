//! Release-only latency observations over deterministic single-repository workloads.
//!
//! Correctness and work counters accompany raw timings; filesystem caches are
//! not flushed, so an empty Rootlight state is not an OS-cold measurement.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash};
use rootlight_ir::{SourceRef, SourceSpan};
use rootlight_query::LocateMode;
use rootlight_runtime::RuntimePaths;
use rootlight_service::{
    FirstSliceBudget, FirstSliceIndexCommit, FirstSliceIndexMode, FirstSliceIndexOperationStrategy,
    FirstSliceIndexProgress, FirstSliceService, SourceReadOptions,
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

#[test]
fn full_source_latency_shapes_preserve_one_byte_body_edits() {
    for workload in [Workload::Rust, Workload::Json] {
        for words in [512, 8_192] {
            let before = full_source(workload, 0, 1, words);
            let after = full_source(workload, 0, 2, words);
            assert_eq!(before.len(), after.len());
            assert_eq!(
                before
                    .bytes()
                    .zip(after.bytes())
                    .filter(|(a, b)| a != b)
                    .count(),
                1
            );
            assert_eq!(before.len() > 32 * 1024, words == 8_192);
            assert_eq!(
                before
                    .split_whitespace()
                    .filter(|word| word.starts_with("word"))
                    .count(),
                words
            );
        }
    }
}

#[test]
#[ignore = "runs release-only full-source size and vocabulary observations"]
fn durable_full_source_latency_observations() -> Result<(), &'static str> {
    if cfg!(debug_assertions) {
        return Err("latency observations require --release");
    }
    for sample in 0..=SAMPLES {
        for files in FILE_COUNTS {
            for workload in [Workload::Rust, Workload::Json] {
                // Alternate size order across samples to expose order-sensitive cache effects.
                let sizes = if sample % 2 == 0 {
                    [512, 8_192]
                } else {
                    [8_192, 512]
                };
                for words in sizes {
                    let observation = observe_full_source(workload, files, words, sample);
                    println!(
                        "FULL_SOURCE_LATENCY {}",
                        serde_json::to_string(&observation).expect("observation serializes")
                    );
                }
            }
        }
    }
    Ok(())
}

fn full_source(workload: Workload, ordinal: usize, body: u32, words: usize) -> String {
    let vocabulary = (0..words)
        .map(|word| format!("word{word:05}"))
        .collect::<Vec<_>>()
        .join(" ");
    match workload {
        Workload::Rust => format!(
            "{}// headmarker {vocabulary} tailmarker\n",
            workload.source(ordinal, body)
        ),
        Workload::Json => format!(
            "{{\"marker\":\"value_{ordinal}\",\"value\":{body},\"vocabulary\":\"headmarker {vocabulary} tailmarker\"}}\n"
        ),
        _ => panic!("full-source observations only declare Rust and JSON shapes"),
    }
}

fn observe_full_source(workload: Workload, files: usize, words: usize, sample: usize) -> Value {
    let fixture = TempDir::new().expect("private fixture root");
    let root = fixture.path().join("repository");
    fs::create_dir(&root).expect("repository directory");
    let mut hasher = blake3::Hasher::new();
    let mut source_bytes = 0usize;
    for file in 0..files {
        let path = workload.path(file);
        let source = full_source(workload, file, 1, words);
        hasher.update(path.as_bytes());
        hasher.update(&[0]);
        hasher.update(source.as_bytes());
        hasher.update(&[0]);
        source_bytes += source.len();
        fs::write(root.join(path), source).expect("full-source fixture writes");
    }
    let paths = RuntimePaths::new(fixture.path().join("state"), fixture.path().join("runtime"))
        .expect("runtime paths");
    paths.prepare_owner().expect("private runtime");
    let opened = Instant::now();
    let mut service =
        FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).expect("durable service");
    let open_micros = micros(opened);
    let (initial, initial_timing, initial_checkpoints) = profiled_index(&mut service, &root);
    let (noop, noop_timing, noop_checkpoints) = profiled_index(&mut service, &root);
    assert_eq!(noop.receipt(), initial.receipt());
    assert_eq!(
        noop.evidence().strategy,
        FirstSliceIndexOperationStrategy::RetainedGeneration
    );
    assert_eq!(noop.evidence().changed_files, 0);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(
        root.join(workload.path(0)),
        full_source(workload, 0, 2, words),
    )
    .expect("one-byte edit");
    let (changed, changed_timing, changed_checkpoints) = profiled_index(&mut service, &root);
    assert_eq!(changed.receipt().parent, Some(initial.receipt().generation));
    assert_ne!(changed.receipt().generation, initial.receipt().generation);
    assert_eq!(changed.evidence().changed_files, 1);
    if matches!(workload, Workload::Rust) {
        assert_eq!(changed.evidence().rebuilt_files, 1);
        assert_eq!(
            changed.evidence().reused_files,
            u64::try_from(files - 1).expect("file count")
        );
    }
    assert_eq!(
        changed.evidence().rebuilt_facts + changed.evidence().reused_facts,
        initial.evidence().rebuilt_facts
    );
    let verification_started = Instant::now();
    for (commit, changed_body) in [(&initial, false), (&changed, true)] {
        assert!(commit.receipt().discovery_complete);
        assert_eq!(commit.receipt().excluded_inputs, 0);
        assert_eq!(
            commit.receipt().discovered_inputs,
            u64::try_from(files).expect("file count")
        );
        assert_eq!(
            commit.receipt().indexed_files,
            u64::try_from(files).expect("file count")
        );
        verify_full_sources(
            &service,
            commit.receipt().generation,
            workload,
            files,
            words,
            changed_body,
        );
    }
    let verification_micros = micros(verification_started);
    let status = service
        .repository_status(changed.receipt().repository, None)
        .expect("language status");
    assert_eq!(status.coverage.len(), 1);
    assert_eq!(status.coverage[0].language, workload.language());
    assert_eq!(
        status.coverage[0].indexed_files,
        if matches!(workload, Workload::Json) {
            0
        } else {
            u64::try_from(files).expect("file count")
        }
    );
    json!({
        "schema":"rootlight.full-source-latency/1", "workload":workload.label(),
        "files":files,"vocabulary_words_per_file":words,"source_bytes":source_bytes,
        "fixture_blake3":hasher.finalize().to_hex().to_string(),
        "sample":sample,"phase":if sample == 0 {"warmup"} else {"measured"},
        "profile":"release","storage":"durable","initial_state":"empty","os_cache":"not_flushed",
        "service_open_micros":open_micros,"verification_micros":verification_micros,
        "operation_watchdog_seconds":OPERATION_WATCHDOG_SECONDS,
        "initial":stage(&initial,initial_timing),"noop":stage(&noop,noop_timing),"body_edit":stage(&changed,changed_timing),
        "preparation_checkpoints":{"initial":initial_checkpoints,"noop":noop_checkpoints,"body_edit":changed_checkpoints},
        "coverage":{"language":status.coverage[0].language,"tier":status.coverage[0].tier,"status":status.coverage[0].status,
            "discovered_files":status.coverage[0].discovered_files,"indexed_files":status.coverage[0].indexed_files},
        "all_files_global_head_and_tail_located":true,"all_files_lexical_omissions_zero":true,
        "all_files_exact_tail_reads":true,"complete_file_reads_per_generation":2,"verified_generations":2,
        "installed_mcp_measurement":false,"full_language_support_claimed":false,"before_after_binary_comparison":false
    })
}

fn verify_full_sources(
    service: &FirstSliceService,
    generation: GenerationId,
    workload: Workload,
    files: usize,
    words: usize,
    changed: bool,
) {
    let mut hits = BTreeMap::new();
    for offset in (0..files).step_by(32) {
        let located = service
            .code_locate(
                generation,
                "headmarker tailmarker".to_owned(),
                LocateMode::Text,
                32,
                offset,
                &deadline(),
            )
            .expect("cross-source vocabulary query");
        assert_eq!(located.data.hits.len(), (files - offset).min(32));
        for hit in located.data.hits {
            assert!(hit.symbol.is_none());
            assert!(
                hits.insert(hit.path, hit.source.expect("file reference"))
                    .is_none()
            );
        }
    }
    assert_eq!(hits.len(), files);
    for file in 0..files {
        let reference = hits
            .remove(&workload.path(file))
            .expect("every expected source located");
        let source = full_source(
            workload,
            file,
            if changed && file == 0 { 2 } else { 1 },
            words,
        );
        assert_eq!(reference.generation(), generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let coverage = service
            .source_file_lexical_coverage_until(generation, reference.span().file(), &deadline())
            .expect("lexical accounting")
            .expect("whole-input accounting");
        assert_eq!(
            coverage.source_bytes,
            u64::try_from(source.len()).expect("source length")
        );
        assert!(coverage.is_complete());
        assert_eq!(coverage.omitted_word_bytes, 0);
        let start = source.find("tailmarker").expect("tail marker");
        assert_eq!(
            read_range(service, &reference, start, start + "tailmarker".len()),
            b"tailmarker"
        );
        if file == 0 || file == files - 1 {
            let mut exact = Vec::new();
            for start in (0..source.len()).step_by(16 * 1024) {
                exact.extend(read_range(
                    service,
                    &reference,
                    start,
                    (start + 16 * 1024).min(source.len()),
                ));
            }
            assert_eq!(exact, source.as_bytes());
        }
    }
}

fn read_range(
    service: &FirstSliceService,
    reference: &SourceRef,
    start: usize,
    end: usize,
) -> Vec<u8> {
    let span = SourceSpan::new(
        reference.span().file(),
        u64::try_from(start).expect("start offset"),
        u64::try_from(end).expect("end offset"),
    )
    .expect("source span");
    let part = SourceRef::new(
        reference.repository(),
        reference.generation(),
        span,
        reference.content_hash(),
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![part],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .expect("bounded exact source read");
    assert_eq!(read.data.chunks.len(), 1);
    read.data
        .chunks
        .into_iter()
        .next()
        .expect("one source chunk")
        .bytes
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

fn profiled_index(
    service: &mut FirstSliceService,
    root: &Path,
) -> (FirstSliceIndexCommit, StageTiming, Value) {
    let cancellation = deadline();
    let mut checkpoints = Vec::with_capacity(16);
    let started = Instant::now();
    let prepared = service
        .prepare_repository_with_mode_and_progress(
            root,
            FirstSliceIndexMode::Structural,
            &cancellation,
            |progress| checkpoints.push((progress, micros(started))),
        )
        .expect("repository prepares");
    let preparation_micros = micros(started);
    let commit = service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .expect("generation publishes");
    let wall_micros = micros(started);
    // Serialize after timing; callback boundaries can repeat or mix work on fallback paths.
    let profile = checkpoint_profile(checkpoints, preparation_micros);
    (
        commit,
        StageTiming {
            wall_micros,
            preparation_micros,
            publication_micros: wall_micros - preparation_micros,
        },
        profile,
    )
}

fn checkpoint_profile(checkpoints: Vec<(FirstSliceIndexProgress, u64)>, total: u64) -> Value {
    let mut previous = 0;
    let checkpoints = checkpoints
        .into_iter()
        .map(|(progress, elapsed)| {
            assert!(elapsed >= previous && elapsed <= total);
            let interval = elapsed - previous;
            previous = elapsed;
            json!({
                "stage":format!("{:?}", progress.stage),
                "completed":progress.completed,"total":progress.total,
                "files_examined":progress.files_examined,"bytes_examined":progress.bytes_examined,
                "written_bytes":progress.written_bytes,
                "elapsed_micros":elapsed,"since_previous_micros":interval,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schema":"rootlight.preparation-checkpoints/1", "checkpoints":checkpoints,
        "after_last_checkpoint_micros":total - previous,
        "intervals_are_exclusive_work_categories":false,
    })
}

#[test]
fn checkpoint_intervals_conserve_time_without_collapsing_repeated_stages() {
    let progress = FirstSliceIndexProgress {
        stage: rootlight_service::FirstSliceIndexStage::Persistence,
        completed: 4,
        total: 6,
        files_examined: 1,
        bytes_examined: 32,
        written_bytes: 8,
    };
    let profile = checkpoint_profile(vec![(progress, 10), (progress, 25)], 30);
    let points = profile["checkpoints"].as_array().expect("checkpoint array");
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["since_previous_micros"], 10);
    assert_eq!(points[1]["since_previous_micros"], 15);
    assert_eq!(points[1]["elapsed_micros"], 25);
    assert_eq!(profile["after_last_checkpoint_micros"], 5);
    assert_eq!(
        checkpoint_profile(Vec::new(), 30)["after_last_checkpoint_micros"],
        30
    );
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
