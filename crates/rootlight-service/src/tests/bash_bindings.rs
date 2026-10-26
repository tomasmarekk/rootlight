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

// The complete original must survive actual service generations and SourceRead.
// The fixture remains CRLF; temporary edits exercise provider requalification.

fn version_companions(
    document: &rootlight_ir::NormalizedIrDocument,
    raw: &[u8],
) -> Vec<rootlight_ir::SourceRef> {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 147
                && site.source.span().end_byte() == 150
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("written VER definition")
    };
    assert!(document.entities.iter().any(|entity| entity.id == symbol
        && entity.kind == EntityKind::Variable
        && entity.canonical_name == "VER"));
    let mut sources = Vec::new();
    for (start, end) in [(168, 171), (273, 276), (330, 333), (337, 340)] {
        let uses: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(uses.len(), 1, "exact original companion Reference");
        assert_eq!(uses[0].target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(
            uses[0].source.content_hash(),
            rootlight_ids::content_hash(raw)
        );
        sources.push(uses[0].source.clone());
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read
                && site.target == (OccurrenceTarget::Resolved { symbol }))
            .count(),
        2
    );
    sources
}

#[test]
fn complete_version_references_requalify_fresh_incremental_and_restored_source() {
    let original = include_bytes!(
        "../../../rootlight-adapter-treesitter/tests/fixtures/bash_module_references/tag_version.sh"
    );
    assert_eq!(original.len(), 343);
    assert_eq!(
        original.windows(2).filter(|pair| *pair == b"\r\n").count(),
        16
    );
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("module.sh");
    fs::write(&path, original).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let fresh = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(fresh.generation)
        .unwrap();
    let sources = version_companions(snapshot.document(), original);
    let read = service
        .source_read_with_options_and_budget(
            fresh.generation,
            sources,
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 4);
    assert!(read.data.chunks.iter().all(|chunk| chunk.bytes == b"VER"));
    let unchanged = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    assert_eq!(unchanged.generation, fresh.generation);
    let mut changed = original.to_vec();
    changed.extend_from_slice(b"# current source requalification\r\n");
    fs::write(&path, &changed).unwrap();
    let incremental = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(incremental.generation)
        .unwrap();
    version_companions(snapshot.document(), &changed);
    fs::write(&path, original).unwrap();
    let restored = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(restored.generation)
        .unwrap();
    version_companions(snapshot.document(), original);
    assert_eq!(fs::read(path).unwrap(), original);
}
