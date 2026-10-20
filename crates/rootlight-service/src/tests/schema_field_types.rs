//! Written schema field types must survive identity-verified service merging.
//! Adapter normalization and generation rebinding must retain the same canonical
//! relation recipe, exact type witness and independently queryable field edge.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint, derive_relation_record_id};

#[test]
fn thrift_field_type_relations_survive_identity_verified_merge() {
    check_field_type(
        "schema.thrift",
        "thrift",
        b"enum HealthState { OK = 1 }\nstruct Status { 1: HealthState state; }\n",
        "state",
        "HealthState",
        EntityKind::Enum,
    );
}

#[test]
fn capnp_field_type_relations_survive_identity_verified_merge() {
    check_field_type(
        "schema.capnp",
        "capnp",
        b"struct MotionObject {}\nstruct Telemetry { motionObject @0 :MotionObject; }\n",
        "motionObject",
        "MotionObject",
        EntityKind::Struct,
    );
}

#[test]
fn graphql_field_type_relations_survive_identity_verified_merge() {
    check_field_type(
        "schema.graphql",
        "graphql",
        b"type Viewer { id: ID! } type Query { viewer: Viewer }\n",
        "viewer",
        "Viewer",
        EntityKind::Struct,
    );
}

fn check_field_type(
    path: &str,
    language: &str,
    source: &[u8],
    field_name: &str,
    target_name: &str,
    target_kind: EntityKind,
) {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join(path), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .expect("canonical field type relations survive identity-verified merging");
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let field = document
        .entities
        .iter()
        .find(|row| {
            row.language == language
                && row.kind == EntityKind::Field
                && row.canonical_name == field_name
        })
        .unwrap();
    let target = document
        .entities
        .iter()
        .find(|row| {
            row.language == language && row.kind == target_kind && row.canonical_name == target_name
        })
        .unwrap();
    let occurrence = document
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::TypeUse
                && row.enclosing == Some(field.id)
                && row.target == (OccurrenceTarget::Resolved { symbol: target.id })
        })
        .unwrap();
    let span = occurrence.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        target_name.as_bytes()
    );
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|row| {
            row.predicate == RelationPredicate::UsesType
                && row.subject == RelationEndpoint::Entity(field.id)
                && row.object == RelationEndpoint::Entity(target.id)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    let relation = relations[0];
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    assert_eq!(relation.evidence.source.as_ref(), Some(&occurrence.source));
    assert_eq!(relation.provenance, occurrence.provenance);
    let mut derivation = [
        FactRef::Entity(field.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target.id),
    ];
    derivation.sort_unstable();
    assert_eq!(relation.evidence.derivation, derivation);
    assert!(document.relations.iter().any(|row| {
        row.predicate == RelationPredicate::UsesType
            && row.subject == RelationEndpoint::Occurrence(occurrence.id)
            && row.object == RelationEndpoint::Entity(target.id)
    }));

    let located = service
        .code_locate(
            receipt.generation,
            field_name.to_owned(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(located.data.hits.iter().any(|hit| {
        hit.symbol == Some(field.id) && hit.path == path && hit.language == language
    }));
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([field.id]),
            vec![RelationFamily::Types],
            Some(rootlight_query::RelationDirection::Outbound),
            0,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(relationships.data.groups.iter().any(|group| {
        group
            .items
            .iter()
            .any(|item| item.symbol == target.id && item.source_refs.contains(&occurrence.source))
    }));
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![occurrence.source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, target_name.as_bytes());
}
