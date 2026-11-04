//! Conventional configuration names select native JSONC through the public service.
//! Licensed originals pin physical property coordinates; strict JSON and explicit
//! user configuration retain their own routing contracts across durable generations.

use std::{
    fs,
    time::{Duration, Instant},
};

use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{
    EntityKind, NormalizedIrDocument, OccurrenceRole, OccurrenceTarget, RelationEndpoint,
    RelationPredicate,
};
use rootlight_runtime::RuntimePaths;
use rootlight_service::{FirstSliceIndexReceipt, FirstSliceService, FirstSliceStoragePolicy};

const TSCONFIG: &[u8] = include_bytes!("fixtures/jsonc-configs/react-tsconfig.json");
const DEVCONTAINER: &[u8] = include_bytes!("fixtures/jsonc-configs/java-devcontainer.json");

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

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
    assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.repository(), receipt.repository);
    assert_eq!(source.generation(), receipt.generation);
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &deadline())
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, bytes);
    assert_eq!(read.data.chunks[0].language, language);
}

fn has_recovery(document: &NormalizedIrDocument, path: &str) -> bool {
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    document
        .skipped_regions
        .iter()
        .any(|gap| gap.source.span().file() == file.id && gap.detail == "syntax-error-recovery")
}

fn assert_property_contains(
    document: &NormalizedIrDocument,
    path: &str,
    bytes: &[u8],
    parent: &str,
    child: &str,
) {
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    let property = |name: &str| {
        document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Property
                    && entity.display_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .min_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte())
            .unwrap()
    };
    let parent = property(parent);
    let child = property(child);
    for entity in [parent, child] {
        assert_eq!(entity.language, "jsonc");
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let start = usize::try_from(definition.source.span().start_byte()).unwrap();
        let end = usize::try_from(definition.source.span().end_byte()).unwrap();
        assert_eq!(
            &bytes[start..end],
            format!("\"{}\"", entity.display_name).as_bytes()
        );
        assert_eq!(definition.source.content_hash(), content_hash(bytes));
        assert_eq!(
            definition.syntactic_text_hash,
            content_hash(&bytes[start..end])
        );
    }
    assert!(
        document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent.id)
                && relation.object == RelationEndpoint::Entity(child.id)
        }),
        "missing authored containment in {path}: {} -> {}",
        parent.display_name,
        child.display_name,
    );
}

#[test]
fn original_commented_configurations_keep_native_properties_and_containment() {
    let root = tempdir();
    fs::create_dir(root.path().join(".devcontainer")).unwrap();
    fs::write(root.path().join("tsconfig.json"), TSCONFIG).unwrap();
    fs::write(
        root.path().join(".devcontainer/devcontainer.json"),
        DEVCONTAINER,
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let document = inspect(&service, &receipt);
    for (path, source, parent, child) in [
        ("tsconfig.json", TSCONFIG, "compilerOptions", "rootDir"),
        (
            ".devcontainer/devcontainer.json",
            DEVCONTAINER,
            "build",
            "args",
        ),
    ] {
        assert_source(&service, &receipt, path, source, "jsonc");
        assert!(!has_recovery(&document, path), "{path}");
        assert_property_contains(&document, path, source, parent, child);
    }
}

#[test]
fn jsonc_names_do_not_relax_ordinary_json_or_hide_malformed_configuration() {
    let root = tempdir();
    for (name, source) in [
        ("data.json", b"{\"value\":1,}\n".as_slice()),
        (
            "package.json",
            b"// comment\n{\"name\":\"example\",}\n".as_slice(),
        ),
        ("tsconfig.json", b"{\"compilerOptions\": [}\n".as_slice()),
        ("ordinary.json", b"{\"value\":1}\n".as_slice()),
    ] {
        fs::write(root.path().join(name), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let document = inspect(&service, &receipt);
    for path in ["data.json", "package.json", "tsconfig.json"] {
        assert!(has_recovery(&document, path), "{path}");
    }
    assert!(!has_recovery(&document, "ordinary.json"));
    assert_source(&service, &receipt, "data.json", b"{\"value\":1,}\n", "json");
    assert_source(
        &service,
        &receipt,
        "package.json",
        b"// comment\n{\"name\":\"example\",}\n",
        "json",
    );
    assert_source(
        &service,
        &receipt,
        "tsconfig.json",
        b"{\"compilerOptions\": [}\n",
        "jsonc",
    );
}

#[test]
fn explicit_configuration_precedes_conventional_jsonc_names() {
    let root = tempdir();
    fs::write(root.path().join("tsconfig.json"), TSCONFIG).unwrap();
    let storage = tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let configuration = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'tsconfig.json'='json'\n";
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(configuration), 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_source(&service, &receipt, "tsconfig.json", TSCONFIG, "json");
    assert!(has_recovery(&inspect(&service, &receipt), "tsconfig.json"));
}

#[test]
fn jsonc_configuration_survives_noop_incremental_reopen_and_restore() {
    let root = tempdir();
    let storage = tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    fs::write(root.path().join("tsconfig.json"), TSCONFIG).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let first = service.index_repository(root.path(), &deadline()).unwrap();
    assert_source(&service, &first, "tsconfig.json", TSCONFIG, "jsonc");
    assert_eq!(
        service
            .index_repository(root.path(), &deadline())
            .unwrap()
            .generation,
        first.generation
    );
    let changed =
        std::str::from_utf8(TSCONFIG)
            .unwrap()
            .replacen("\"noEmit\": true", "\"noEmit\": false", 1);
    assert_ne!(changed.as_bytes(), TSCONFIG);
    fs::write(root.path().join("tsconfig.json"), &changed).unwrap();
    let updated = service.index_repository(root.path(), &deadline()).unwrap();
    assert_ne!(updated.generation, first.generation);
    assert_source(
        &service,
        &updated,
        "tsconfig.json",
        changed.as_bytes(),
        "jsonc",
    );
    assert_property_contains(
        &inspect(&service, &updated),
        "tsconfig.json",
        changed.as_bytes(),
        "compilerOptions",
        "rootDir",
    );
    drop(service);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_source(&service, &first, "tsconfig.json", TSCONFIG, "jsonc");
    assert_source(
        &service,
        &updated,
        "tsconfig.json",
        changed.as_bytes(),
        "jsonc",
    );
    fs::write(root.path().join("tsconfig.json"), TSCONFIG).unwrap();
    let restored = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(restored.logical_snapshot, first.logical_snapshot);
    assert_source(&service, &restored, "tsconfig.json", TSCONFIG, "jsonc");
    assert!(!has_recovery(
        &inspect(&service, &restored),
        "tsconfig.json"
    ));
}
