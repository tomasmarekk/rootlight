//! Durable VB.NET default admission preserves original declaration evidence.
//! Full-source retention and lifecycle controls do not qualify body semantics.

use super::*;
use rootlight_source::{SourceEncoding, SourceReadOptions};
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationReadLimit, GenerationReader,
    OccurrenceReadRequest,
};

const ORIGINAL: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/vbnet/TaskbarAnimate.vb");

const MSAA: &[u8] = include_bytes!("../../../../tests/native-grammars/fixtures/vbnet/MSAA.vb");
const STYLE: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/vbnet/TaskbarStyle.vb");
const MAIN_WINDOW: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/vbnet/MainWindow.xaml.vb");

fn assert_main_window_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    raw: &[u8],
) -> Vec<SymbolId> {
    let cancel = deadline();
    let context = GenerationContext::new(&cancel, GenerationBudget::default());
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
    let generation = reader.read_generation(&context).unwrap();
    let doc = generation.document();
    assert_eq!(doc.files.len(), 1);
    let file = &doc.files[0];
    assert_eq!(file.language, "visual-basic-dotnet");
    assert_eq!(file.content_hash, content_hash(raw));
    let mut exact_bytes = Vec::new();
    let mut start = 0;
    while start < raw.len() {
        let limit = (start + 32 * 1024).min(raw.len());
        let end = if limit == raw.len() {
            limit
        } else {
            start
                + raw[start..limit]
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .unwrap()
                + 1
        };
        let span =
            SourceSpan::new(file.id, start.try_into().unwrap(), end.try_into().unwrap()).unwrap();
        let source = SourceRef::new(
            file.repository,
            receipt.generation,
            span,
            file.content_hash,
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(
            (read.data.chunks[0].start_byte, read.data.chunks[0].end_byte),
            (start as u64, end as u64)
        );
        assert!(read.data.chunks[0].bytes == raw[start..end]);
        exact_bytes.extend_from_slice(&read.data.chunks[0].bytes);
        start = end;
    }
    assert!(exact_bytes == raw);
    for (kind, count) in [
        (EntityKind::Class, 1),
        (EntityKind::Struct, 3),
        (EntityKind::Enum, 3),
        (EntityKind::Method, 48),
    ] {
        assert_eq!(
            doc.entities
                .iter()
                .filter(|entity| entity.kind == kind)
                .count(),
            count
        );
    }
    let class = doc
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "MainWindow")
        .unwrap();
    let mut identities = Vec::new();
    for (name, kind, start, end) in [
        ("MainWindow", EntityKind::Class, 336, 346),
        ("AccentPolicy", EntityKind::Struct, 3369, 3381),
        ("Button_Click_8", EntityKind::Method, 71_771, 71_785),
    ] {
        let entity = doc
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind);
        if entity.id != class.id {
            assert_eq!(
                entity.container,
                Some(rootlight_ir::ContainerRef::Entity(class.id))
            );
        }
        let sites: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            (
                sites[0].source.span().start_byte(),
                sites[0].source.span().end_byte()
            ),
            (start, end)
        );
        assert_eq!(sites[0].source.content_hash(), content_hash(raw));
        identities.push(entity.id);
    }
    for start in [73_925, 74_403] {
        assert!(doc.skipped_regions.iter().any(|gap| {
            gap.detail == "vbnet-statement-binding-and-call-analysis-unavailable"
                && gap.source.span().start_byte() == start
                && gap.source.generation() == receipt.generation
        }));
    }
    assert!(
        !doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "vbnet-complete-native-structure-unavailable")
    );
    assert!(!doc.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::Reads | RelationPredicate::Writes
    )));
    identities
}

