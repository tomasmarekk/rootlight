//! Durable ABAP admission retains native declarations and untyped-clause gaps.

use super::*;
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationReadLimit, GenerationReader,
    OccurrenceReadRequest, RelationReadDirection, RelationReadRequest,
};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../../tests/native-grammars/fixtures/abap/zif_abapgit_gui_renderable.intf.abap"
);
const DEFINITIONS: &[u8] = include_bytes!(
    "../../../../tests/native-grammars/fixtures/abap/zif_abapgit_definitions.intf.abap"
);

#[test]
fn native_abap_class_owners_survive_reopen_without_implementation_duplicates() {
    for (raw, class_name, method_name) in [
        (
            include_bytes!(
                "../../../../tests/native-grammars/fixtures/abap/zcl_abapgit_object_tabl.clas.testclasses.abap"
            )
            .as_slice(),
            "ltcl_ci",
            "run",
        ),
        (
            include_bytes!(
                "../../../../tests/native-grammars/fixtures/abap/zcl_abapgit_diff_factory.clas.abap"
            )
            .as_slice(),
            "zcl_abapgit_diff_factory",
            "get",
        ),
    ] {
        let repository = durable_test_tempdir();
        let storage = durable_test_tempdir();
        let paths =
            RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
        paths.prepare_owner().unwrap();
        fs::write(repository.path().join("class.abap"), raw).unwrap();
        let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
        let assert_class = |service: &FirstSliceService, receipt: &FirstSliceIndexReceipt, raw: &[u8]| {
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let doc = snapshot.document();
            assert_eq!(doc.files.len(), 1);
            let file = &doc.files[0];
            assert_eq!(file.language, "abap");
            assert_eq!(file.content_hash, content_hash(raw));
            let source = service
                .source_read(
                    receipt.generation,
                    vec![file.evidence.source.clone().unwrap()],
                    &deadline(),
                )
                .unwrap();
            assert_eq!(source.data.chunks.len(), 1);
            assert_eq!(source.data.chunks[0].bytes, raw);
            let classes: Vec<_> = doc
                .entities
                .iter()
                .filter(|e| e.kind == EntityKind::Class)
                .collect();
            let methods: Vec<_> = doc
                .entities
                .iter()
                .filter(|e| e.kind == EntityKind::Method)
                .collect();
            assert_eq!(classes.len(), 1);
            assert_eq!(methods.len(), 1);
            let class = classes[0];
            let method = methods[0];
            assert_eq!(class.canonical_name, class_name);
            assert_eq!(method.canonical_name, method_name);
            assert_eq!(
                method.container,
                Some(rootlight_ir::ContainerRef::Entity(class.id))
            );
            for entity in [class, method] {
                let sites: Vec<_> = doc
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                    })
                    .collect();
                assert_eq!(sites.len(), if entity.kind == EntityKind::Method { 2 } else { 1 });
                for site in sites {
                    let span = site.source.span();
                    assert_eq!(site.source.content_hash(), content_hash(raw));
                    assert_eq!(
                        raw.get(
                            usize::try_from(span.start_byte()).unwrap()
                                ..usize::try_from(span.end_byte()).unwrap()
                        ),
                        Some(entity.display_name.as_bytes())
                    );
                    assert_eq!(site.syntactic_text_hash, content_hash(entity.display_name.as_bytes()));
                    if site.syntax_kind == "abap.method_implementation_name.reference" {
                        assert_eq!(site.enclosing, Some(class.id));
                        let evidence = site.evidence.source.as_ref().unwrap();
                        let start = raw.windows(7).position(|part| part == b"METHOD ").unwrap();
                        let end = raw.windows(10).position(|part| part == b"ENDMETHOD.").unwrap() + 10;
                        assert_eq!((evidence.span().start_byte(), evidence.span().end_byte()), (u64::try_from(start).unwrap(), u64::try_from(end).unwrap()));
                        assert_eq!(evidence.content_hash(), content_hash(raw));
                    }
                }
            }
            let relations: Vec<_> = doc
                .relations
                .iter()
                .filter(|relation| {
                    relation.predicate == RelationPredicate::Contains
                        && relation.subject == RelationEndpoint::Entity(class.id)
                        && relation.object == RelationEndpoint::Entity(method.id)
                })
                .collect();
            assert_eq!(relations.len(), 1);
            assert_eq!(
                relations[0].evidence.source.as_ref().unwrap().content_hash(),
                content_hash(raw)
            );
            assert!(doc.occurrences.iter().all(|site| {
                matches!(site.role, OccurrenceRole::Definition | OccurrenceRole::Documentation)
            }));
            assert!(doc.skipped_regions.iter().any(|gap| {
                gap.detail == "abap-class-header-bindings-unavailable"
            }));
            assert!(doc.skipped_regions.iter().any(|gap| {
                gap.detail == "abap-native-construct-not-lowered"
            }));
            if method_name == "get" {
                assert!(doc.skipped_regions.iter().any(|gap| {
                    gap.detail == "abap-static-method-dispatch-bindings-unavailable"
                }));
            }
            let cancel = deadline();
            let context = GenerationContext::new(&cancel, GenerationBudget::default());
            let directory = paths
                .state_dir()
                .join("first-slice/repositories")
                .join(receipt.repository.to_string())
                .join(receipt.generation.to_string());
            let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
            for entity in [class, method] {
                assert_eq!(reader.entity(entity.id, &context).unwrap().as_ref(), Some(entity));
            }
            let mut cold = Vec::new();
            let mut after = None;
            loop {
                let request = OccurrenceReadRequest::new(file.id, GenerationReadLimit::new(1).unwrap());
                let request = after.map_or(request, |cursor| request.with_after(cursor));
                let page = reader.occurrences(&request, &context).unwrap();
                cold.extend_from_slice(page.items());
                let Some(next) = page.next_cursor() else { break };
                after = Some(next);
            }
            assert_eq!(cold.len(), 3);
            for site in cold {
                assert_eq!(doc.occurrences.iter().find(|expected| expected.id == site.id), Some(&site));
            }
            (class.id, method.id)
        };
        let mut service =
            FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
                .unwrap();
        let fresh = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        let identities = assert_class(&service, &fresh, raw);
        assert_eq!(
            service
                .index_repository(repository.path(), &deadline())
                .unwrap()
                .generation,
            fresh.generation
        );
        let changed = std::str::from_utf8(raw).unwrap().replace(
            &format!("METHOD {method_name}."),
            &format!("METHOD {method_name}.\n    WRITE 'changed'."),
        );
        fs::write(repository.path().join("class.abap"), &changed).unwrap();
        let incremental = service.index_repository(repository.path(), &deadline()).unwrap();
        assert_ne!(incremental.generation, fresh.generation);
        assert_eq!(assert_class(&service, &incremental, changed.as_bytes()), identities);
        fs::write(repository.path().join("class.abap"), raw).unwrap();
        let restored = service.index_repository(repository.path(), &deadline()).unwrap();
        assert_ne!(restored.generation, incremental.generation);
        assert_eq!(assert_class(&service, &restored, raw), identities);
        drop(service);
        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(assert_class(&reopened, &restored, raw), identities);
        assert_eq!(
            reopened
                .index_repository(repository.path(), &deadline())
                .unwrap()
                .generation,
            restored.generation
        );
    }
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    raw: &[u8],
    method_name: &str,
) -> SymbolId {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    assert_eq!(doc.files.len(), 1);
    let file = &doc.files[0];
    assert_eq!(file.language, "abap");
    assert_eq!(file.content_hash, content_hash(raw));
    let source = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(source.data.chunks.len(), 1);
    assert_eq!(source.data.chunks[0].bytes, raw);
    let cancel = deadline();
    let context = GenerationContext::new(&cancel, GenerationBudget::default());
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
    assert_eq!(reader.file(file.id, &context).unwrap().as_ref(), Some(file));
    let owner = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Interface)
        .unwrap();
    let method = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Method)
        .unwrap();
    assert_eq!(owner.canonical_name, "zif_abapgit_gui_renderable");
    assert_eq!(method.canonical_name, method_name);
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Method)
            .count(),
        1
    );
    assert_eq!(
        method.container,
        Some(rootlight_ir::ContainerRef::Entity(owner.id))
    );
    for entity in [owner, method] {
        assert_eq!(
            reader.entity(entity.id, &context).unwrap().as_ref(),
            Some(entity)
        );
    }
    let mut cold = Vec::new();
    let mut after = None;
    loop {
        let request = OccurrenceReadRequest::new(file.id, GenerationReadLimit::new(1).unwrap());
        let request = after.map_or(request, |cursor| request.with_after(cursor));
        let page = reader.occurrences(&request, &context).unwrap();
        cold.extend_from_slice(page.items());
        let Some(next) = page.next_cursor() else {
            break;
        };
        assert!(after.is_none_or(|old| next > old));
        after = Some(next);
    }
    assert_eq!(cold.len(), 2);
    for (entity, name, start) in [
        (owner, "zif_abapgit_gui_renderable", 10),
        (method, method_name, 59),
    ] {
        let site = cold
            .iter()
            .find(|site| site.target == OccurrenceTarget::Resolved { symbol: entity.id })
            .unwrap();
        assert_eq!(site.role, OccurrenceRole::Definition);
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(site.source.span().start_byte(), start);
        assert_eq!(
            site.source.span().end_byte(),
            start + u64::try_from(name.len()).unwrap()
        );
        assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        assert_eq!(
            doc.occurrences
                .iter()
                .find(|expected| expected.id == site.id),
            Some(site)
        );
    }
    let contains: Vec<_> = doc
        .relations
        .iter()
        .filter(|r| {
            r.predicate == RelationPredicate::Contains
                && r.subject == RelationEndpoint::Entity(owner.id)
                && r.object == RelationEndpoint::Entity(method.id)
        })
        .collect();
    assert_eq!(contains.len(), 1);
    let evidence = contains[0].evidence.source.as_ref().unwrap();
    assert_eq!(evidence.content_hash(), content_hash(raw));
    assert!(
        evidence.span().start_byte() <= 59
            && evidence.span().end_byte() >= 59 + u64::try_from(method_name.len()).unwrap()
    );
    let relations = reader
        .relations(
            &RelationReadRequest::new(
                RelationEndpoint::Entity(owner.id),
                RelationReadDirection::Outgoing,
                GenerationReadLimit::new(1).unwrap(),
            )
            .with_predicates(vec![RelationPredicate::Contains])
            .unwrap(),
            &context,
        )
        .unwrap();
    assert_eq!(relations.items(), [contains[0].clone()]);
    assert!(relations.next_cursor().is_none());
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "abap-method-parameter-and-contract-bindings-unavailable")
    );
    assert!(!doc.entities.iter().any(|e| e.kind == EntityKind::Parameter));
    owner.id
}

