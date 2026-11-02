//! Default GYP discovery preserves written targets and source mentions durably.
//! Cold indexed reads must retain GYP roles without resolving source strings to
//! same-spelled targets or evaluating build configuration and commands.

use super::*;
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationReadLimit, GenerationReader,
    OccurrenceReadRequest,
};

const ORIGINALS: [(&str, &[u8], usize, usize); 5] = [
    (
        "explicit/binding.gyp",
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/gyp/binding-deno.gyp"),
        1,
        1,
    ),
    (
        "implicit/binding.gyp",
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/gyp/binding-deno.gyp"),
        1,
        1,
    ),
    (
        "electron/uv-dlopen.gyp",
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/gyp/uv-dlopen.gyp"),
        2,
        2,
    ),
    (
        "electron/mouse-input.gyp",
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/gyp/mouse-input.gyp"),
        1,
        8,
    ),
    (
        "electron/virtual-display.gyp",
        include_bytes!(
            "../../../rootlight-adapter-treesitter/tests/fixtures/gyp/virtual-display.gyp"
        ),
        1,
        2,
    ),
];

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    first: &[u8],
) -> SymbolId {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let cancel = deadline();
    let context = GenerationContext::new(&cancel, GenerationBudget::default());
    let directory = paths
        .state_dir()
        .join("first-slice/repositories")
        .join(receipt.repository.to_string())
        .join(receipt.generation.to_string());
    let reader = rootlight_catalog::OracleReader::open_in(&directory, &context).unwrap();
    assert_eq!(document.files.len(), ORIGINALS.len());
    let mut first_target = None;
    for (index, (path, original, targets, references)) in ORIGINALS.into_iter().enumerate() {
        let raw = if index == 0 { first } else { original };
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "gyp");
        assert_eq!(file.content_hash, content_hash(raw));
        assert_eq!(reader.file(file.id, &context).unwrap().as_ref(), Some(file));
        let source = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(source.data.chunks[0].bytes, raw);
        let definitions: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::BuildTarget
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        assert_eq!(definitions.len(), targets);
        for target in &definitions {
            assert_eq!(target.language, "gyp");
            assert_eq!(
                reader.entity(target.id, &context).unwrap().as_ref(),
                Some(*target)
            );
        }
        if index == 0 {
            first_target = Some(definitions[0].id);
        }
        let expected: BTreeSet<_> = document
            .occurrences
            .iter()
            .filter(|site| site.file == file.id)
            .map(|site| site.id)
            .collect();
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
        assert_eq!(
            cold.iter().map(|site| site.id).collect::<BTreeSet<_>>(),
            expected
        );
        let uses: Vec<_> = cold
            .iter()
            .filter(|site| site.syntax_kind == "gyp.source_path.reference")
            .collect();
        assert_eq!(uses.len(), references);
        for site in uses {
            assert_eq!(site.role, OccurrenceRole::Reference);
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(
                definitions
                    .iter()
                    .any(|target| site.enclosing == Some(target.id))
            );
            assert_eq!(site.source.content_hash(), content_hash(raw));
            assert_eq!(site.source.generation(), receipt.generation);
            let span = site.source.span();
            let text = raw
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap();
            assert_eq!(site.syntactic_text_hash, content_hash(text));
        }
    }
    assert!(!document.entities.iter().any(|e| e.language == "python"));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DependsOn)
    );
    first_target.unwrap()
}

#[test]
fn original_gyp_sources_survive_default_noop_changed_body_restore_and_cold_reads() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    for (path, raw, _, _) in ORIGINALS {
        let file = repository.path().join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, raw).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let target = assert_generation(&service, &initial, &paths, ORIGINALS[0].1);
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINALS[0].1)
        .unwrap()
        .replace("src/binding.cc", "src/changed.cc")
        .into_bytes();
    fs::write(repository.path().join(ORIGINALS[0].0), &changed).unwrap();
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
    assert_eq!(
        assert_generation(&service, &updated, &paths, &changed),
        target
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_eq!(
        assert_generation(&reopened, &updated, &paths, &changed),
        target
    );
    fs::write(repository.path().join(ORIGINALS[0].0), ORIGINALS[0].1).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_eq!(
        assert_generation(&reopened, &restored, &paths, ORIGINALS[0].1),
        target
    );
    drop(reopened);
    let mut cold =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_generation(&cold, &restored, &paths, ORIGINALS[0].1),
        target
    );
    assert_eq!(
        cold.index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn gyp_configuration_precedence_and_literal_path_fence_do_not_invent_bindings() {
    let repository = durable_test_tempdir();
    fs::write(
        repository.path().join("ordinary.GYP"),
        b"{'targets': [{'target_name': 'same', 'sources': ['same']}]}\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("fragment.gypi"),
        b"{'variables': {'literal': 1}}\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("foreign.gyp"),
        b"def authored():\n    return 1\n",
    )
    .unwrap();
    let config = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'foreign.gyp'='python'\n";
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
    let document = snapshot.document();
    for (path, language) in [
        ("ordinary.GYP", "gyp"),
        ("fragment.gypi", "gyp"),
        ("foreign.gyp", "python"),
    ] {
        assert_eq!(
            document
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap()
                .language,
            language
        );
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|e| e.language == "gyp" && e.kind == EntityKind::BuildTarget)
            .count(),
        1
    );
    assert!(document.entities.iter().any(|e| e.language == "python"
        && e.kind == EntityKind::Function
        && e.canonical_name == "authored"));
    let uses: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "gyp.source_path.reference")
        .collect();
    assert_eq!(uses.len(), 1);
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DependsOn)
    );
}
