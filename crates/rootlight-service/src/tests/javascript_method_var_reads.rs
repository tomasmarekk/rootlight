//! Source-owned JavaScript reads must survive generation merge and query.
//! Reads with entity endpoints require a proven method owner, independently of RefersTo.

use super::*;
use rootlight_ir::{ContainerRef, FactRef, RelationEndpoint, derive_relation_record_id};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../rootlight-adapter-treesitter/tests/fixtures/javascript_method_var_reads/curve.babel.js"
);

#[test]
fn javascript_complete_curve_method_reads_local_var_with_exact_source_evidence() {
    assert_eq!(ORIGINAL.len(), 1480);
    assert_eq!(&ORIGINAL[511..518], b"radiusX");
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("curve.babel.js"), ORIGINAL).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let definition = |name: &str, kind, start, end| {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(definitions.len(), 1, "written {name} definition");
        let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
            panic!("written {name} definition resolves");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.language, "javascript");
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
        symbol
    };
    let owner = definition("_draw", EntityKind::Method, 311, 316);
    let target = definition("radiusX", EntityKind::Variable, 374, 381);
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.id == target)
        .unwrap();
    assert_eq!(variable.container, Some(ContainerRef::Entity(owner)));
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 511
                && site.source.span().end_byte() == 518
                && site.target == (OccurrenceTarget::Resolved { symbol: target })
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "the original method-local read survives service merge"
    );
    let site = sites[0];
    assert_eq!(site.enclosing, Some(owner));
    assert_eq!(site.source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(site.source.repository(), receipt.repository);
    assert_eq!(site.source.generation(), receipt.generation);
    assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Reads
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(target)
                && edge.evidence.source.as_ref() == Some(&site.source)
        })
        .collect();
    assert_eq!(
        edges.len(),
        1,
        "the authored Reads edge has method and variable entity endpoints"
    );
    let mut parents = [
        FactRef::Entity(owner),
        FactRef::Fact(site.id),
        FactRef::Entity(target),
    ];
    parents.sort_unstable();
    assert_eq!(edges[0].evidence.derivation, parents);
    assert_eq!(edges[0].id, derive_relation_record_id(edges[0]).unwrap());
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls
                || edge.object != RelationEndpoint::Entity(target))
    );
    let source = site.source.clone();
    let file = document
        .files
        .iter()
        .find(|file| file.id == source.span().file())
        .unwrap();
    assert_eq!(file.path, "curve.babel.js");
    let full = rootlight_ir::SourceRef::new(
        receipt.repository,
        receipt.generation,
        rootlight_ir::SourceSpan::new(file.id, 0, 1480).unwrap(),
        content_hash(ORIGINAL),
        None,
    );
    drop(snapshot);
    let result = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.data.chunks.len(), 1);
    assert_eq!(result.data.chunks[0].bytes, b"radiusX");
    let result = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![full],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.data.chunks.len(), 1);
    assert_eq!(result.data.chunks[0].bytes, ORIGINAL);
    let unchanged = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    assert_eq!(unchanged.generation, receipt.generation);
    let snapshot = service
        .loaded_generation_snapshot(unchanged.generation)
        .unwrap();
    assert_eq!(
        snapshot
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Reads
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(target)
                && edge
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().start_byte() == 511
                        && source.span().end_byte() == 518))
            .count(),
        1
    );
}
