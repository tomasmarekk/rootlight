//! Native TypeSpec routing and durable source-owned declarations.
//! Imported types and decorator evaluation remain explicit adapter gaps.

use super::*;
use rootlight_ir::{ContainerRef, OccurrenceRole, OccurrenceTarget};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../../third_party/tree-sitter-typespec-0.0.1/tests/fixtures/namespace.tsp"
);

fn assert_native_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &[u8],
    model_name: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc
        .files
        .iter()
        .find(|file| file.path == "main.tsp")
        .unwrap();
    assert_eq!(file.language, "typespec");
    assert_eq!(file.content_hash, content_hash(source));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "typespec");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.language, "typespec");
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("tree-sitter-typespec-0.0.1")
    );
    let named = |name: &str, kind| {
        doc.entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == name)
            .unwrap()
    };
    let model = named(model_name, EntityKind::Struct);
    let property = named("name", EntityKind::Property);
    let model_namespace = named("TspTest.NamespaceModel", EntityKind::Namespace);
    let client_namespace = named("TspTest.NamespaceClient", EntityKind::Namespace);
    let operation = named("get", EntityKind::Function);
    assert_eq!(property.container, Some(ContainerRef::Entity(model.id)));
    assert_eq!(
        model.container,
        Some(ContainerRef::Entity(model_namespace.id))
    );
    assert_eq!(
        operation.container,
        Some(ContainerRef::Entity(client_namespace.id))
    );
    for entity in [
        model,
        property,
        model_namespace,
        client_namespace,
        operation,
    ] {
        assert_eq!(entity.language, "typespec");
        let definition = doc
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(span.file(), file.id);
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name.as_bytes(),
        );
    }
}

#[test]
fn original_typespec_declarations_survive_durable_transitions() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let input = repository.path().join("main.tsp");
    fs::write(&input, ORIGINAL).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_native_source(&service, &initial, ORIGINAL, "Model");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("model Model", "model Record")
        .into_bytes();
    assert_ne!(changed, ORIGINAL);
    fs::write(&input, &changed).unwrap();
    let update = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(update.generation, initial.generation);
    assert_native_source(&service, &update, &changed, "Record");
    assert_eq!(
        service
            .incremental_evidence(update.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_native_source(&reopened, &update, &changed, "Record");
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        update.generation
    );
    fs::write(&input, ORIGINAL).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_native_source(&reopened, &restored, ORIGINAL, "Model");
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_native_source(&final_open, &restored, ORIGINAL, "Model");
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn typespec_routing_preserves_configuration_and_analyzer_absence() {
    let service = FirstSliceService::new(2).unwrap();
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("typespec").is_some());
    assert!(absent.contains_key("typescript"));
    for path in ["main.tsp", "MAIN.TSP", "main.ts.tsp"] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([3; 20]),
            path: path.to_owned(),
            content_hash: content_hash(ORIGINAL),
            bytes: u64::try_from(ORIGINAL.len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![rootlight_discovery::LanguageSignal {
                language: "python".to_owned(),
                evidence: LanguageEvidence::Content,
            }],
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        assert_eq!(detected_source_language(&input), Some("typespec"));
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("typespec")
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
}
