//! Structural service reads must retain exact native formal bindings.
//! This regression does not qualify the separate project-analyzer Deep route.

use super::*;

#[test]
fn original_python_return_references_keep_formal_identity_and_source_reads() {
    let raw = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/python_parameter_binding/test.py"
    );
    assert_eq!(raw.len(), 4073);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("written.py"), raw).unwrap();
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
    let owner = document
        .entities
        .iter()
        .find(|entity| {
            entity.language == "python"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == "add_numbers"
        })
        .unwrap();
    for (name, declaration, reference) in [("a", 3727_u64, 3899_u64), ("b", 3735, 3903)] {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == declaration
                    && site.source.span().end_byte() == declaration + 1
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
            panic!("formal Definition must resolve")
        };
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(parameter.kind, EntityKind::Parameter);
        assert_eq!(parameter.canonical_name, name);
        assert_eq!(parameter.container, Some(ContainerRef::Entity(owner.id)));
        let uses: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == reference
                    && site.source.span().end_byte() == reference + 1
            })
            .collect();
        assert_eq!(uses.len(), 1);
        let site = uses[0];
        assert_eq!(site.enclosing, Some(owner.id));
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
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
        assert_eq!(read.data.chunks[0].bytes, name.as_bytes());
    }
    let repeated = service
        .index_repository_with_mode(
            directory.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_eq!(repeated.generation, receipt.generation);
}
