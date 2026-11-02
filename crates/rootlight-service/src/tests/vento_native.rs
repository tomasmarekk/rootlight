//! Durable Vento host facts retain physical source without host-filter binding.

use super::*;
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationReadLimit, GenerationReader,
    OccurrenceReadRequest,
};

const ORIGINAL: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/vento/badly_formatted.vto");
const HOST: &str = "{{ function toUpperCase() }}literal{{ /function }}\n{{ function greet(name = \"seed\") }}{{ set message = name }}{{ message |> toUpperCase }}{{ /function }}\n";

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    paths: &RuntimePaths,
    host: &[u8],
    variable: &str,
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
    assert_eq!(document.files.len(), 2);
    for (path, raw) in [("original.vto", ORIGINAL), ("host.vto", host)] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "vento");
        assert_eq!(file.content_hash, content_hash(raw));
        assert_eq!(reader.file(file.id, &context).unwrap().as_ref(), Some(file));
        let source = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(source.data.chunks.len(), 1);
        assert_eq!(source.data.chunks[0].bytes, raw);
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
        let expected: BTreeSet<_> = document
            .occurrences
            .iter()
            .filter(|site| site.file == file.id)
            .map(|site| site.id)
            .collect();
        assert_eq!(
            cold.iter().map(|site| site.id).collect::<BTreeSet<_>>(),
            expected
        );
        let filters: Vec<_> = cold
            .iter()
            .filter(|site| site.syntax_kind == "vento.filter_name.reference")
            .collect();
        assert_eq!(filters.len(), 1);
        for site in filters {
            assert_eq!(site.role, OccurrenceRole::Reference);
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            let span = site.source.span();
            assert_eq!(site.source.content_hash(), content_hash(raw));
            assert_eq!(
                raw.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(b"toUpperCase".as_slice())
            );
        }
    }
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "greet")
        .unwrap();
    assert!(document.entities.iter().any(
        |entity| entity.kind == EntityKind::Function && entity.canonical_name == "toUpperCase"
    ));
    for (kind, name) in [
        (EntityKind::Parameter, "name"),
        (EntityKind::Variable, variable),
    ] {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == kind && entity.canonical_name == name)
            .collect();
        assert_eq!(entities.len(), 1);
        let entity = entities[0];
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(function.id))
        );
        assert_eq!(
            reader.entity(entity.id, &context).unwrap().as_ref(),
            Some(entity)
        );
        let contains: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(function.id)
                    && edge.object == RelationEndpoint::Entity(entity.id)
            })
            .collect();
        assert_eq!(contains.len(), 1);
        let evidence = contains[0].evidence.source.as_ref().unwrap();
        assert_eq!(evidence.content_hash(), content_hash(host));
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .count(),
        1
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vento-javascript-expression-binding-unavailable")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vento-filter-host-binding-unavailable")
    );
    function.id
}

#[test]
fn native_vento_survives_fresh_noop_changed_body_reopen_restore_and_cold_reads() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("original.vto"), ORIGINAL).unwrap();
    fs::write(repository.path().join("host.vto"), HOST).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let fresh = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let owner = assert_generation(&service, &fresh, &paths, HOST.as_bytes(), "message");
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        fresh.generation
    );
    let changed = HOST.replace("set message", "set changed");
    fs::write(repository.path().join("host.vto"), &changed).unwrap();
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
        assert_generation(&service, &updated, &paths, changed.as_bytes(), "changed"),
        owner
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_eq!(
        assert_generation(&reopened, &updated, &paths, changed.as_bytes(), "changed"),
        owner
    );
    fs::write(repository.path().join("host.vto"), HOST).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, fresh.logical_snapshot);
    assert_eq!(
        assert_generation(&reopened, &restored, &paths, HOST.as_bytes(), "message"),
        owner
    );
    drop(reopened);
    let mut cold =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_generation(&cold, &restored, &paths, HOST.as_bytes(), "message"),
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
fn vento_defaults_do_not_override_configuration_or_claim_nunjucks_filters() {
    let repository = durable_test_tempdir();
    fs::write(repository.path().join("original.VTO"), ORIGINAL).unwrap();
    fs::write(
        repository.path().join("pipe.vto"),
        b"{{ \"text\" | upper }}",
    )
    .unwrap();
    fs::write(
        repository.path().join("other.njk"),
        b"{{ \"text\" | upper }}",
    )
    .unwrap();
    fs::write(
        repository.path().join("override.vto"),
        b"def authored():\n    return 1\n",
    )
    .unwrap();
    let config = "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'override.vto'='python'\n";
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
        ("original.VTO", "vento"),
        ("pipe.vto", "vento"),
        ("override.vto", "python"),
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
    assert_ne!(
        document
            .files
            .iter()
            .find(|file| file.path == "other.njk")
            .unwrap()
            .language,
        "vento"
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.language == "python"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == "authored")
    );
    let filters: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vento.filter_name.reference")
        .collect();
    assert_eq!(filters.len(), 1);
    let file = document
        .files
        .iter()
        .find(|file| file.id == filters[0].file)
        .unwrap();
    assert_eq!(file.path, "original.VTO");
    assert!(matches!(
        filters[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}
