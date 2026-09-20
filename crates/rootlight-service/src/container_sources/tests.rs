//! Derived VBA sources participate in the actual durable indexing lifecycle.
//! These fixtures are authored, not copied from a diagnostic repository.

use super::*;
use rootlight_ir::OccurrenceTarget;
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
    let origin = hit.extracted_from.as_ref().expect("derived locate origin");
    assert_eq!(result.data.chunks[0].extracted_from.as_ref(), Some(origin));
    assert_eq!(origin.repository(), reference.repository());
    assert_eq!(origin.generation(), generation);
    assert_ne!(origin.span().file(), reference.span().file());
    assert_eq!(origin.span().start_byte(), 0);
    assert!(origin.line_hint().is_none());
    let container = service
        .source_read_with_options_and_budget(
            generation,
            vec![origin.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &cancellation,
        )
        .unwrap();
    let container = &container.data.chunks[0];
    assert!(container.extracted_from.is_none());
    assert_eq!(content_hash(&container.bytes), origin.content_hash());
    assert_eq!(
        u64::try_from(container.bytes.len()).unwrap(),
        origin.span().end_byte()
    );
    reference
}

#[test]
fn native_project_identity_issues_preserve_sources_and_follow_metadata_after_restart() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::book(&[("Alpha", FIRST)]),
    )
    .unwrap();
    let mut fixed_symbol = None;
    let mut previous_generation = None;
    for (value, issue) in [
        ("\"Project\"", None),
        ("\"Different\"", Some("Mismatch")),
        ("Project", Some("Quoting")),
        ("\"Project\"", None),
    ] {
        fs::write(
            root.path().join("book.xlsm"),
            fixture::book_with_project_value(&[("Alpha", FIRST, false)], value),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 2);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        if let Some(previous) = previous_generation {
            assert_ne!(receipt.generation, previous);
        }
        previous_generation = Some(receipt.generation);
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let diagnostics = document
            .diagnostics
            .iter()
            .filter(|d| d.message.starts_with("vba project identity:"))
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), usize::from(issue.is_some()));
        if let Some(issue) = issue {
            let diagnostic = diagnostics[0];
            assert_eq!(diagnostic.message, format!("vba project identity: {issue}"));
            assert_eq!(diagnostic.coverage_effect, CoverageStatus::Unknown);
            let file = diagnostic.source.as_ref().unwrap().span().file();
            assert_eq!(
                document.files.iter().find(|f| f.id == file).unwrap().path,
                "book.xlsm"
            );
        }
        for container in ["book.xlsm", "fixed.xlsm"] {
            let path = derived_path(container, "Alpha", ModuleKind::Procedural).unwrap();
            let reference = read_module(&service, receipt.generation, path.as_str(), FIRST);
            let symbol = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Compute"
                        && e.evidence.source.as_ref().unwrap().span().file()
                            == reference.span().file()
                })
                .unwrap()
                .id;
            if container == "fixed.xlsm" {
                if let Some(previous) = fixed_symbol {
                    assert_eq!(symbol, previous);
                }
                fixed_symbol = Some(symbol);
            }
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let restored_snapshot = restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        assert_eq!(
            restored_snapshot.document().diagnostics,
            document.diagnostics
        );
        let path = derived_path("book.xlsm", "Alpha", ModuleKind::Procedural).unwrap();
        read_module(&restored, receipt.generation, path.as_str(), FIRST);
    }
}

