//! Native SPARQL aliases survive durable transitions and shared .ru routing.
//! Explicit configuration and genuine Rack sources retain independent language authority.

use super::*;
use rootlight_ir::{OccurrenceRole, OccurrenceTarget};

const ORIGINAL: &[u8] = b"PREFIX ex: <urn:example:>\r\nDELETE { ex:s ex:p ?result } INSERT { ex:s ex:p ?result } WHERE { BIND(1 AS ?result) }\r\n";

fn assert_native(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &[u8],
    name: &str,
    sigil: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc
        .files
        .iter()
        .find(|file| file.path == "update.ru")
        .unwrap();
    assert_eq!(file.language, "sparql");
    assert_eq!(file.content_hash, content_hash(source));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "sparql");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("tree-sitter-sparql-0.1.0")
    );
    assert_eq!(provenance.language, "sparql");
    let text = std::str::from_utf8(source).unwrap();
    let written = format!("{sigil}{name}");
    let marker = format!("AS {written}");
    let start = u64::try_from(text.find(&marker).unwrap() + 3).unwrap();
    let end = start + u64::try_from(written.len()).unwrap();
    let occurrence = doc
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().file() == file.id
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .unwrap();
    assert_eq!(occurrence.source.generation(), receipt.generation);
    assert_eq!(occurrence.source.content_hash(), file.content_hash);
    let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
        panic!("exact written alias target")
    };
    let entity = doc
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(
        (
            entity.kind,
            entity.canonical_name.as_str(),
            entity.language.as_str()
        ),
        (EntityKind::Variable, name, "sparql")
    );
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Namespace
                && entity.language == "sparql"
                && entity.canonical_name == "ex")
    );
    let ruby = doc
        .files
        .iter()
        .find(|file| file.path == "config.ru")
        .unwrap();
    assert_eq!(ruby.language, "ruby");
}

#[test]
fn sparql_update_aliases_and_rack_sources_survive_durable_transitions() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let input = repository.path().join("update.ru");
    fs::write(&input, ORIGINAL).unwrap();
    fs::write(
        repository.path().join("config.ru"),
        b"run Rails.application\n",
    )
    .unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_native(&service, &initial, ORIGINAL, "result", "?");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("?result", "$updated")
        .into_bytes();
    fs::write(&input, &changed).unwrap();
    let update = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(update.generation, initial.generation);
    assert_native(&service, &update, &changed, "updated", "$");
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
    assert_native(&reopened, &update, &changed, "updated", "$");
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
    assert_native(&reopened, &restored, ORIGINAL, "result", "?");
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_native(&final_open, &restored, ORIGINAL, "result", "?");
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn sparql_shared_suffix_honors_configuration_conflicts_and_missing_native_analyzer() {
    let service = FirstSliceService::new(2).unwrap();
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("sparql").is_some());
    assert!(absent.contains_key("ruby"));
    let mut input = ManifestInput {
        file: FileId::from_bytes([3; 20]),
        path: "update.RU".to_owned(),
        content_hash: content_hash(ORIGINAL),
        bytes: u64::try_from(ORIGINAL.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![
            rootlight_discovery::LanguageSignal {
                language: "ruby".to_owned(),
                evidence: LanguageEvidence::Extension,
            },
            rootlight_discovery::LanguageSignal {
                language: "sparql".to_owned(),
                evidence: LanguageEvidence::Content,
            },
        ],
        project_language_witnesses: vec![],
        decisive_rule: None,
    };
    assert_eq!(detected_source_language(&input), Some("sparql"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("sparql")
    );
    assert_eq!(source_language_route(&input, &absent), None);
    input
        .language_signals
        .push(rootlight_discovery::LanguageSignal {
            language: "ruby".to_owned(),
            evidence: LanguageEvidence::Shebang,
        });
    assert_eq!(detected_source_language(&input), None);
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    input
        .language_signals
        .push(rootlight_discovery::LanguageSignal {
            language: "json".to_owned(),
            evidence: LanguageEvidence::Configuration,
        });
    assert_eq!(detected_source_language(&input), Some("json"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("json")
    );
}

#[test]
#[ignore = "requires explicitly supplied original RDFLib and Rails corpus roots"]
fn sparql_original_update_and_rack_files_keep_their_native_source_routes() {
    let root = PathBuf::from(
        std::env::var_os("ROOTLIGHT_DIAGNOSTIC_CORPUS_ROOT")
            .expect("explicit original corpus parent"),
    );
    let update = fs::read(
        root.join("183_N3_rdflib/test/data/suites/w3c/sparql11/basic-update/insert-data-spo1.ru"),
    )
    .unwrap();
    let rails = fs::read(root.join("17_Ruby_rails/activestorage/test/dummy/config.ru")).unwrap();
    let erb = fs::read(root.join("177_ERB_rails/actiontext/test/dummy/config.ru")).unwrap();
    assert_eq!(update.len(), 60);
    assert_eq!(rails.len(), 166);
    assert_eq!(rails, erb);
    let fixture = durable_test_tempdir();
    for (path, bytes) in [
        ("update.ru", &update),
        ("config.ru", &rails),
        ("erb/config.ru", &erb),
    ] {
        let path = fixture.path().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    for (path, language, bytes) in [
        ("update.ru", "sparql", &update),
        ("config.ru", "ruby", &rails),
        ("erb/config.ru", "ruby", &erb),
    ] {
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language);
        let read = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(&read.data.chunks[0].bytes, bytes);
        assert_eq!(read.data.chunks[0].language, language);
    }
}