#[test]
fn vbnet_complete_main_window_survives_noop_append_restore_and_cold_reopen() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let source = repository.path().join("MainWindow.xaml.vb");
    fs::write(&source, MAIN_WINDOW).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let fresh = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let identities = assert_main_window_generation(&service, &fresh, &paths, MAIN_WINDOW);
    let noop = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(noop.generation, fresh.generation);
    let mut changed = MAIN_WINDOW.to_vec();
    changed.push(b'\n');
    fs::write(&source, &changed).unwrap();
    let incremental = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(incremental.generation, fresh.generation);
    assert_eq!(
        assert_main_window_generation(&service, &incremental, &paths, &changed),
        identities
    );
    fs::write(&source, MAIN_WINDOW).unwrap();
    let restored = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(restored.generation, incremental.generation);
    assert_eq!(
        assert_main_window_generation(&service, &restored, &paths, MAIN_WINDOW),
        identities
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_main_window_generation(&reopened, &restored, &paths, MAIN_WINDOW),
        identities
    );
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

fn assert_external_originals(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    msaa: &[u8],
    external_name: &str,
) -> Vec<SymbolId> {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    assert_eq!(doc.files.len(), 2);
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Method)
            .count(),
        15
    );
    let cancel = deadline();
    let context = GenerationContext::new(&cancel, GenerationBudget::default());
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
    let mut identities = Vec::new();
    for (raw, names, spans) in [
        (
            msaa,
            ["MSAA", external_name, "Hwnd"],
            [(122, 126), (607, 633), (654, 658)],
        ),
        (
            STYLE,
            ["TaskbarStyle", "EnumWindows", "Adress"],
            [(149, 161), (292, 303), (324, 330)],
        ),
    ] {
        let file = doc
            .files
            .iter()
            .find(|file| file.content_hash == content_hash(raw))
            .unwrap();
        assert_eq!(file.language, "visual-basic-dotnet");
        let read = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].bytes, raw);
        let mut selected = Vec::new();
        for ((kind, name), (start, end)) in
            [EntityKind::Class, EntityKind::Method, EntityKind::Parameter]
                .into_iter()
                .zip(names)
                .zip(spans)
        {
            let entity = doc
                .entities
                .iter()
                .find(|entity| entity.kind == kind && entity.canonical_name == name)
                .expect(name);
            assert_eq!(entity.language, "visual-basic-dotnet");
            assert_eq!(entity.display_name, name);
            assert_eq!(
                reader.entity(entity.id, &context).unwrap().as_ref(),
                Some(entity)
            );
            let site = doc
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .unwrap();
            assert_eq!(
                (
                    site.source.span().start_byte(),
                    site.source.span().end_byte()
                ),
                (start, end)
            );
            assert_eq!(site.source.content_hash(), content_hash(raw));
            assert_eq!(
                &raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
                name.as_bytes()
            );
            selected.push(entity);
            identities.push(entity.id);
        }
        for pair in selected.windows(2) {
            assert_eq!(
                pair[1].container,
                Some(rootlight_ir::ContainerRef::Entity(pair[0].id))
            );
            assert!(
                doc.relations
                    .iter()
                    .any(|r| r.predicate == RelationPredicate::Contains
                        && r.subject == RelationEndpoint::Entity(pair[0].id)
                        && r.object == RelationEndpoint::Entity(pair[1].id))
            );
        }
    }
    assert_eq!(
        doc.skipped_regions
            .iter()
            .filter(|gap| gap.detail == "vbnet-external-abi-and-entry-point-bindings-unavailable")
            .count(),
        2
    );
    assert!(
        !doc.skipped_regions
            .iter()
            .any(|gap| gap.detail == "vbnet-complete-native-structure-unavailable")
    );
    assert!(
        !doc.relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::Calls)
    );
    identities
}

