//! Shared RON admission preserves source-owned data and public exact lookup.
//! Durable lifecycle checks distinguish native data facts from source-only hits.

use super::*;
use rootlight_ir::{
    ContainerRef, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
};

const SIMPLE: &[u8] =
    include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/ron/simple_data.ron");
const GRAPH: &[u8] =
    include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/ron/Fox.animgraph.ron");

fn assert_original(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    raw: &[u8],
    expected_properties: usize,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc.files.iter().find(|file| file.path == path).unwrap();
    assert_eq!(file.language, "ron");
    assert_eq!(file.content_hash, content_hash(raw));
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(source.span().start_byte(), 0);
    assert_eq!(source.span().end_byte(), u64::try_from(raw.len()).unwrap());
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &deadline())
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, raw);
    assert_eq!(read.data.chunks[0].language, "ron");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.language, "ron");
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("tree-sitter-ron-0.2.0")
    );
    let module = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Module && e.container == Some(ContainerRef::File(file.id)))
        .unwrap();
    assert_eq!(module.language, "ron");
    let properties: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| {
            e.kind == EntityKind::Property
                && e.evidence
                    .source
                    .as_ref()
                    .is_some_and(|s| s.span().file() == file.id)
        })
        .collect();
    assert_eq!(properties.len(), expected_properties);
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|o| o.role == OccurrenceRole::Definition && o.source.span().file() == file.id)
        .collect();
    assert_eq!(definitions.len(), expected_properties);
    for property in properties {
        assert_eq!(property.language, "ron");
        assert!(definitions.iter().any(|o| o.target
            == OccurrenceTarget::Resolved {
                symbol: property.id
            }));
        let Some(ContainerRef::Entity(owner)) = property.container else {
            panic!("native property retains a source-owned data parent");
        };
        assert!(
            doc.relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(owner)
                    && r.object == RelationEndpoint::Entity(property.id))
        );
    }
    assert!(!doc.entities.iter().any(|e| {
        e.evidence
            .source
            .as_ref()
            .is_some_and(|s| s.span().file() == file.id)
            && matches!(
                e.kind,
                EntityKind::Function | EntityKind::Method | EntityKind::Struct
            )
    }));
    assert!(
        !doc.occurrences
            .iter()
            .any(|o| o.source.span().file() == file.id && o.role == OccurrenceRole::CallSite)
    );
    assert!(
        !doc.diagnostics
            .iter()
            .any(|d| d.code == "unsupported-language")
    );
    if expected_properties == 7 {
        let property = doc
            .entities
            .iter()
            .find(|e| {
                e.kind == EntityKind::Property
                    && e.canonical_name == "\"MyDataAsset\""
                    && e.evidence
                        .source
                        .as_ref()
                        .is_some_and(|s| s.span().file() == file.id)
            })
            .expect("the original key remains a canonical quoted property");
        assert_eq!(property.display_name, "MyDataAsset");
        for name in ["MyDataAsset", "\"MyDataAsset\""] {
            let located = service
                .code_locate(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    25,
                    0,
                    &deadline(),
                )
                .unwrap();
            let symbols: Vec<_> = located
                .data
                .hits
                .iter()
                .filter(|hit| hit.symbol.is_some())
                .collect();
            assert_eq!(symbols.len(), 1, "{name}: {:?}", located.data.hits);
            assert_eq!(symbols[0].path, path);
            assert_eq!(symbols[0].language, "ron");
            assert_eq!(symbols[0].kind, "property");
            assert_eq!(
                symbols[0].symbol,
                Some(property.id),
                "a source-only file hit cannot satisfy exact property lookup"
            );
        }
        let explained = service
            .symbol_explain(receipt.generation, property.id, &deadline())
            .unwrap();
        assert_eq!(explained.data.entity.canonical_name, "\"MyDataAsset\"");
        assert_eq!(explained.data.entity.display_name, "MyDataAsset");
    }
}

#[test]
fn published_original_uses_native_ron_and_plain_and_quoted_exact_symbol_lookup() {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("data.RON"), SIMPLE).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 1);
    assert_original(&service, &receipt, "data.RON", SIMPLE, 7);
    let inventory = service.support_inventory_snapshot().unwrap();
    let capability = inventory
        .languages
        .iter()
        .find(|entry| entry.language == "ron")
        .unwrap();
    assert_eq!(capability.suffixes, [".ron"]);
    assert!(capability.aliases.is_empty());
    assert_eq!(capability.analyzers, ["treesitter"]);
    assert_eq!(capability.maximum_tier, "tier_d");
    assert_eq!(capability.detectors, ["extension"]);
}

