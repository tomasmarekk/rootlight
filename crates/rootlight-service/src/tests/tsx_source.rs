//! Preserve authored TSX markup definitions separately from lexical bindings.
//! This original-source control requires native ownership and exact readback;
//! it makes no React runtime, DOM construction, or component dispatch claim.

use super::*;
use rootlight_ir::{ContainerRef, RelationEndpoint};

#[test]
fn original_tsx_markup_definitions_and_ownership_keep_exact_source() {
    let raw = include_bytes!("../../tests/fixtures/tsx/skeleton.tsx");
    assert_eq!(raw.len(), 300);
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("view.tsx"), raw).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "view.tsx")
        .unwrap();
    assert_eq!(file.language, "typescript");
    assert_eq!(file.byte_length, 300);
    assert_eq!(file.content_hash, content_hash(raw));
    let definition = |name: &str, kind, start, end| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "typescript"
                    && entity.kind == kind
                    && entity.canonical_name == name
                    && entity
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        assert_eq!(entities.len(), 1, "authored {kind:?}:{name}");
        let entity = entities[0];
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
                    && site.source.span().file() == file.id
            })
            .collect();
        assert_eq!(
            definitions.len(),
            1,
            "exact definition {start}..{end}:{name}"
        );
        let source = &definitions[0].source;
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.content_hash(), file.content_hash);
        assert_eq!(
            &raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            name.as_bytes()
        );
        entity.id
    };
    let function = definition("Skeleton", EntityKind::Function, 65, 73);
    let parameter = definition("props", EntityKind::Parameter, 90, 95);
    let element = definition("div", EntityKind::MarkupElement, 148, 151);
    let attribute = definition("data-slot", EntityKind::MarkupAttribute, 159, 168);
    for (subject, object, start, end) in [
        (function, parameter, 90, 95),
        (function, element, 148, 151),
        (element, attribute, 159, 168),
    ] {
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(subject)
                    && edge.object == RelationEndpoint::Entity(object)
                    && edge.evidence.source.as_ref().is_some_and(|source| {
                        source.span().file() == file.id
                            && source.span().start_byte() <= start
                            && source.span().end_byte() >= end
                    })
            })
            .collect();
        assert_eq!(edges.len(), 1, "typed authored Contains {start}..{end}");
        let source = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(source.repository(), receipt.repository);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.content_hash(), file.content_hash);
        assert_eq!(
            document
                .entities
                .iter()
                .find(|entity| entity.id == object)
                .unwrap()
                .container,
            Some(ContainerRef::Entity(subject))
        );
    }
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().file() == file.id
            && site.source.span().start_byte() == 253
            && site.source.span().end_byte() == 258
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(parameter)
            && edge.object == RelationEndpoint::Entity(function)
    }));
    let whole = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, 300).unwrap(),
        file.content_hash,
        None,
    );
    let exact = service
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
    assert_eq!(exact.data.chunks.len(), 1);
    assert_eq!(exact.data.chunks[0].reference, whole);
    assert_eq!(exact.data.chunks[0].bytes, raw);
    for (query, expected) in [
        ("data-slot", 1),
        ("data-slot__rootlight_literal_absent__", 0),
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
        assert_eq!(found.matches.len(), expected);
        for reference in found.matches {
            assert_eq!(reference.span().start_byte(), 159);
            assert_eq!(reference.span().end_byte(), 168);
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), file.content_hash);
            let exact = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(exact.data.chunks.len(), 1);
            assert_eq!(exact.data.chunks[0].reference, reference);
            assert_eq!(exact.data.chunks[0].bytes, query.as_bytes());
        }
    }
}