#[test]
fn vbnet_complete_external_originals_survive_noop_case_edit_restore_and_cold_reopen() {
    let repository = durable_test_tempdir();
    let path = repository.path().join("MSAA.vb");
    fs::write(&path, MSAA).unwrap();
    fs::write(repository.path().join("TaskbarStyle.vb"), STYLE).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let fresh = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let ids =
        assert_external_originals(&service, &fresh, &paths, MSAA, "AccessibleObjectFromWindow");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        fresh.generation
    );
    let changed = String::from_utf8(MSAA.to_vec())
        .unwrap()
        .replace("AccessibleObjectFromWindow", "ACCESSIBLEOBJECTFROMWINDOW");
    fs::write(&path, changed.as_bytes()).unwrap();
    let incremental = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(incremental.generation, fresh.generation);
    assert_eq!(
        assert_external_originals(
            &service,
            &incremental,
            &paths,
            changed.as_bytes(),
            "ACCESSIBLEOBJECTFROMWINDOW"
        ),
        ids
    );
    fs::write(&path, MSAA).unwrap();
    let restored = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(restored.generation, incremental.generation);
    assert_eq!(
        assert_external_originals(
            &service,
            &restored,
            &paths,
            MSAA,
            "AccessibleObjectFromWindow"
        ),
        ids
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_external_originals(
            &reopened,
            &restored,
            &paths,
            MSAA,
            "AccessibleObjectFromWindow"
        ),
        ids
    );
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn vbnet_configured_aliases_index_original_under_the_canonical_language() {
    let repository = durable_test_tempdir();
    fs::write(repository.path().join("TaskbarAnimate.vb"), ORIGINAL).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut expected = None;
    for language in [
        None,
        Some("vbnet"),
        Some("visualbasicnet"),
        Some("visual-basic-dotnet"),
    ] {
        let config = language.map(|language| format!("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'TaskbarAnimate.vb'='{language}'\n"));
        let policy = FirstSliceStoragePolicy::resolve_user_config(config.as_deref(), 3).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        let receipt = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        let actual = assert_generation(&service, &receipt, &paths, ORIGINAL);
        if let Some(expected) = expected {
            assert_eq!(actual, expected, "configured language {language:?}");
        } else {
            expected = Some(actual);
        }
    }
}