fn index_with_work(service: &mut FirstSliceService, path: &Path) -> FirstSliceIndexCommit {
    let prepared = service.prepare_repository(path, &deadline()).unwrap();
    service
        .publish_prepared_with_metrics(prepared, &deadline())
        .unwrap()
}

fn payload_hashes(root: &Path) -> BTreeMap<std::path::PathBuf, ContentHash> {
    let mut pending = vec![root.to_path_buf()];
    let mut result = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                assert!(kind.is_file(), "private test payloads contain no links");
                let bytes = fs::read(entry.path()).unwrap();
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    content_hash(&bytes),
                );
            }
        }
    }
    result
}

fn assert_noop(
    service: &mut FirstSliceService,
    repository: &Path,
    state: &Path,
    generation: GenerationId,
) {
    let id = service
        .registered_repository_for_root(repository, &deadline())
        .unwrap()
        .unwrap();
    let payload = state
        .join("first-slice/repositories")
        .join(id.to_string())
        .join(generation.to_string());
    let before = payload_hashes(&payload);
    let commit = index_with_work(service, repository);
    assert_eq!(commit.receipt().generation, generation);
    assert_eq!(
        commit.evidence().strategy,
        FirstSliceIndexOperationStrategy::RetainedGeneration
    );
    assert_eq!(commit.evidence().rebuilt_files, 0);
    assert_eq!(commit.evidence().rebuilt_facts, 0);
    assert_eq!(commit.evidence().reused_files, 2);
    // Retained publication persists a new activation marker outside the immutable
    // generation. Those real bookkeeping writes do not represent rebuilt facts.
    assert_eq!(
        commit.evidence().newly_written_bytes,
        commit.written_bytes()
    );
    assert_eq!(payload_hashes(&payload), before);
    println!(
        "RON no-op: rebuilt_files=0 rebuilt_facts=0 reused_files=2 activation_bytes={}",
        commit.written_bytes()
    );
}

#[test]
fn shared_ron_routes_preserve_configuration_and_refuse_inner_suffixes() {
    let service = FirstSliceService::new(2).unwrap();
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("ron").is_some());
    assert!(absent.contains_key("rust"));
    for path in ["data.ron", "DATA.RON", "data.rs.ron"] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([3; 20]),
            path: path.to_owned(),
            content_hash: content_hash(SIMPLE),
            bytes: u64::try_from(SIMPLE.len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![rootlight_discovery::LanguageSignal {
                language: "rust".to_owned(),
                evidence: LanguageEvidence::Content,
            }],
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        assert_eq!(source_language_from_path(path), Some("ron"));
        assert_eq!(detected_source_language(&input), Some("ron"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("ron")
        );
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("ron")
        );
        assert_eq!(source_language_route(&input, &absent), None);
        input
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "json".to_owned(),
                evidence: LanguageEvidence::Configuration,
            });
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("json")
        );
        input.language_signals.last_mut().unwrap().language = "unsupported-language".to_owned();
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for path in [
        "data.ron.bak",
        "data.ronx",
        "data.ron/source",
        "data.ron.tt",
    ] {
        assert_eq!(source_language_from_path(path), None);
    }
    assert_eq!(source_language_from_path("data.ron.json"), Some("json"));
    assert_eq!(source_language_from_path("data.ron.rs"), Some("rust"));
    assert_eq!(source_language_from_path("data.ron.erb"), Some("erb"));
}

