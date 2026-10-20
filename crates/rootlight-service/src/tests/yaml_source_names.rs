//! Written YAML lookup must remain useful while scalar identities preserve types.
//! Service queries, exact source reads and owner edges exercise the public path
//! independently from the native coverage auditor's expected-name derivation.

use super::*;
use rootlight_ir::{
    EntityKind, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
};

#[test]
fn yaml_written_property_lookup_retains_typed_identity_and_source_owner() {
    let source = b"---\nmetadata:\n  name: sample\nspec:\n  endpointSelector: {}\n";
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("config.yaml"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut selected = BTreeMap::new();
    for written in ["metadata", "name", "spec", "endpointSelector"] {
        let canonical = format!("str:\"{written}\"");
        let entity = document
            .entities
            .iter()
            .find(|row| {
                row.language == "yaml"
                    && row.kind == EntityKind::Property
                    && row.canonical_name == canonical
            })
            .unwrap();
        let located = service
            .code_locate(
                receipt.generation,
                written.to_owned(),
                LocateMode::Exact,
                25,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.path == "config.yaml" && hit.symbol == Some(entity.id)),
            "{written}: {:?}",
            located.data.hits
        );
        let explained = service
            .symbol_explain(receipt.generation, entity.id, &deadline())
            .unwrap()
            .data
            .entity;
        assert_eq!(explained.canonical_name, canonical);
        assert_eq!(explained.display_name, written);
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let reference = definitions[0].source.clone();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source));
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
        assert_eq!(read.data.chunks[0].bytes, written.as_bytes());
        selected.insert(written, entity.id);
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(selected["metadata"])
                    && edge.object == RelationEndpoint::Entity(selected["name"])
            })
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(selected["name"])
            && edge.object == RelationEndpoint::Entity(selected["metadata"])
    }));
}

#[test]
fn yaml_numeric_and_string_keys_keep_distinct_typed_lookup_and_source() {
    let source = b"1: numeric\n'1': string\n";
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("config.yaml"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let integer = document
        .entities
        .iter()
        .find(|row| {
            row.language == "yaml"
                && row.kind == EntityKind::Property
                && row.canonical_name == "int:1"
        })
        .unwrap();
    let string = document
        .entities
        .iter()
        .find(|row| {
            row.language == "yaml"
                && row.kind == EntityKind::Property
                && row.canonical_name == "str:\"1\""
        })
        .unwrap();
    assert_ne!(integer.id, string.id);
    for (entity, written) in [(integer, b"1".as_slice()), (string, b"'1'".as_slice())] {
        let located = service
            .code_locate(
                receipt.generation,
                entity.canonical_name.clone(),
                LocateMode::Exact,
                25,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.path == "config.yaml" && hit.symbol == Some(entity.id)),
            "{:?}",
            located.data.hits
        );
        assert!(
            located
                .data
                .hits
                .iter()
                .all(|hit| { hit.symbol.is_none() || hit.symbol == Some(entity.id) })
        );
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let reference = site.source.clone();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source));
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
        assert_eq!(read.data.chunks[0].bytes, written);
    }
}
