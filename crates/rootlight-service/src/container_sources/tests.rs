//! Derived VBA sources participate in the actual durable indexing lifecycle.
//! These fixtures are authored, not copied from a diagnostic repository.

use super::*;
use std::fs;

#[path = "fixture.rs"]
mod fixture;

fn tempdir() -> tempfile::TempDir {
    #[cfg(target_os = "macos")]
    {
        tempfile::tempdir_in("/private/tmp").unwrap()
    }
    #[cfg(not(target_os = "macos"))]
    {
        tempfile::tempdir().unwrap()
    }
}

const FIRST: &[u8] = b"Attribute VB_Name = \"Alpha\"\r\nPublic Function Compute() As Long\r\n Compute = 1\r\nEnd Function\r\n";
const SECOND: &[u8] = b"Attribute VB_Name = \"Beta\"\r\nPublic Sub Dispatch()\r\nEnd Sub\r\n";

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn read_module(
    service: &FirstSliceService,
    generation: GenerationId,
    path: &str,
    bytes: &[u8],
) -> SourceRef {
    let cancellation = deadline();
    let located = service
        .code_locate(
            generation,
            path.to_owned(),
            LocateMode::Exact,
            10,
            0,
            &cancellation,
        )
        .unwrap();
    let hit = located
        .data
        .hits
        .iter()
        .find(|hit| hit.symbol.is_none() && hit.path == path)
        .unwrap();
    let reference = hit.source.clone().unwrap();
    let result = service
        .source_read_with_options_and_budget(
            generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    assert_eq!(result.data.chunks[0].bytes, bytes);
    assert_eq!(reference.content_hash(), content_hash(bytes));
    reference
}

#[test]
fn derived_modules_survive_edit_noop_rebuild_removal_and_restart() {
    let storage = tempdir();
    let root = tempdir();
    let cancellation = deadline();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let book = root.path().join("book.xlsm");
    let original = fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)]);
    fs::write(&book, &original).unwrap();
    let mut service = FirstSliceService::new_durable(5, paths.state_dir(), &cancellation).unwrap();
    let first = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(
        first.discovered_inputs, 1,
        "physical discovery excludes virtual modules"
    );
    assert_eq!(
        first.indexed_files, 3,
        "original container plus two real derived sources"
    );
    let alpha = derived_path("book.xlsm", "Alpha", ModuleKind::Procedural).unwrap();
    let beta = derived_path("book.xlsm", "Beta", ModuleKind::Procedural).unwrap();
    let stale = read_module(&service, first.generation, alpha.as_str(), FIRST);
    read_module(&service, first.generation, beta.as_str(), SECOND);
    let snapshot = service
        .loaded_generation_snapshot(first.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.version, NormalizedIrVersion::V1_6);
    assert_eq!(
        document
            .source_mappings
            .iter()
            .filter(|mapping| mapping.kind == SourceMappingKind::ExtractedFromContainer)
            .count(),
        2
    );
    assert!(
        !document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "container-source-incomplete")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.display_name == "Compute")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.display_name == "Dispatch")
    );
    assert_eq!(
        service
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        first
    );
    let changed = String::from_utf8(FIRST.to_vec())
        .unwrap()
        .replace("= 1", "= 2")
        .into_bytes();
    fs::write(
        &book,
        fixture::book(&[("Beta", SECOND), ("Alpha", &changed)]),
    )
    .unwrap();
    let updated = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let current = read_module(&service, updated.generation, alpha.as_str(), &changed);
    assert_eq!(
        stale.span().file(),
        current.span().file(),
        "stream reorder does not change source identity"
    );
    assert_ne!(first.generation, updated.generation);
    assert!(
        service
            .source_read(updated.generation, vec![stale], &cancellation)
            .is_err()
    );
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    let prepared = service
        .prepare_repository_with_options(
            root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &cancellation,
        )
        .unwrap();
    let clean = service.publish_prepared(prepared, &cancellation).unwrap();
    assert_eq!(updated.logical_snapshot, clean.logical_snapshot);
    fs::write(&book, fixture::book(&[("Alpha", &changed)])).unwrap();
    let removed = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(removed.indexed_files, 2);
    assert!(
        !service
            .loaded_generation_snapshot(removed.generation)
            .unwrap()
            .document()
            .files
            .iter()
            .any(|file| file.path == beta.as_str())
    );
    drop(service);
    fs::remove_file(&book).unwrap();
    let restored = FirstSliceService::new_durable(5, paths.state_dir(), &cancellation).unwrap();
    read_module(&restored, first.generation, alpha.as_str(), FIRST);
    read_module(&restored, updated.generation, alpha.as_str(), &changed);
    read_module(&restored, removed.generation, alpha.as_str(), &changed);
}

#[test]
fn unverified_module_keeps_source_and_does_not_hide_verified_sibling() {
    let root = tempdir();
    let wrong = b"Attribute VB_Name = \"Wrong\"\r\nPublic Sub Hidden()\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("book.xlsm"),
        fixture::book(&[("Alpha", FIRST), ("Beta", wrong)]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(receipt.indexed_files, 3);
    read_module(
        &service,
        receipt.generation,
        derived_path("book.xlsm", "Beta", ModuleKind::Procedural)
            .unwrap()
            .as_str(),
        wrong,
    );
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(
        snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "Compute")
    );
    assert!(
        !snapshot
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "Hidden")
    );
    assert!(
        snapshot
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "container-source-incomplete")
    );
}