#[test]
fn original_ron_sources_survive_noop_edit_byte_restore_and_reopen() {
    let fixture = durable_test_tempdir();
    fs::write(fixture.path().join("data.RON"), SIMPLE).unwrap();
    fs::write(fixture.path().join("graph.ron"), GRAPH).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let initial = index_with_work(&mut service, fixture.path());
    let initial_receipt = initial.receipt();
    assert_eq!(initial_receipt.indexed_files, 2);
    assert_eq!(initial.evidence().rebuilt_files, 2);
    assert_eq!(initial.evidence().reused_files, 0);
    assert_original(&service, initial_receipt, "data.RON", SIMPLE, 7);
    assert_original(&service, initial_receipt, "graph.ron", GRAPH, 22);
    let symbols: BTreeSet<_> = service
        .loaded_generation_snapshot(initial_receipt.generation)
        .unwrap()
        .document()
        .entities
        .iter()
        .map(|e| e.id)
        .collect();
    assert_noop(
        &mut service,
        fixture.path(),
        paths.state_dir(),
        initial_receipt.generation,
    );
    let changed = std::str::from_utf8(SIMPLE)
        .unwrap()
        .replace("Hello World!", "Hello Night!")
        .into_bytes();
    assert_ne!(changed, SIMPLE);
    fs::write(fixture.path().join("data.RON"), &changed).unwrap();
    let updated = index_with_work(&mut service, fixture.path());
    assert_ne!(updated.receipt().generation, initial_receipt.generation);
    let work = service
        .incremental_evidence(updated.receipt().generation)
        .unwrap();
    assert_eq!(work.parsed_files(), 1);
    assert_eq!(work.reused_parser_artifacts(), 1);
    assert_eq!(updated.evidence().rebuilt_files, 1);
    assert_eq!(updated.evidence().reused_files, 1);
    assert_original(&service, updated.receipt(), "data.RON", &changed, 7);
    assert_original(&service, updated.receipt(), "graph.ron", GRAPH, 22);
    assert_fresh_equivalent(
        &service,
        fixture.path(),
        initial_receipt.generation,
        updated.receipt(),
        &deadline(),
    );
    assert_eq!(
        service
            .loaded_generation_snapshot(updated.receipt().generation)
            .unwrap()
            .document()
            .entities
            .iter()
            .map(|e| e.id)
            .collect::<BTreeSet<_>>(),
        symbols
    );
    drop(service);
    let mut reopened = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert_original(&reopened, updated.receipt(), "data.RON", &changed, 7);
    assert_original(&reopened, updated.receipt(), "graph.ron", GRAPH, 22);
    assert_noop(
        &mut reopened,
        fixture.path(),
        paths.state_dir(),
        updated.receipt().generation,
    );
    fs::write(fixture.path().join("data.RON"), SIMPLE).unwrap();
    let restored = index_with_work(&mut reopened, fixture.path());
    assert_ne!(restored.receipt().generation, updated.receipt().generation);
    assert_eq!(restored.evidence().rebuilt_files, 2);
    assert_eq!(restored.evidence().reused_files, 0);
    assert_eq!(
        reopened
            .incremental_evidence(restored.receipt().generation)
            .unwrap()
            .parsed_files(),
        2
    );
    assert_eq!(
        restored.receipt().logical_snapshot,
        initial_receipt.logical_snapshot
    );
    assert_original(&reopened, restored.receipt(), "data.RON", SIMPLE, 7);
    assert_original(&reopened, restored.receipt(), "graph.ron", GRAPH, 22);
    assert_eq!(
        reopened
            .loaded_generation_snapshot(restored.receipt().generation)
            .unwrap()
            .document()
            .entities
            .iter()
            .map(|e| e.id)
            .collect::<BTreeSet<_>>(),
        symbols
    );
    drop(reopened);
    let mut final_open = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert_original(&final_open, restored.receipt(), "data.RON", SIMPLE, 7);
    assert_original(&final_open, restored.receipt(), "graph.ron", GRAPH, 22);
    assert_noop(
        &mut final_open,
        fixture.path(),
        paths.state_dir(),
        restored.receipt().generation,
    );
}

