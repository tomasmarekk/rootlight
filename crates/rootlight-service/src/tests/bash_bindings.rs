//! Module parameter reads must survive identity-verified service indexing.
//! The exact written leaf and assignment-owner dependency are independent of
//! shell execution, expanded values, or a whole-project Bash semantic provider.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint, derive_relation_record_id};

#[test]
fn module_assignment_read_and_dependency_survive_merge_and_exact_query() {
    let directory = TempDir::new().unwrap();
    let source = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/bash_bindings/find_git_conflicts.sh"
    );
    fs::write(directory.path().join("module.sh"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut symbols = Vec::new();
    for name in ["prs", "REPO"] {
        let found = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        let hits: Vec<_> = found
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .collect();
        assert_eq!(hits.len(), 1, "{name}");
        assert_eq!(hits[0].path, "module.sh");
        let symbol = hits[0].symbol.unwrap();
        let entity = service
            .symbol_explain(receipt.generation, symbol, &deadline())
            .unwrap()
            .data
            .entity;
        assert_eq!(entity.kind, EntityKind::Variable);
        assert_eq!(entity.canonical_name, name);
        symbols.push(symbol);
    }
    let owner = symbols[0];
    let target = symbols[1];
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.enclosing == Some(owner)
                && site.target == (OccurrenceTarget::Resolved { symbol: target })
                && site.source.span().start_byte() == 248
                && site.source.span().end_byte() == 252
        })
        .collect();
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!((reads.len(), relations.len()), (1, 1));
    let relation = relations[0];
    assert_eq!(relation.evidence.source.as_ref(), Some(&reads[0].source));
    let mut proof = vec![
        FactRef::Entity(owner),
        FactRef::Fact(reads[0].id),
        FactRef::Entity(target),
    ];
    proof.sort_unstable();
    assert_eq!(relation.evidence.derivation, proof);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    let result = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reads[0].source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(result.data.chunks.len(), 1);
    assert_eq!(result.data.chunks[0].bytes, b"REPO");
    let unchanged = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    assert_eq!(unchanged.generation, receipt.generation);
}
