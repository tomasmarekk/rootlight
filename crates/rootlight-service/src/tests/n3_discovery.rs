//! N3 uses native source-owned declarations through durable product transitions.
//! Formula logic, quick-variable binding and RDF references retain explicit gaps.

use super::*;
use rootlight_ir::{OccurrenceRole, OccurrenceTarget};

const ORIGINAL: &[u8] = b"@prefix var: <urn:variables:> .\r\n{ @forAll var:x . var:x var:p true . } => { @forSome var:y . var:y var:q true . } .\r\n";

fn assert_native_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &[u8],
    name: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc
        .files
        .iter()
        .find(|file| file.path == "rules.n3")
        .unwrap();
    assert_eq!(file.language, "n3");
    assert_eq!(file.content_hash, content_hash(source));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "n3");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("tree-sitter-n3-0.1.0")
    );
    assert_eq!(provenance.language, "n3");
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Namespace && entity.canonical_name == "var")
    );
    let variables: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .collect();
    assert_eq!(variables.len(), 2);
    assert!(variables.iter().any(|entity| entity.canonical_name == name));
    assert_ne!(variables[0].id, variables[1].id);
    for entity in variables {
        let occurrence = doc.occurrences.iter().find(|occurrence| occurrence.role==OccurrenceRole::Definition && matches!(occurrence.target,OccurrenceTarget::Resolved{symbol} if symbol==entity.id)).unwrap();
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name.as_bytes()
        );
    }
    assert!(doc.skipped_regions.iter().any(|gap| gap.detail
        == "n3-formula-rule-triple-quick-variable-and-resource-reference-semantics-unavailable"));
}

#[test]
fn n3_native_declarations_and_exact_reads_survive_durable_transitions() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let input = repository.path().join("rules.n3");
    fs::write(&input, ORIGINAL).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_native_source(&service, &initial, ORIGINAL, "var:x");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("var:x", "var:changed")
        .into_bytes();
    fs::write(&input, &changed).unwrap();
    let update = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(update.generation, initial.generation);
    assert_native_source(&service, &update, &changed, "var:changed");
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
    assert_native_source(&reopened, &update, &changed, "var:changed");
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
    assert_native_source(&reopened, &restored, ORIGINAL, "var:x");
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_native_source(&final_open, &restored, ORIGINAL, "var:x");
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn n3_routing_honors_configuration_and_missing_native_analyzer() {
    let service = FirstSliceService::new(2).unwrap();
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("n3").is_some());
    assert!(absent.contains_key("turtle"));
    for path in ["rules.n3", "RULES.N3", "rules.ttl.n3"] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([3; 20]),
            path: path.to_owned(),
            content_hash: content_hash(ORIGINAL),
            bytes: u64::try_from(ORIGINAL.len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![],
            project_language_witnesses: vec![],
            decisive_rule: None,
        };
        assert_eq!(detected_source_language(&input), Some("n3"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("n3")
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
