//! Tcl formal declarations must survive indexing, lookup and exact source reads.
//! Parameter ownership is checked independently of identical spelling in the
//! procedure body, which must not create another definition.

use super::*;

#[test]
fn formal_parameters_retain_lookup_source_and_procedure_ownership() {
    let fixture = TempDir::new().unwrap();
    let source = b"proc paint {w {color red} args} {return $w}\n";
    fs::write(fixture.path().join("theme.tcl"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let procedure = document
        .entities
        .iter()
        .find(|entity| {
            entity.language == "tcl"
                && entity.kind == EntityKind::Function
                && entity.canonical_name == "paint"
        })
        .unwrap();

    let body_start = u64::try_from(
        source
            .windows(b"{return".len())
            .position(|part| part == b"{return")
            .unwrap(),
    )
    .unwrap();
    for (name, name_start, name_end) in [("w", 12, 13), ("color", 15, 20), ("args", 26, 30)] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        let hits: Vec<_> = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .collect();
        assert_eq!(hits.len(), 1, "missing or ambiguous Tcl parameter {name}");
        let hit = hits[0];
        assert_eq!(hit.path, "theme.tcl");
        assert_eq!(hit.language, "tcl");
        assert_eq!(hit.kind, "parameter");
        let parameter = service
            .symbol_explain(receipt.generation, hit.symbol.unwrap(), &deadline())
            .unwrap()
            .data
            .entity;
        assert_eq!(parameter.kind, EntityKind::Parameter);
        assert_eq!(
            parameter.container,
            Some(ContainerRef::Entity(procedure.id))
        );
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let definition_span = definitions[0].source.span();
        assert_eq!(definition_span.start_byte(), name_start);
        assert_eq!(definition_span.end_byte(), name_end);
        assert_eq!(
            source
                .get(usize::try_from(name_start).unwrap()..usize::try_from(name_end).unwrap())
                .unwrap(),
            name.as_bytes()
        );
        let reference = parameter.evidence.source.unwrap();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        let declaration_span = reference.span();
        assert_eq!(declaration_span.file(), definition_span.file());
        assert!(declaration_span.start_byte() <= name_start);
        assert!(declaration_span.end_byte() >= name_end);
        assert!(declaration_span.end_byte() <= body_start);
        let declaration = source
            .get(
                usize::try_from(declaration_span.start_byte()).unwrap()
                    ..usize::try_from(declaration_span.end_byte()).unwrap(),
            )
            .unwrap();
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
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].bytes, declaration);
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.predicate == RelationPredicate::Contains
                        && edge.subject == rootlight_ir::RelationEndpoint::Entity(procedure.id)
                        && edge.object == rootlight_ir::RelationEndpoint::Entity(parameter.id)
                })
                .count(),
            1
        );
    }

    let body_use = source.windows(2).position(|pair| pair == b"$w").unwrap() + 1;
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == u64::try_from(body_use).unwrap()
    }));
}
