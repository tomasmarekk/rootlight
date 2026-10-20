//! Written Ada proof is separate from overload, unit and foreign ABI analysis.

use super::*;
use rootlight_ir::{FactRef, RelationEndpoint};

#[test]
fn ada_original_nested_import_has_exact_definitions_containment_and_call_proof() {
    let source =
        include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/ada/raylib.adb");
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("raylib.adb"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mut symbols = Vec::new();
    for (start, end, name, kind) in [
        (13, 19, "Raylib", EntityKind::Module),
        (38, 57, "Draw_Triangle_Strip", EntityKind::Function),
        (113, 134, "Draw_Triangle_Strip_C", EntityKind::Function),
        (58, 64, "Points", EntityKind::Parameter),
    ] {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "{name}");
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("definition must resolve");
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.language, "ada");
        assert_eq!(sites[0].source.repository(), receipt.repository);
        assert_eq!(sites[0].source.generation(), receipt.generation);
        assert_eq!(sites[0].source.content_hash(), content_hash(source));
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![sites[0].source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, name.as_bytes());
        symbols.push(symbol);
    }
    for (parent, child) in [
        (symbols[0], symbols[1]),
        (symbols[1], symbols[2]),
        (symbols[1], symbols[3]),
    ] {
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(parent)
                    && edge.object == RelationEndpoint::Entity(child))
        );
    }
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(symbols[1])
                && edge.object == RelationEndpoint::Entity(symbols[2])
        })
        .collect();
    assert_eq!(calls.len(), 1);
    let witness = calls[0].evidence.source.as_ref().unwrap();
    assert_eq!(witness.span().start_byte(), 332);
    assert_eq!(witness.span().end_byte(), 353);
    assert_eq!(witness.content_hash(), content_hash(source));
    let call_site = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source == *witness
                && site.enclosing == Some(symbols[1])
                && site.target == (OccurrenceTarget::Resolved { symbol: symbols[2] })
        })
        .unwrap();
    let proof: BTreeSet<_> = calls[0].evidence.derivation.iter().copied().collect();
    assert_eq!(calls[0].evidence.derivation.len(), 3);
    assert_eq!(
        proof,
        BTreeSet::from([
            FactRef::Entity(symbols[1]),
            FactRef::Fact(call_site.id),
            FactRef::Entity(symbols[2]),
        ])
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 332)
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(symbols[1])
                && edge.object == RelationEndpoint::Entity(symbols[0]))
    );
    assert!(
        document.skipped_regions.iter().any(
            |gap| gap.detail == "ada-unit-import-overload-type-and-runtime-binding-unavailable"
        )
    );
}

#[test]
fn ada_grouped_parameter_names_and_case_folded_nested_calls_keep_written_spelling() {
    let source = b"procedure Host (Left, Right : Integer) is\n procedure Child is begin null; end Child;\nbegin\n -- procedure Phantom is\n cHiLd;\nend Host;\n";
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("host.adb"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    for name in ["Left", "Right"] {
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.language == "ada")
                .count(),
            1
        );
    }
    assert!(!document.entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "Integer" | "Phantom" | "cHiLd"
    )));
    let host = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Host")
        .unwrap();
    let child = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Child")
        .unwrap();
    assert!(
        document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(host.id)
                && edge.object == RelationEndpoint::Entity(child.id))
    );
    let before = (host.id, child.id);
    let changed = String::from_utf8(source.to_vec())
        .unwrap()
        .replace("null;", "null; null;");
    fs::write(fixture.path().join("host.adb"), changed).unwrap();
    let after = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let updated = service
        .loaded_generation_snapshot(after.generation)
        .unwrap();
    for (name, id) in [("Host", before.0), ("Child", before.1)] {
        assert!(
            updated
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.id == id)
        );
    }
}

#[test]
fn ada_same_named_overloads_and_unproven_block_scopes_do_not_gain_call_edges() {
    for source in [
        "procedure Host is\n procedure Work (Value : Integer) is begin null; end Work;\n procedure WORK (Value : Boolean) is begin null; end WORK;\nbegin\n Work (1);\nend Host;\n",
        "procedure Host is\n procedure Child is begin null; end Child;\nbegin\n declare\n  procedure Child is begin null; end Child;\n begin\n  Child;\n end;\nend Host;\n",
    ] {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("host.adb"), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        assert!(
            !snapshot
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert!(snapshot.document().skipped_regions.iter().any(
            |gap| gap.detail == "ada-unit-import-overload-type-and-runtime-binding-unavailable"
        ));
    }
}
