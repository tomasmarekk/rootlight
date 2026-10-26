//! The structural service preserves a formal Reference from the complete Ruby original.
//! Exact source reads and repeated indexing exercise persisted physical authority;
//! this does not qualify dynamic method dispatch or template ownership.

use super::*;

#[test]
fn complete_original_ruby_formal_reference_retains_exact_source_and_method_owner() {
    let raw = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/ruby_formal_receivers/html_helper.rb"
    );
    assert_eq!(raw.len(), 1690);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("written.rb"), raw).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository_with_mode(
            directory.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 1389
                && site.source.span().end_byte() == 1390
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("exact formal Definition")
    };
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(parameter.kind, EntityKind::Parameter);
    assert_eq!(parameter.canonical_name, "e");
    let ContainerRef::Entity(method) = parameter.container.unwrap() else {
        panic!("method-owned formal")
    };
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.id == method)
        .unwrap();
    assert_eq!(owner.kind, EntityKind::Method);
    assert_eq!(owner.canonical_name, "whitelisted_tag?");
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1419
                && site.source.span().end_byte() == 1420
        })
        .collect();
    assert_eq!(references.len(), 1);
    let site = references[0];
    assert_eq!(site.syntax_kind, "ruby.formal_receiver.reference");
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(site.enclosing, Some(method));
    assert_eq!(site.source.content_hash(), content_hash(raw));
    assert_eq!(site.source.repository(), receipt.repository);
    assert_eq!(site.source.generation(), receipt.generation);
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
    assert_eq!(read.data.chunks[0].bytes, b"e");
    let next = service
        .index_repository_with_mode(
            directory.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_eq!(next.generation, receipt.generation);
}
