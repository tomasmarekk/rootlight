//! Durable VB.NET default admission preserves original declaration evidence.
//! Full-source retention and lifecycle controls do not qualify body semantics.

use super::*;
use rootlight_storage::{
    GenerationBudget, GenerationContext, GenerationReadLimit, GenerationReader,
    OccurrenceReadRequest,
};

const ORIGINAL: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/vbnet/TaskbarAnimate.vb");

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
    assert_eq!(file.language, "vbnet");
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
    assert_eq!(class.canonical_name, "taskbaranimate");
    assert_eq!(class.display_name, "TaskbarAnimate");
    assert_eq!(method.canonical_name, "animate");
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
    for (entity, start, end) in [(class, 36, 50), (method, 122, 129), (formal, 136, 140)] {
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