#[test]
fn native_abap_survives_fresh_noop_changed_source_reopen_restore_and_cold_reads() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let source = repository.path().join("original.abap");
    fs::write(&source, ORIGINAL).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let fresh = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let owner = assert_generation(&service, &fresh, &paths, ORIGINAL, "render");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        fresh.generation
    );
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("METHODS render", "METHODS paint");
    fs::write(&source, &changed).unwrap();
    let updated = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, fresh.generation);
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_eq!(
        assert_generation(&service, &updated, &paths, changed.as_bytes(), "paint"),
        owner
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_eq!(
        assert_generation(&reopened, &updated, &paths, changed.as_bytes(), "paint"),
        owner
    );
    fs::write(&source, ORIGINAL).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, fresh.logical_snapshot);
    assert_eq!(
        assert_generation(&reopened, &restored, &paths, ORIGINAL, "render"),
        owner
    );
    drop(reopened);
    let mut cold =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_generation(&cold, &restored, &paths, ORIGINAL, "render"),
        owner
    );
    assert_eq!(
        cold.index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn abap_defaults_repair_original_false_cpp_and_keep_configuration_precedence() {
    assert_eq!(DEFINITIONS.len(), 16_094);
    let repository = durable_test_tempdir();
    fs::write(repository.path().join("definitions.ABAP"), DEFINITIONS).unwrap();
    fs::write(repository.path().join("other.txt"), ORIGINAL).unwrap();
    fs::write(repository.path().join("backup.abap.bak"), ORIGINAL).unwrap();
    fs::write(
        repository.path().join("override.abap"),
        b"def authored():\n    return 1\n",
    )
    .unwrap();
    let config = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'override.abap'='python'\n";
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some(config), 3).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let definitions = doc
        .files
        .iter()
        .find(|f| f.path == "definitions.ABAP")
        .unwrap();
    assert_eq!(definitions.language, "abap");
    assert_eq!(definitions.content_hash, content_hash(DEFINITIONS));
    let read = service
        .source_read(
            receipt.generation,
            vec![definitions.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, DEFINITIONS);
    assert!(
        doc.provenance.iter().any(|p| p.language == "abap"
            && p.frontend_version.as_deref() == Some("tree-sitter-abap-0.1.0"))
    );
    assert!(
        doc.skipped_regions.iter().any(
            |gap| gap.source.span().file() == definitions.id && gap.detail.starts_with("abap-")
        )
    );
    for path in ["other.txt", "backup.abap.bak", "override.abap"] {
        assert_ne!(
            doc.files.iter().find(|f| f.path == path).unwrap().language,
            "abap"
        );
    }
    assert_eq!(
        doc.files
            .iter()
            .find(|f| f.path == "override.abap")
            .unwrap()
            .language,
        "python"
    );
    assert!(doc.entities.iter().any(|e| e.language == "python"
        && e.kind == EntityKind::Function
        && e.canonical_name == "authored"));
    assert!(
        doc.occurrences
            .iter()
            .filter(|site| site.file == definitions.id)
            .all(|site| site.role == OccurrenceRole::Definition
                || site.role == OccurrenceRole::Documentation)
    );
}
