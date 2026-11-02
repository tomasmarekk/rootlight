//! Native early-return reads retain written formal ownership through storage.
//! The complete original WAForth source also keeps later unsupported body work
//! outside the return-prefix certificate.

use super::*;

const ORIGINAL: &[u8] =
    include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/c_parameters/sieve.c");

fn assert_c_return(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    raw: &[u8],
    expected: bool,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let owner = document
        .entities
        .iter()
        .find(|entity| {
            entity.language == "c"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == "sieve"
        })
        .unwrap();
    let formal = document
        .entities
        .iter()
        .find(|entity| {
            entity.language == "c"
                && entity.kind == EntityKind::Parameter
                && entity.canonical_name == "n"
                && entity.container == Some(ContainerRef::Entity(owner.id))
        })
        .unwrap();
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 89
                && site.source.span().end_byte() == 90
        })
        .collect();
    assert_eq!(reads.len(), usize::from(expected));
    if let Some(read) = reads.first() {
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved { symbol: formal.id }
        );
        assert_eq!(read.enclosing, Some(owner.id));
        assert_eq!(read.source.content_hash(), content_hash(raw));
        assert_eq!(read.source.generation(), receipt.generation);
        assert_eq!(read.syntactic_text_hash, content_hash(b"n"));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Reads
                    && edge.subject == rootlight_ir::RelationEndpoint::Entity(owner.id)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(formal.id)
                    && edge.evidence.source.as_ref() == Some(&read.source))
                .count(),
            1
        );
    }
    let file = &document.files[0];
    assert_eq!(file.language, "c");
    assert_eq!(file.content_hash, content_hash(raw));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, raw);
}

#[test]
fn original_c_bare_return_survives_noop_changed_body_restore_and_reopen() {
    assert_eq!(ORIGINAL.len(), 508);
    assert_eq!(&ORIGINAL[89..90], b"n");
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::write(repository.path().join("sieve.c"), ORIGINAL).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository_with_mode(
            repository.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_c_return(&service, &initial, ORIGINAL, true);
    assert_eq!(
        service
            .index_repository_with_mode(
                repository.path(),
                FirstSliceIndexMode::Structural,
                &deadline()
            )
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("return n;", "return 0;")
        .into_bytes();
    fs::write(repository.path().join("sieve.c"), &changed).unwrap();
    let updated = service
        .index_repository_with_mode(
            repository.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_c_return(&service, &updated, &changed, false);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_c_return(&reopened, &updated, &changed, false);
    fs::write(repository.path().join("sieve.c"), ORIGINAL).unwrap();
    let restored = reopened
        .index_repository_with_mode(
            repository.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_c_return(&reopened, &restored, ORIGINAL, true);
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_c_return(&final_open, &restored, ORIGINAL, true);
    assert_eq!(
        final_open
            .index_repository_with_mode(
                repository.path(),
                FirstSliceIndexMode::Structural,
                &deadline()
            )
            .unwrap()
            .generation,
        restored.generation
    );
}