#[test]
fn native_office_classes_keep_exact_sources_members_and_restart_identity() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let body = "Attribute VB_Name = \"Counter\"\r\nAttribute VB_GlobalNameSpace = False\r\nAttribute VB_Creatable = False\r\nAttribute VB_PredeclaredId = False\r\nAttribute VB_Exposed = False\r\nPrivate total As Long\r\nPublic Sub Refresh()\r\ntotal = 1\r\nEnd Sub\r\nPublic Sub Run()\r\nMe.Refresh\r\nEnd Sub\r\n";
    let path = derived_path("book.xlsm", "Counter", ModuleKind::Object).unwrap();
    let mut previous = None;
    for source in [body.to_owned(), body.replace("total = 1", "total = 2 + 3")] {
        fs::write(
            root.path().join("book.xlsm"),
            fixture::typed_book(&[
                ("Counter", source.as_bytes(), true),
                ("Beta", SECOND, false),
            ]),
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 2);
        assert_eq!(
            service.index_repository(root.path(), &deadline()).unwrap(),
            receipt
        );
        read_module(
            &service,
            receipt.generation,
            path.as_str(),
            source.as_bytes(),
        );
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|f| f.path == path.as_str())
            .unwrap();
        let owner = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "Counter")
            .unwrap();
        assert_eq!(owner.kind, EntityKind::Class);
        let identities = document
            .entities
            .iter()
            .filter(|e| {
                e.evidence
                    .source
                    .as_ref()
                    .is_some_and(|s| s.span().file() == file.id)
            })
            .map(|e| (e.canonical_name.clone(), e.id))
            .collect::<BTreeMap<_, _>>();
        if let Some(prior) = &previous {
            assert_eq!(&identities, prior);
        }
        previous = Some(identities.clone());
        for (name, kind) in [
            ("total", EntityKind::Field),
            ("Refresh", EntityKind::Method),
            ("Run", EntityKind::Method),
        ] {
            let member = document
                .entities
                .iter()
                .find(|e| e.canonical_name == name)
                .unwrap();
            assert_eq!(member.kind, kind);
            assert_eq!(
                member.container,
                Some(rootlight_ir::ContainerRef::Entity(owner.id))
            );
        }
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: identities["Refresh"]
                        })
        );
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
        drop(service);
        let restored = FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        read_module(
            &restored,
            receipt.generation,
            path.as_str(),
            source.as_bytes(),
        );
        let restored = restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        assert!(
            restored
                .document()
                .entities
                .iter()
                .any(|e| e.id == owner.id && e.kind == EntityKind::Class)
        );
    }
}

#[test]
fn native_office_metadata_changes_do_not_reuse_foreign_module_kinds() {
    let root = tempdir();
    let source = b"Attribute VB_Name = \"Counter\"\r\nPublic Sub Refresh()\r\nEnd Sub\r\nPublic Sub Run()\r\nMe.Refresh\r\nEnd Sub\r\n";
    fs::write(
        root.path().join("fixed.xlsm"),
        fixture::typed_book(&[("Counter", source, true)]),
    )
    .unwrap();
    let mut service = FirstSliceService::new(8).unwrap();
    let mut fixed_identity = None;
    let mut prior_generation = None;
    for object in [false, true, false] {
        fs::write(
            root.path().join("changing.xlsm"),
            fixture::typed_book(&[("Counter", source, object)]),
        )
        .unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.extracted_files, 2);
        assert_ne!(prior_generation, Some(receipt.generation));
        prior_generation = Some(receipt.generation);
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let owners: Vec<_> = document
            .entities
            .iter()
            .filter(|e| e.canonical_name == "Counter")
            .collect();
        assert_eq!(owners.len(), 2);
        assert_ne!(owners[0].id, owners[1].id);
        for (container, is_object) in [("changing.xlsm", object), ("fixed.xlsm", true)] {
            let kind = if is_object {
                ModuleKind::Object
            } else {
                ModuleKind::Procedural
            };
            let path = derived_path(container, "Counter", kind).unwrap();
            let reference = read_module(&service, receipt.generation, path.as_str(), source);
            let owner = owners
                .iter()
                .find(|e| {
                    e.evidence.source.as_ref().unwrap().span().file() == reference.span().file()
                })
                .unwrap();
            assert_eq!(
                owner.kind,
                if is_object {
                    EntityKind::Class
                } else {
                    EntityKind::Module
                }
            );
            if container == "fixed.xlsm" {
                if let Some(previous) = fixed_identity {
                    assert_eq!(owner.id, previous);
                }
                fixed_identity = Some(owner.id);
            }
            let refresh = document
                .entities
                .iter()
                .find(|e| {
                    e.canonical_name == "Refresh"
                        && e.container == Some(rootlight_ir::ContainerRef::Entity(owner.id))
                })
                .unwrap();
            assert_eq!(
                refresh.kind,
                if is_object {
                    EntityKind::Method
                } else {
                    EntityKind::Function
                }
            );
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.self_invoked_member.reference"
                        && site.source.span().file() == reference.span().file()
                })
                .unwrap();
            if is_object {
                assert_eq!(
                    site.target,
                    OccurrenceTarget::Resolved { symbol: refresh.id }
                );
            } else {
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            }
        }
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert_eq!(rebuilt.logical_snapshot, receipt.logical_snapshot);
    }
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
    assert_eq!(serde_json::to_value(&first).unwrap()["extracted_files"], 2);
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
    assert_eq!(updated.extracted_files, 2);
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
    assert_eq!(clean.extracted_files, 2);
    fs::write(&book, fixture::book(&[("Alpha", &changed)])).unwrap();
    let removed = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_eq!(removed.indexed_files, 2);
    assert_eq!(removed.extracted_files, 1);
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
    for receipt in [&first, &updated, &removed] {
        let recovered = restored.receipts.get(&receipt.generation).unwrap();
        assert_eq!(recovered.extracted_files, receipt.extracted_files);
        assert_eq!(recovered.indexed_files - recovered.extracted_files, 1);
    }
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
    assert_eq!(receipt.extracted_files, 2);
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
    assert_eq!(receipt.extracted_files, 2);
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

