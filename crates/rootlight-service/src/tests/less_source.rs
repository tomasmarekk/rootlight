//! Original Less declarations and direct mixin calls retain physical witnesses.
//! Exact source availability remains independent of bounded language analysis.

use super::*;
use rootlight_ir::{ContainerRef, FactDomain as IrFactDomain, FactRef, RelationEndpoint};

#[test]
fn original_less_mixins_parameters_contains_calls_and_source_are_exact() {
    let fixture = TempDir::new().unwrap();
    let original = include_bytes!("../../../rootlight-adapters/tests/fixtures/less/size.less");
    assert_eq!(original.len(), 139);
    fs::write(fixture.path().join("size.less"), original).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "size.less")
        .unwrap();
    assert_eq!(file.language, "less");
    assert_eq!(file.byte_length, 139);
    assert_eq!(file.content_hash, content_hash(original));
    let source = SourceRef::new(
        receipt.repository,
        receipt.generation,
        SourceSpan::new(file.id, 0, 139).unwrap(),
        file.content_hash,
        None,
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, source);
    assert_eq!(read.data.chunks[0].bytes.as_slice(), original.as_slice());
    let definition = |name: &str, kind, start, end| {
        let entities: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.language == "less"
                    && entity.kind == kind
                    && entity.canonical_name == name
                    && entity.evidence.source.as_ref().is_some_and(|source| {
                        source.repository() == receipt.repository
                            && source.generation() == receipt.generation
                            && source.span().file() == file.id
                            && source.content_hash() == file.content_hash
                    })
            })
            .collect();
        assert_eq!(entities.len(), 1, "{name} must have its authored kind");
        let entity = entities[0];
        assert_eq!(entity.display_name, name);
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.file == file.id
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(sites.len(), 1, "{name} exact physical definition");
        assert_eq!(sites[0].source.content_hash(), file.content_hash);
        entity.id
    };
    let size = definition(".size", EntityKind::Function, 23, 28);
    let square = definition(".square", EntityKind::Function, 92, 99);
    let width = definition("width", EntityKind::Parameter, 30, 35);
    let height = definition("height", EntityKind::Parameter, 38, 44);
    for (parameter, start, end) in [(width, 30, 35), (height, 38, 44)] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == parameter)
            .unwrap();
        assert_eq!(entity.container, Some(ContainerRef::Entity(size)));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(size)
                    && edge.object == RelationEndpoint::Entity(parameter)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        let witness = edges[0].evidence.source.as_ref().unwrap();
        assert_eq!(witness.repository(), receipt.repository);
        assert_eq!(witness.generation(), receipt.generation);
        assert_eq!(witness.span().file(), file.id);
        assert_eq!(witness.content_hash(), file.content_hash);
        assert!(witness.span().start_byte() <= start && witness.span().end_byte() >= end);
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(parameter)
                    && edge.object == RelationEndpoint::Entity(size))
        );
    }
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(square)
                && edge.object == RelationEndpoint::Entity(size)
        })
        .collect();
    assert_eq!(calls.len(), 1, "source-authored square includes size");
    let witness = calls[0].evidence.source.as_ref().unwrap();
    assert_eq!(witness.repository(), receipt.repository);
    assert_eq!(witness.generation(), receipt.generation);
    assert_eq!(witness.span().file(), file.id);
    assert_eq!(witness.content_hash(), file.content_hash);
    assert!(witness.span().start_byte() <= 112 && witness.span().end_byte() >= 117);
    assert!(
        calls[0]
            .evidence
            .derivation
            .contains(&FactRef::Entity(square))
    );
    assert!(
        calls[0]
            .evidence
            .derivation
            .contains(&FactRef::Entity(size))
    );
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && site.enclosing == Some(square)
                && site.source == *witness
                && site.target == (OccurrenceTarget::Resolved { symbol: size })
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert!(
        calls[0]
            .evidence
            .derivation
            .contains(&FactRef::Fact(sites[0].id))
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.file == file.id
                && site.source.span().start_byte() == 112
                && site.source.span().end_byte() == 117)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == IrFactDomain::Relations
                && gap.detail.contains("less-")
                && gap.source.content_hash() == file.content_hash)
    );
}
