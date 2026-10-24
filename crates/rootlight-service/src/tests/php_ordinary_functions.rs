//! Default service resolution preserves native ordinary PHP scope proofs.
//! Legacy call/name evidence and external uncertainty remain separate from added Calls.

use super::*;
use rootlight_ir::{RelationEndpoint, RelationPredicate};

const ORIGINAL: &[u8] = include_bytes!(
    "../../../rootlight-adapter-treesitter/tests/fixtures/php_ordinary_functions/geo_helpers.php"
);

#[test]
fn complete_original_php_formals_and_written_call_survive_real_service_resolution_and_noop() {
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("ordinary.php"), ORIGINAL).unwrap();
    let mut service = FirstSliceService::new(3).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    for _ in 0..2 {
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let file = document
            .files
            .iter()
            .find(|file| file.path == "ordinary.php")
            .unwrap();
        let parameter = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().file() == file.id
                    && site.source.span().start_byte() == 358
                    && site.source.span().end_byte() == 362
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = parameter.target else {
            panic!("written formal definition")
        };
        let formal = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(formal.kind, EntityKind::Parameter);
        let Some(rootlight_ir::ContainerRef::Entity(owner)) = formal.container else {
            panic!("function owns formal")
        };
        let reference = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().file() == file.id
                    && site.source.span().start_byte() == 440
                    && site.source.span().end_byte() == 444
            })
            .unwrap();
        assert_eq!(reference.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(reference.enclosing, Some(owner));
        assert_eq!(reference.provenance, file.provenance);
        assert_eq!(formal.provenance, file.provenance);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
        let calls: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
            })
            .collect();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].object, RelationEndpoint::Entity(owner));
        let source = calls[0].evidence.source.as_ref().unwrap();
        assert_eq!(source.span().start_byte(), 838);
        assert_eq!(source.span().end_byte(), 849);
        assert_eq!(calls[0].provenance, file.provenance);
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "php.name.reference"
                    && site.source.span() == source.span()
                    && site.role == OccurrenceRole::Reference)
        );
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "php.call.call"
                    && site.source.span().file() == file.id
                    && site.source.span().start_byte() == 838
                    && site.source.span().end_byte() == 869
                    && site.role == OccurrenceRole::CallSite)
        );
        for start in [399, 738] {
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| site.role == OccurrenceRole::CallSite
                        && site.source.span().file() == file.id
                        && site.source.span().start_byte() == start
                        && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            );
        }
        let noop = service
            .index_repository(directory.path(), &deadline())
            .unwrap();
        assert_eq!(noop, receipt);
    }
}

#[test]
fn rejected_ordinary_variable_sites_cannot_borrow_a_parameter_from_another_service_file() {
    let directory = TempDir::new().unwrap();
    fs::write(
        directory.path().join("good.php"),
        b"<?php function good($lat) { return $lat; }",
    )
    .unwrap();
    let bad = b"<?php function bad($lat) { $lat = 0; return $lat; }";
    fs::write(directory.path().join("bad.php"), bad).unwrap();
    let mut service = FirstSliceService::new(3).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let good = document
        .files
        .iter()
        .find(|file| file.path == "good.php")
        .unwrap();
    let bad_file = document
        .files
        .iter()
        .find(|file| file.path == "bad.php")
        .unwrap();
    assert!(document.entities.iter().any(|entity| {
        entity.kind == EntityKind::Parameter
            && entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == good.id)
    }));
    assert!(!document.entities.iter().any(|entity| {
        entity.kind == EntityKind::Parameter
            && entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == bad_file.id)
    }));
    let last = bad.windows(4).rposition(|bytes| bytes == b"$lat").unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().file() == bad_file.id
                && site.source.span().start_byte() == u64::try_from(last).unwrap()
        })
        .unwrap();
    assert_eq!(site.syntax_kind, "php.ordinary_unproved_variable.reference");
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
}
