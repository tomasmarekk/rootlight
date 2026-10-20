//! Native module-call proofs must survive strict generation identity verification.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint, derive_relation_record_id};

#[test]
fn typescript_module_call_survives_merge_query_and_exact_source_read() {
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/typescript_calls/vue-util.ts"
    );
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("util.ts"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let function = |name: &str| {
        document
            .entities
            .iter()
            .find(|row| {
                row.language == "typescript"
                    && row.kind == EntityKind::Function
                    && row.canonical_name == name
            })
            .unwrap()
    };
    let owner = function("isPromise");
    let target = function("isDef");
    let occurrence = document
        .occurrences
        .iter()
        .find(|row| {
            row.role == OccurrenceRole::CallSite
                && row.source.span().start_byte() == 2018
                && row.enclosing == Some(owner.id)
                && row.target == (OccurrenceTarget::Resolved { symbol: target.id })
        })
        .expect("the exact written call remains resolved after identity-verified merge");
    let relation = document
        .relations
        .iter()
        .find(|row| {
            row.predicate == RelationPredicate::Calls
                && row.subject == RelationEndpoint::Entity(owner.id)
                && row.object == RelationEndpoint::Entity(target.id)
                && row.evidence.source.as_ref() == Some(&occurrence.source)
        })
        .unwrap();
    let mut parents = [
        FactRef::Entity(owner.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(target.id),
    ];
    parents.sort_unstable();
    assert_eq!(relation.evidence.derivation, parents);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([owner.id]),
            vec![RelationFamily::Calls],
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
            .any(|row| row.symbol == target.id && row.source_refs.contains(&occurrence.source))
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
    assert_eq!(read.data.chunks[0].bytes, b"isDef");
}