#[test]
fn derived_paths_escape_stream_names_and_preserve_exact_unicode_identity() {
    let names = ["../Alpha", "a/b", "a\\b", "CON", "α", "é", "e\u{301}"];
    let paths = names
        .iter()
        .map(|name| {
            derived_path("nested/book.xlsm", name, ModuleKind::Procedural)
                .unwrap()
                .as_str()
                .to_owned()
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(paths.len(), names.len());
    assert!(
        paths
            .iter()
            .all(|path| path.starts_with("nested/book.xlsm/@vba/") && path.ends_with(".bas"))
    );
}

#[test]
fn admission_failure_spends_reservation_and_never_wraps() {
    let mut budget = Budget {
        work: 64,
        bytes: 64,
        files: 2,
    };
    let reservation = budget.reserve();
    assert_eq!(budget.work, 0);
    assert!(budget.settle(reservation, 65).is_err());
    budget.settle(reservation, 16).unwrap();
    assert_eq!(budget.work, 48);
}

fn prepared_fixture(
    bytes: &[u8],
    files: usize,
    capacity: usize,
    cancellation: &Cancellation,
) -> (PreparedSources, BTreeMap<FileId, SourceSnapshot>) {
    let repository = RepositoryId::from_bytes([81; 16]);
    let path = RelativePath::parse(Path::new("book.xlsm")).unwrap();
    let file = derive_file(rootlight_ids::FileIdentity {
        repository,
        path_identity: path.identity_bytes(),
    })
    .id();
    let hash = content_hash(bytes);
    let snapshot =
        SourceSnapshot::from_persisted(repository, path, file, hash, bytes.to_vec()).unwrap();
    let input = ManifestInput {
        file,
        path: "book.xlsm".to_owned(),
        content_hash: hash,
        bytes: u64::try_from(bytes.len()).unwrap(),
        encoding: "binary".to_owned(),
        class: InputClass::Source,
        language_signals: vec![LanguageSignal {
            language: "office-open-xml".to_owned(),
            evidence: LanguageEvidence::Content,
        }],
        decisive_rule: None,
    };
    let mut snapshots = BTreeMap::from([(file, snapshot)]);
    let prepared = prepare(
        &[input],
        &mut snapshots,
        repository,
        files,
        capacity,
        cancellation,
    )
    .unwrap();
    (prepared, snapshots)
}

#[test]
fn aggregate_admission_preserves_container_and_reports_remaining_modules() {
    let bytes = fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)]);
    let (prepared, snapshots) = prepared_fixture(&bytes, 1, 1_000_000, &deadline());
    assert_eq!(prepared.inputs.len(), 1);
    assert_eq!(snapshots.len(), 2);
    assert!(
        prepared
            .issues
            .iter()
            .any(|(_, message)| message.contains("remaining declarations: 1"))
    );
    let (prepared, snapshots) = prepared_fixture(&bytes, 2, 16, &deadline());
    assert!(prepared.inputs.is_empty());
    assert_eq!(snapshots.len(), 1);
    assert!(!prepared.issues.is_empty());
}

#[test]
fn declared_codepage_produces_exact_utf8_without_changing_original() {
    let source =
        b"Attribute VB_Name = \"Alpha\"\r\n' caf\xe9\r\nPublic Sub Compute()\r\nEnd Sub\r\n";
    let original = fixture::book(&[("Alpha", source)]);
    let (prepared, snapshots) = prepared_fixture(&original, 2, 1_000_000, &deadline());
    assert!(prepared.issues.is_empty(), "{:?}", prepared.issues);
    assert_eq!(prepared.inputs.len(), 1);
    let (derived, container) = prepared.origins[0];
    assert_eq!(snapshots[&container].content(), original);
    assert_eq!(
        snapshots[&derived].content(),
        "Attribute VB_Name = \"Alpha\"\r\n' café\r\nPublic Sub Compute()\r\nEnd Sub\r\n".as_bytes()
    );
    assert_eq!(prepared.fingerprints.len(), 2);
}

#[test]
fn identical_module_names_in_distinct_containers_remain_distinct() {
    let root = tempdir();
    let bytes = fixture::book(&[("Alpha", FIRST)]);
    fs::write(root.path().join("first.xlsm"), &bytes).unwrap();
    fs::write(root.path().join("second.xlsm"), &bytes).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    assert_eq!(receipt.discovered_inputs, 2);
    assert_eq!(receipt.indexed_files, 4);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let symbols = snapshot
        .document()
        .entities
        .iter()
        .filter(|entity| entity.display_name == "Compute")
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(symbols.len(), 2);
    assert_eq!(
        snapshot
            .document()
            .source_mappings
            .iter()
            .filter(|mapping| mapping.kind == SourceMappingKind::ExtractedFromContainer)
            .count(),
        2
    );
}
