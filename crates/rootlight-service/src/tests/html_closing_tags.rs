//! Persisted HTML closing References retain the exact opening target and bytes.
//! Fresh indexing, unchanged indexing, an edit and cold reopen share the same
//! source-proof contract without claiming browser or embedded runtime semantics.

use super::*;

fn assert_reference(service: &FirstSliceService, receipt: &FirstSliceIndexReceipt, raw: &[u8]) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let closing = raw
        .windows(b"</head>".len())
        .position(|part| part == b"</head>")
        .unwrap() as u64
        + 2;
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 143
                && site.source.span().end_byte() == 147
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("opening head Definition")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::MarkupElement);
    assert_eq!(entity.canonical_name, "head");
    assert_eq!(entity.language, "html");
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == closing
                && site.source.span().end_byte() == closing + 4
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.syntax_kind, "html.closing_tag.reference");
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(site.enclosing, Some(symbol));
    assert_eq!(site.source.repository(), receipt.repository);
    assert_eq!(site.source.generation(), receipt.generation);
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
                && edge.object == rootlight_ir::RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&site.source))
            .count(),
        1
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![site.source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, site.source);
    assert_eq!(read.data.chunks[0].bytes, b"head");
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
#[test]
fn original_head_reference_survives_noop_edit_and_durable_reopen() {
    let raw = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/html_closing_tags/respond-proxy.html"
    );
    assert_eq!(raw.len(), 2587);
    assert_eq!(&raw[143..147], b"head");
    assert_eq!(&raw[215..219], b"head");
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let fixture = durable_test_tempdir();
    let live = fixture.path().join("proxy.html");
    fs::write(&live, raw).unwrap();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    let initial = service
        .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Structural, &deadline())
        .unwrap();
    assert_reference(&service, &initial, raw);
    let unchanged = service
        .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Structural, &deadline())
        .unwrap();
    assert_eq!(unchanged.generation, initial.generation);
    assert_reference(&service, &unchanged, raw);
    let changed = std::str::from_utf8(raw).unwrap().replacen(
        "<title>Respond JS Proxy</title>",
        "<title>Changed response proxy</title>",
        1,
    );
    assert_ne!(changed.as_bytes(), raw);
    fs::write(&live, changed.as_bytes()).unwrap();
    let next = service
        .index_repository_with_mode(fixture.path(), FirstSliceIndexMode::Structural, &deadline())
        .unwrap();
    assert_ne!(next.generation, initial.generation);
    assert_reference(&service, &next, changed.as_bytes());
    drop(service);
    fs::write(&live, b"<p>different live source</p>").unwrap();
    let reopened = FirstSliceService::new_durable(2, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(reopened.active_generation(), Some(next.generation));
    assert_reference(&reopened, &next, changed.as_bytes());
}
