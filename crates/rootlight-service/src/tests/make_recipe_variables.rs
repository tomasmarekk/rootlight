//! Literal Make recipe variable uses retain source-owned lexical references.
//! Rule ownership and definition leaves are independent of shell execution,
//! expanded values and ambiguous assignments, which remain bounded analysis.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint};

#[test]
fn make_recipe_variable_uses_have_exact_source_and_rule_owner() {
    let source = b"FRONTEND := frontend\nSTAMP := stamp\nbuild: $(STAMP)\n\tnpm --prefix $(FRONTEND) run build\nother:\n\t@echo $$(FRONTEND) $(PREFIX_$(KEY)) $(shell echo BAD) ${FRONTEND}\n";
    let fixture = TempDir::new().unwrap();
    let path = fixture.path().join("Makefile");
    fs::write(&path, source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let frontend = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "FRONTEND")
        .unwrap();
    let build = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "build")
        .unwrap();
    let other = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "other")
        .unwrap();
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "make.variable.reference"
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: frontend.id,
                    })
        })
        .collect();
    assert_eq!(references.len(), 2);
    for owner in [build, other] {
        let owned: Vec<_> = references
            .iter()
            .copied()
            .filter(|site| site.enclosing == Some(owner.id))
            .collect();
        assert_eq!(owned.len(), 1);
        let reference = owned[0].source.clone();
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source));
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Entity(owner.id)
                    && edge.object == RelationEndpoint::Entity(frontend.id)
            })
            .collect();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&reference));
        for proof in [
            FactRef::Entity(owner.id),
            FactRef::Fact(owned[0].id),
            FactRef::Entity(frontend.id),
        ] {
            assert!(edges[0].evidence.derivation.contains(&proof));
        }
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
        assert_eq!(read.data.chunks[0].bytes, b"FRONTEND");
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "make-recipe-not-analyzed" })
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| { matches!(entity.canonical_name.as_str(), "PREFIX_" | "KEY" | "BAD") })
    );
    let before_build = build.id;
    let before_frontend = frontend.id;
    let mut changed = source.to_vec();
    changed.extend_from_slice(b"\t@echo another shell command\n");
    fs::write(&path, changed).unwrap();
    let updated = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, receipt.generation);
    let updated_snapshot = service
        .loaded_generation_snapshot(updated.generation)
        .unwrap();
    for (name, id) in [("build", before_build), ("FRONTEND", before_frontend)] {
        assert!(
            updated_snapshot
                .document()
                .entities
                .iter()
                .any(|entity| { entity.canonical_name == name && entity.id == id })
        );
    }
}

#[test]
fn make_ambiguous_variable_assignments_do_not_gain_resolved_recipe_edges() {
    let source = b"APP := first\nAPP += second\nbuild:\n\t@echo $(APP)\n";
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("Makefile"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let build = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::BuildTarget && entity.canonical_name == "build")
        .unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.syntax_kind == "make.variable.reference"
            && !matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Entity(build.id)
    }));
}
