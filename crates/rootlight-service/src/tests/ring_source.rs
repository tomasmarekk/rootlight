//! Ring written ownership must survive service identity verification and exact reads.
//! Unsupported owner shapes remain source-accessible with explicit analysis gaps.

use super::*;
use rootlight_ir::RelationEndpoint;
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

#[test]
fn original_ring_source_keeps_exact_definitions_contains_read_and_literal_eof() {
    let fixture = TempDir::new().unwrap();
    let original = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/ring/htmlfunctions.ring"
    );
    assert_eq!(original.len(), 1778);
    fs::write(fixture.path().join("htmlfunctions.ring"), original).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut targets = Vec::new();
    for (name, kind, start, end) in [
        ("HTMLFunctions", EntityKind::Class, 209, 222),
        ("HTMLSpecialChars", EntityKind::Method, 383, 399),
        ("cStr", EntityKind::Parameter, 400, 404),
    ] {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{name}");
        let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
            panic!("definition is resolved")
        };
        let explained = service
            .symbol_explain(receipt.generation, symbol, &deadline())
            .unwrap();
        assert_eq!(explained.data.entity.kind, kind);
        assert_eq!(explained.data.entity.canonical_name, name);
        assert_eq!(definitions[0].source.repository(), receipt.repository);
        assert_eq!(definitions[0].source.generation(), receipt.generation);
        assert_eq!(definitions[0].source.content_hash(), content_hash(original));
        targets.push(symbol);
    }
    for (owner, member) in [(targets[0], targets[1]), (targets[1], targets[2])] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(member))
                .count(),
            1
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(targets[2])
                && edge.object == RelationEndpoint::Entity(targets[1]))
    );
    let read = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 439
                && site.source.span().end_byte() == 443
        })
        .unwrap();
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved { symbol: targets[2] }
    );
    assert_eq!(read.enclosing, Some(targets[1]));
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span() == read.source.span())
    );
    let file = document
        .files
        .iter()
        .find(|file| file.path == "htmlfunctions.ring")
        .unwrap();
    assert_eq!(file.language, "ring");
    for (query, expected) in [
        ("HTMLFunctions", vec![(209, 222)]),
        ("RING_ABSENT_SENTINEL_271828", vec![]),
    ] {
        let found = service
            .find_literal(
                receipt.generation,
                file.id,
                query,
                None,
                SourceFindBudget::new(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(found.stop, SourceFindStop::EndOfSource);
        assert!(found.continuation.is_none());
        assert_eq!(
            found
                .matches
                .iter()
                .map(|reference| (reference.span().start_byte(), reference.span().end_byte()))
                .collect::<Vec<_>>(),
            expected
        );
        for reference in found.matches {
            let source = service
                .source_read_with_options_and_budget(
                    receipt.generation,
                    vec![reference.clone()],
                    SourceReadOptions::new()
                        .with_context_lines_before(0)
                        .with_context_lines_after(0)
                        .with_encoding(SourceEncoding::Bytes),
                    FirstSliceBudget::default(),
                    &deadline(),
                )
                .unwrap();
            assert_eq!(source.data.chunks[0].reference, reference);
            assert_eq!(source.data.chunks[0].bytes, query.as_bytes());
        }
    }
    let source = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![read.source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(source.data.chunks[0].bytes, b"cStr");
}

#[test]
fn ambiguous_class_ownership_remains_source_accessible_without_invented_owner_facts() {
    let fixture = TempDir::new().unwrap();
    let source = b"class First\nfunc first value { consume(value) }\nclass Second\nfunc second value { consume(value) }\n";
    fs::write(fixture.path().join("ambiguous.ring"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(!document.entities.iter().any(|entity| matches!(
        entity.kind,
        EntityKind::Class | EntityKind::Method | EntityKind::Parameter
    )));
    let file = document
        .files
        .iter()
        .find(|file| file.path == "ambiguous.ring")
        .unwrap();
    for domain in [
        rootlight_ir::FactDomain::Entities,
        rootlight_ir::FactDomain::Occurrences,
        rootlight_ir::FactDomain::Relations,
    ] {
        assert!(
            document.skipped_regions.iter().any(
                |gap| gap.domain == domain && gap.source.content_hash() == content_hash(source)
            )
        );
    }
    let reference = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, u64::try_from(source.len()).unwrap()).unwrap(),
        content_hash(source),
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
}