#[test]
fn vbnet_common_configuration_alias_route_keeps_cpp_identity_and_unknown_overrides() {
    let repository = durable_test_tempdir();
    let source = b"int Run(int value) { return value; }\n";
    fs::write(repository.path().join("owner.cpp"), source).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut expected = None;
    for language in [None, Some("cplusplus"), Some("cpp")] {
        let config = language.map(|language| format!("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'owner.cpp'='{language}'\n"));
        let policy = FirstSliceStoragePolicy::resolve_user_config(config.as_deref(), 3).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                .unwrap();
        let receipt = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let doc = snapshot.document();
        assert_eq!(doc.files.len(), 1);
        assert_eq!(doc.files[0].language, "cpp");
        assert_eq!(doc.files[0].content_hash, content_hash(source));
        let function = doc
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Run")
            .unwrap();
        assert_eq!(function.language, "cpp");
        if let Some(expected) = expected {
            assert_eq!(function.id, expected);
        } else {
            expected = Some(function.id);
        }
    }
    let service = FirstSliceService::new(2).unwrap();
    let mut input = ManifestInput {
        file: FileId::from_bytes([17; 20]),
        path: "owner.cpp".to_owned(),
        content_hash: content_hash(source),
        bytes: u64::try_from(source.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![
            rootlight_discovery::LanguageSignal {
                language: "cpp".to_owned(),
                evidence: LanguageEvidence::Extension,
            },
            rootlight_discovery::LanguageSignal {
                language: "custom-unavailable".to_owned(),
                evidence: LanguageEvidence::Configuration,
            },
        ],
        project_language_witnesses: Vec::new(),
        decisive_rule: None,
    };
    assert_eq!(detected_source_language(&input), Some("custom-unavailable"));
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    input.language_signals[1].language = "cplusplus".to_owned();
    assert_eq!(detected_source_language(&input), Some("cpp"));
    let mut unavailable = service.analyzers.clone();
    unavailable.remove("cpp");
    assert_eq!(source_language_route(&input, &unavailable), None);
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    raw: &[u8],
) -> (SymbolId, SymbolId, SymbolId) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    assert_eq!(doc.files.len(), 1);
    let file = &doc.files[0];
    assert_eq!(file.language, "visual-basic-dotnet");
    assert_eq!(file.content_hash, content_hash(raw));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, raw);
    let class = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Class)
        .unwrap();
    let method = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Method)
        .unwrap();
    let formal = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Parameter && e.canonical_name == "hwnd")
        .unwrap();
    assert_eq!(class.canonical_name, "TaskbarAnimate");
    assert_eq!(class.display_name, "TaskbarAnimate");
    assert_eq!(method.canonical_name, "Animate");
    assert_eq!(method.display_name, "Animate");
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Class)
            .count(),
        1
    );
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Method)
            .count(),
        1
    );
    assert_eq!(
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Parameter)
            .count(),
        8
    );
    for (owner, member) in [(class, method), (method, formal)] {
        assert_eq!(
            member.container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
        assert_eq!(
            doc.relations
                .iter()
                .filter(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(owner.id)
                    && r.object == RelationEndpoint::Entity(member.id))
                .count(),
            1
        );
    }
    let cancel = deadline();
    let context = GenerationContext::new(&cancel, GenerationBudget::default());
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
    for entity in [class, method, formal] {
        assert_eq!(
            reader.entity(entity.id, &context).unwrap().as_ref(),
            Some(entity)
        );
    }
    let mut cold = Vec::new();
    let mut after = None;
    loop {
        let request = OccurrenceReadRequest::new(file.id, GenerationReadLimit::new(2).unwrap());
        let request = after.map_or(request, |cursor| request.with_after(cursor));
        let page = reader.occurrences(&request, &context).unwrap();
        cold.extend_from_slice(page.items());
        let Some(next) = page.next_cursor() else {
            break;
        };
        assert!(after.is_none_or(|old| next > old));
        after = Some(next);
    }
    assert_eq!(cold.len(), doc.occurrences.len());
    for site in &cold {
        assert_eq!(doc.occurrences.iter().find(|s| s.id == site.id), Some(site));
    }
    for (entity, kind, name, start, end) in [
        (class, EntityKind::Class, "TaskbarAnimate", 36, 50),
        (method, EntityKind::Method, "Animate", 122, 129),
        (formal, EntityKind::Parameter, "hwnd", 136, 140),
    ] {
        assert_eq!(entity.language, "visual-basic-dotnet");
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.display_name, name);
        let sites: Vec<_> = cold
            .iter()
            .filter(|s| {
                s.role == OccurrenceRole::Definition
                    && s.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert_eq!(
            (
                site.source.span().start_byte(),
                site.source.span().end_byte()
            ),
            (start, end)
        );
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(
            site.syntactic_text_hash,
            content_hash(&raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()])
        );
    }
    assert!(
        doc.skipped_regions
            .iter()
            .any(|g| g.detail == "vbnet-statement-binding-and-call-analysis-unavailable")
    );
    assert!(!doc.relations.iter().any(|r| matches!(
        r.predicate,
        RelationPredicate::Calls | RelationPredicate::Reads | RelationPredicate::Writes
    )));
    (class.id, method.id, formal.id)
}

#[test]
fn original_vbnet_default_route_survives_noop_edit_restore_and_cold_reopen() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let path = repository.path().join("TaskbarAnimate.vb");
    fs::write(&path, ORIGINAL).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let fresh = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let identities = assert_generation(&service, &fresh, &paths, ORIGINAL);
    let noop = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(noop.generation, fresh.generation);
    let changed = String::from_utf8(ORIGINAL.to_vec())
        .unwrap()
        .replace("Main.ClearMemory()", "Main.ClearMemory( )");
    assert_ne!(changed.as_bytes(), ORIGINAL);
    fs::write(&path, changed.as_bytes()).unwrap();
    let incremental = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(incremental.generation, fresh.generation);
    assert_eq!(
        assert_generation(&service, &incremental, &paths, changed.as_bytes()),
        identities
    );
    fs::write(&path, ORIGINAL).unwrap();
    let restored = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(restored.generation, incremental.generation);
    assert_eq!(
        assert_generation(&service, &restored, &paths, ORIGINAL),
        identities
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_generation(&reopened, &restored, &paths, ORIGINAL),
        identities
    );
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}
