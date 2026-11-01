//! TriG preserves native graph ownership through durable product transitions.
//! Resource labels and unproved RDF assertions do not become authored bindings.

use super::*;
use rootlight_ir::{OccurrenceRole, OccurrenceTarget};

const SOURCE: &[u8] = b"@prefix ex: <https://example.test/> .\r\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\r\ngraph ex:g { ex:s ex:p \"\"\"first\n#\0last\"\"\" . # comment\0\r\nex:Item a rdfs:Class }\r\n";

#[test]
fn turtle_native_nul_literal_keeps_following_written_class_and_exact_source() {
    let repository = durable_test_tempdir();
    let source = b"@prefix ex: <https://example.test/> .\n@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\nex:s ex:p '''first\n#\0last''' .\nex:AfterNul a rdfs:Class .\n";
    fs::write(repository.path().join("data.ttl"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "data.ttl")
        .unwrap();
    assert_eq!(file.language, "turtle");
    assert_eq!(file.content_hash, content_hash(source));
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "ex:AfterNul")
        .unwrap();
    assert_eq!(class.qualified_name, "https://example.test/AfterNul");
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "turtle");
}

fn assert_trig(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &[u8],
    class_name: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc
        .files
        .iter()
        .find(|file| file.path == "data.trig")
        .unwrap();
    assert_eq!(file.language, "trig");
    assert_eq!(file.content_hash, content_hash(source));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "trig");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.language, "trig");
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("tree-sitter-trig-0.2.0")
    );
    let graph = doc
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "ex:g")
        .unwrap();
    let class = doc
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == class_name)
        .unwrap();
    assert_eq!(
        class.container,
        Some(rootlight_ir::ContainerRef::Entity(graph.id))
    );
    assert!(doc.relations.iter().any(|relation| relation.predicate
        == rootlight_ir::RelationPredicate::Contains
        && relation.subject == rootlight_ir::RelationEndpoint::Entity(graph.id)
        && relation.object == rootlight_ir::RelationEndpoint::Entity(class.id)));
    assert!(!doc.occurrences.iter().any(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == graph.id)));
    let definition = doc.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == class.id)).unwrap();
    let span = definition.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        class_name.as_bytes()
    );
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("trig-rdf-assertions-"))
    );
}

#[test]
fn trig_native_graph_and_typed_resource_survive_fresh_noop_incremental_and_restored() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let input = repository.path().join("data.trig");
    fs::write(&input, SOURCE).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_trig(&service, &initial, SOURCE, "ex:Item");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(SOURCE)
        .unwrap()
        .replace("ex:Item", "ex:Changed")
        .into_bytes();
    fs::write(&input, &changed).unwrap();
    let updated = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, initial.generation);
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_trig(&service, &updated, &changed, "ex:Changed");
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_trig(&reopened, &updated, &changed, "ex:Changed");
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        updated.generation
    );
    fs::write(&input, SOURCE).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_trig(&reopened, &restored, SOURCE, "ex:Item");
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_trig(&final_open, &restored, SOURCE, "ex:Item");
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn trig_route_keeps_configuration_and_missing_analyzer_authority_separate_from_turtle() {
    let service = FirstSliceService::new(2).unwrap();
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("trig").is_some());
    assert!(absent.contains_key("turtle"));
    for path in ["data.trig", "DATA.TRIG", "data.ttl.trig"] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([3; 20]),
            path: path.to_owned(),
            content_hash: content_hash(SOURCE),
            bytes: u64::try_from(SOURCE.len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![],
            project_language_witnesses: vec![],
            decisive_rule: None,
        };
        assert_eq!(detected_source_language(&input), Some("trig"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("trig")
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
    }
}
