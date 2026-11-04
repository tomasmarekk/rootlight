//! PHP-tagged generator inputs use the native PHP route without expanding templates.
//! Exact Laravel fixtures are from 8df67f9d176d1d0375a866d8c6780be95ce0336e;
//! their MIT license is retained beside the original bytes in tests/fixtures/laravel-stubs.

use std::{
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{NormalizedIrDocument, OccurrenceRole, OccurrenceTarget};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{FirstSliceIndexReceipt, FirstSliceService, FirstSliceStoragePolicy};

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn durable_test_tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

const DATABASE: &[u8] = include_bytes!("fixtures/laravel-stubs/database.stub");
const CONFIG: &[u8] = include_bytes!("fixtures/laravel-stubs/config.stub");
const FACTORY: &[u8] = include_bytes!("fixtures/laravel-stubs/factory.stub");

fn inspect(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt) -> NormalizedIrDocument {
    service
        .inspect_generation(
            receipt.repository,
            Some(receipt.generation),
            &deadline(),
            |snapshot| snapshot.document().clone(),
        )
        .unwrap()
}

fn assert_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
    language: &str,
) {
    let document = inspect(service, receipt);
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, language, "{path}");
    assert_eq!(file.content_hash, content_hash(bytes));
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.repository(), receipt.repository);
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(source.span().start_byte(), 0);
    assert_eq!(
        source.span().end_byte(),
        u64::try_from(bytes.len()).unwrap()
    );
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &deadline())
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, bytes);
    assert_eq!(read.data.chunks[0].language, language);
}

#[test]
fn original_php_stubs_keep_native_method_spans_and_unexpanded_template_gaps() {
    let root = durable_test_tempdir();
    for (name, bytes) in [
        ("database.stub", DATABASE),
        ("config.stub", CONFIG),
        ("factory.stub", FACTORY),
    ] {
        fs::write(root.path().join(name), bytes).unwrap();
    }
    assert_eq!(DATABASE.len(), 818);
    assert_eq!(CONFIG.len(), 31);
    assert_eq!(FACTORY.len(), 429);
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    for (name, bytes) in [
        ("database.stub", DATABASE),
        ("config.stub", CONFIG),
        ("factory.stub", FACTORY),
    ] {
        assert_source(&service, &receipt, name, bytes, "php");
    }
    let document = inspect(&service, &receipt);
    let doc = document;
    let database = doc
        .files
        .iter()
        .find(|file| file.path == "database.stub")
        .unwrap();
    for name in ["up", "down"] {
        let marker = format!("function {name}");
        let start = DATABASE
            .windows(marker.len())
            .position(|part| part == marker.as_bytes())
            .unwrap()
            + "function ".len();
        let entity = doc
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == database.id)
            })
            .unwrap_or_else(|| panic!("native method definition missing: {name}"));
        assert_eq!(entity.language, "php");
        assert!(doc.occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                && site.source.span().file() == database.id
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(start + name.len()).unwrap()
                && site.source.content_hash() == content_hash(DATABASE)
        }));
    }
    assert!(!doc.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "syntax-error-recovery"
            && diagnostic
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == database.id)
    }));
    let factory = doc
        .files
        .iter()
        .find(|file| file.path == "factory.stub")
        .unwrap();
    assert!(doc.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == factory.id
            && gap.source.content_hash() == content_hash(FACTORY)
            && gap.detail == "syntax-error-recovery"
    }));
    assert!(
        !doc.entities
            .iter()
            .any(|entity| entity.canonical_name.contains("{{"))
    );
}

#[test]
fn non_php_stubs_and_embedded_tag_decoys_do_not_enter_php() {
    let root = durable_test_tempdir();
    for (name, bytes) in [
        (
            "native.stub",
            b"namespace native {\nclass Parser {};\n}\n".as_slice(),
        ),
        ("script.stub", b"export const value = 1;\n".as_slice()),
        ("view.stub", b"<div>{{ $slot }}</div>\n".as_slice()),
        ("comment.stub", b"// <?php\nclass Native {};\n".as_slice()),
        ("quoted.stub", b"\"<?php function decoy() {}\"\n".as_slice()),
        ("boundary.stub", b"<?phpx function decoy() {}\n".as_slice()),
        ("backup.stub.bak", b"<?php function decoy() {}\n".as_slice()),
    ] {
        fs::write(root.path().join(name), bytes).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let document = inspect(&service, &receipt);
    assert_eq!(document.files.len(), 7);
    assert!(document.files.iter().all(|file| file.language != "php"));
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.language == "php")
    );
}

#[test]
fn php_stub_routing_survives_noop_incremental_reopen_and_restore() {
    let root = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let original = b"<?php function before(): int { return 1; }\n";
    let changed = b"<?php function after(): int { return 2; }\n";
    fs::write(root.path().join("program.stub"), original).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service.index_repository(root.path(), &deadline()).unwrap();
    assert_source(&service, &initial, "program.stub", original, "php");
    assert_eq!(
        service
            .index_repository(root.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    fs::write(root.path().join("program.stub"), changed).unwrap();
    let updated = service.index_repository(root.path(), &deadline()).unwrap();
    assert_ne!(updated.generation, initial.generation);
    assert_source(&service, &updated, "program.stub", changed, "php");
    let document = inspect(&service, &updated);
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "after")
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "before")
    );
    drop(document);
    drop(service);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_source(&service, &updated, "program.stub", changed, "php");
    fs::write(root.path().join("program.stub"), original).unwrap();
    let restored = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_source(&service, &restored, "program.stub", original, "php");
}

#[test]
fn explicit_language_configuration_precedes_php_stub_content() {
    let root = durable_test_tempdir();
    fs::write(root.path().join("override.stub"), CONFIG).unwrap();
    let config = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'override.stub'='json'\n";
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(config), 2).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_source(&service, &receipt, "override.stub", CONFIG, "json");
    let document = inspect(&service, &receipt);
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language != "php")
    );
}