#[test]
#[ignore = "requires the explicitly prepared actual retained predecessor product state"]
fn actual_predecessor_unknown_ron_state_requalifies_before_noop_and_reopen() {
    let repository = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_RON_PREDECESSOR_REPOSITORY")
            .expect("actual predecessor repository is required"),
    );
    let outer = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_RON_PREDECESSOR_STATE")
            .expect("actual predecessor outer state is required"),
    );
    let report_path = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_RON_PREDECESSOR_REPORT")
            .expect("actual predecessor stdout is required"),
    );
    let report_bytes = fs::read(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(
        report["schema"],
        "rootlight-index-source-accounting-diagnostic/1"
    );
    assert_eq!(report["no_op_same_generation"], true);
    assert_eq!(
        report["source_accounting"]["source_read_verified_file_records"],
        2
    );
    assert_eq!(report["source_accounting"]["retained_entity_records"], 0);
    assert_eq!(report["source_accounting"]["retained_relation_records"], 0);
    let prior: FirstSliceIndexReceipt = serde_json::from_value(report["cold"].clone()).unwrap();
    assert_eq!(prior.indexed_files, 2);
    assert_eq!(prior.entities, 0);
    for (path, raw) in [("data.RON", SIMPLE), ("graph.ron", GRAPH)] {
        assert_eq!(fs::read(repository.join(path)).unwrap(), raw);
        let row = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["file"]["path"] == path)
            .unwrap();
        assert_eq!(row["file"]["language"], "unknown");
        assert_eq!(row["file"]["content_hash"], content_hash(raw).to_string());
        assert_eq!(row["source_exact"], true);
        assert_eq!(row["source_read_exact"], true);
        assert_eq!(row["occurrence_record_count"], 0);
    }
    let state = outer.join("state");
    let repository_state = state
        .join("first-slice/repositories")
        .join(prior.repository.to_string());
    let old_generation = repository_state.join(prior.generation.to_string());
    let old_payloads = payload_hashes(&old_generation);
    let blobs = repository_state.join("source-blobs");
    let old_blobs = payload_hashes(&blobs);
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, &state, &deadline()).unwrap();
    assert_eq!(
        service
            .registered_repository_for_root(&repository, &deadline())
            .unwrap(),
        Some(prior.repository)
    );
    let old = service
        .loaded_generation_snapshot(prior.generation)
        .unwrap();
    assert!(old.document().entities.is_empty());
    assert!(old.document().occurrences.is_empty());
    assert!(old.document().relations.is_empty());
    let old_provider = old.metadata().provider_set_hash();
    let old_configuration = old.metadata().configuration_hash();
    for (path, raw) in [("data.RON", SIMPLE), ("graph.ron", GRAPH)] {
        let row = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["file"]["path"] == path)
            .unwrap();
        let file: FileId = row["file"]["id"].as_str().unwrap().parse().unwrap();
        let record = old.find_file(file).unwrap();
        assert_eq!(record.language, "unknown");
        let reference = record.evidence.source.as_ref().unwrap().clone();
        let read = service
            .source_read(prior.generation, vec![reference], &deadline())
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, raw);
        assert_eq!(read.data.chunks[0].language, "unknown");
    }
    drop(old);
    let upgraded = index_with_work(&mut service, &repository);
    assert_eq!(upgraded.receipt().repository, prior.repository);
    assert_eq!(upgraded.receipt().parent, Some(prior.generation));
    assert_ne!(upgraded.receipt().generation, prior.generation);
    assert_eq!(upgraded.evidence().rebuilt_files, 2);
    assert_eq!(upgraded.evidence().reused_files, 0);
    let current = service
        .loaded_generation_snapshot(upgraded.receipt().generation)
        .unwrap();
    assert_eq!(current.metadata().configuration_hash(), old_configuration);
    assert_ne!(current.metadata().provider_set_hash(), old_provider);
    drop(current);
    assert_original(&service, upgraded.receipt(), "data.RON", SIMPLE, 7);
    assert_original(&service, upgraded.receipt(), "graph.ron", GRAPH, 22);
    assert_noop(
        &mut service,
        &repository,
        &state,
        upgraded.receipt().generation,
    );
    drop(service);
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, &state, &deadline()).unwrap();
    assert_original(&reopened, upgraded.receipt(), "data.RON", SIMPLE, 7);
    assert_original(&reopened, upgraded.receipt(), "graph.ron", GRAPH, 22);
    assert_noop(
        &mut reopened,
        &repository,
        &state,
        upgraded.receipt().generation,
    );
    assert_eq!(payload_hashes(&old_generation), old_payloads);
    let remaining_blobs = payload_hashes(&blobs);
    for (path, hash) in old_blobs {
        assert_eq!(remaining_blobs.get(&path), Some(&hash));
    }
    assert_eq!(fs::read(report_path).unwrap(), report_bytes);
    for (path, raw) in [("data.RON", SIMPLE), ("graph.ron", GRAPH)] {
        assert_eq!(fs::read(repository.join(path)).unwrap(), raw);
    }
    println!(
        "actual predecessor requalification: old_unknown_files=2 current_ron_files=2 parsed_files=2 reused_parser_artifacts=0 properties=29; no-op/reopen preserved current native symbols and original generation payloads"
    );
}
