//! Finite Modelica bindings survive identity-verified service merge and exact reads.
//! Malformed analysis never removes a captured immutable source from retrieval.

use super::*;

#[test]
fn original_inputs_contains_read_targets_and_literal_source_are_generation_bound() {
    let directory = TempDir::new().unwrap();
    let source =
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/modelica/U_sf.mo");
    let malformed = b"model Broken annotation(Documentation(info=\"\\</a>\")); end Broken;";
    fs::write(directory.path().join("U_sf.mo"), source).unwrap();
    fs::write(directory.path().join("malformed.mo"), malformed).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    for file in &document.files {
        assert_eq!(file.language, "modelica", "source route for {}", file.path);
    }
    let find = |name: &str, kind| {
        document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap()
            .id
    };
    let function = find("U_sf", EntityKind::Function);
    let unom = find("Unom", EntityKind::Parameter);
    let mdot = find("Mdot", EntityKind::Parameter);
    for (name, symbol) in [("Unom", unom), ("Mdot", mdot)] {
        let located = service
            .code_locate(
                receipt.generation,
                name.into(),
                LocateMode::Exact,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            located
                .data
                .hits
                .iter()
                .filter(|hit| hit.symbol == Some(symbol))
                .count(),
            1
        );
        assert_eq!(
            service
                .symbol_explain(receipt.generation, symbol, &deadline())
                .unwrap()
                .data
                .entity
                .kind,
            EntityKind::Parameter
        );
    }
    let relation = document
        .relations
        .iter()
        .find(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::Contains
                && edge.subject == rootlight_ir::RelationEndpoint::Entity(function)
                && edge.object == rootlight_ir::RelationEndpoint::Entity(unom)
        })
        .unwrap();
    let witness = relation.evidence.source.as_ref().unwrap();
    assert!(witness.span().start_byte() <= 115 && witness.span().end_byte() >= 119);
    for (offset, symbol) in [(178, unom), (183, mdot)] {
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Read
                    && site.enclosing == Some(function)
                    && site.target == (OccurrenceTarget::Resolved { symbol })
                    && site.source.span().start_byte() == offset
                    && site.source.span().end_byte() == offset + 4)
                .count(),
            1
        );
    }
    for (path, raw) in [
        ("U_sf.mo", source.as_slice()),
        ("malformed.mo", malformed.as_slice()),
    ] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, "modelica");
        assert_eq!(file.content_hash, content_hash(raw));
        let whole = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![whole.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].reference, whole);
        assert_eq!(read.data.chunks[0].bytes, raw);
        if path == "U_sf.mo" {
            for (query, count) in [
                ("U_sf", 2),
                (
                    "__rootlight_absent_8d64da9cabe975c9c6f9e58c456c38333678838a75761ed43fd23c6a72c9cf5f__",
                    0,
                ),
            ] {
                let found = service
                    .find_literal(
                        receipt.generation,
                        file.id,
                        query,
                        None,
                        rootlight_source::SourceFindBudget::new(),
                        &deadline(),
                    )
                    .unwrap();
                assert_eq!(found.stop, rootlight_source::SourceFindStop::EndOfSource);
                assert!(found.continuation.is_none());
                assert_eq!(found.matches.len(), count);
                for reference in found.matches {
                    let read = service
                        .source_read_with_options_and_budget(
                            receipt.generation,
                            vec![reference],
                            SourceReadOptions::new()
                                .with_context_lines_before(0)
                                .with_context_lines_after(0),
                            FirstSliceBudget::default(),
                            &deadline(),
                        )
                        .unwrap();
                    assert_eq!(read.data.chunks[0].bytes, query.as_bytes());
                }
            }
        }
    }
    assert!(!document.skipped_regions.is_empty());
}