#[test]
fn extracted_counts_restore_legacy_manifests_and_reject_false_current_counts() {
    let storage = tempdir();
    let root = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let book = root.path().join("book.xlsm");
    fs::write(&book, fixture::book(&[("Alpha", FIRST), ("Beta", SECOND)])).unwrap();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    let receipt = service.index_repository(root.path(), &deadline()).unwrap();
    let manifest_path = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string())
        .join("manifest.json");
    let original = fs::read(&manifest_path).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(manifest["version"], 4);
    assert_eq!(manifest["receipt"]["extracted_files"], 2);
    let retained_without_manifest = receipt
        .retained_durable_bytes
        .checked_sub(u64::try_from(original.len()).unwrap())
        .unwrap();
    fs::remove_file(&book).unwrap();
    for (version, extracted, discovered, valid) in [
        (4, Some(2), 1, true),
        (4, None, 1, false),
        (4, Some(0), 1, false),
        (4, Some(3), 1, false),
        (4, Some(u64::MAX), 1, false),
        (4, Some(2), 0, false),
        (3, None, 1, true),
        (3, None, 0, false),
    ] {
        let mut changed = manifest.clone();
        changed["version"] = version.into();
        changed["receipt"]["discovered_inputs"] = discovered.into();
        match extracted {
            Some(count) => changed["receipt"]["extracted_files"] = count.into(),
            None => {
                changed["receipt"]
                    .as_object_mut()
                    .unwrap()
                    .remove("extracted_files");
            }
        }
        let bytes = loop {
            let bytes = serde_json::to_vec(&changed).unwrap();
            let retained = retained_without_manifest
                .checked_add(u64::try_from(bytes.len()).unwrap())
                .unwrap();
            if changed["receipt"]["retained_durable_bytes"] == retained {
                break bytes;
            }
            changed["receipt"]["retained_durable_bytes"] = retained.into();
        };
        fs::write(&manifest_path, bytes).unwrap();
        let result = service.durable.as_ref().unwrap().restore_exact_generation(
            receipt.repository,
            receipt.generation,
            &deadline(),
        );
        if valid {
            let restored = result.unwrap();
            assert_eq!(restored.receipt.discovered_inputs, 1);
            assert_eq!(restored.receipt.indexed_files, 3);
            assert_eq!(restored.receipt.extracted_files, 2);
        } else {
            assert!(matches!(result, Err(FirstSliceError::CatalogCorrupt)));
        }
    }
}
