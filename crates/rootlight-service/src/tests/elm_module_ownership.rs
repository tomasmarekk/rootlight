//! Explicit Elm modules own their written top-level declarations after merging.
//! Exact definition leaves and reads remain independent of the wider module
//! scope, whose body must not become part of the module's stable identity.

use super::*;
use rootlight_ir::RelationEndpoint;

#[test]
fn explicit_module_owns_original_asset_function_with_exact_definition() {
    let fixture = TempDir::new().unwrap();
    let source =
        include_bytes!("../../../rootlight-adapters/tests/fixtures/elm_module_ownership/Asset.elm");
    assert_eq!(source.len(), 764);
    fs::write(fixture.path().join("Asset.elm"), source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let module = document
        .entities
        .iter()
        .find(|row| row.kind == EntityKind::Module && row.canonical_name == "Asset")
        .unwrap();
    let located = service
        .code_locate(
            receipt.generation,
            "image".to_owned(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    let hits: Vec<_> = located
        .data
        .hits
        .iter()
        .filter(|row| row.symbol.is_some() && row.kind == "function" && row.identifier == "image")
        .collect();
    assert_eq!(hits.len(), 1);
    let image = service
        .symbol_explain(receipt.generation, hits[0].symbol.unwrap(), &deadline())
        .unwrap()
        .data
        .entity;
    assert_eq!(image.kind, EntityKind::Function);
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|row| {
            row.role == OccurrenceRole::Definition
                && row.target == (OccurrenceTarget::Resolved { symbol: image.id })
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let reference = definitions[0].source.clone();
    assert_eq!(reference.span().start_byte(), 609);
    assert_eq!(reference.span().end_byte(), 614);
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
    assert_eq!(read.data.chunks[0].bytes, b"image");
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(module.id)
                    && edge.object == RelationEndpoint::Entity(image.id)
            })
            .count(),
        1,
        "the explicit Asset module must own its written image function"
    );
    assert_eq!(image.container, Some(ContainerRef::Entity(module.id)));
}

#[test]
fn module_scope_preserves_nested_owners_and_identity_after_body_edit() {
    let fixture = TempDir::new().unwrap();
    let source = "{- Ω -}\r\nmodule Demo exposing (Choice(..), Model, render)\r\ntype Choice = One | Two\r\ntype alias Model = { value : Int }\r\nrender model = \"first\"\r\n";
    let path = fixture.path().join("Demo.elm");
    fs::write(&path, source).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let first = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(first.generation)
        .unwrap();
    let document = snapshot.document();
    let mut identities = BTreeMap::new();
    for name in ["Demo", "Choice", "One", "Two", "Model", "value", "render"] {
        let entity = document
            .entities
            .iter()
            .find(|row| row.canonical_name == name)
            .unwrap();
        identities.insert(name, entity.id);
        let definition = document
            .occurrences
            .iter()
            .find(|row| {
                row.role == OccurrenceRole::Definition
                    && row.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
    for (parent, child) in [
        ("Demo", "Choice"),
        ("Demo", "Model"),
        ("Demo", "render"),
        ("Choice", "One"),
        ("Choice", "Two"),
        ("Model", "value"),
    ] {
        let child_entity = document
            .entities
            .iter()
            .find(|row| row.id == identities[child])
            .unwrap();
        assert_eq!(
            child_entity.container,
            Some(ContainerRef::Entity(identities[parent]))
        );
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|row| {
                row.predicate == RelationPredicate::Contains
                    && row.subject == RelationEndpoint::Entity(identities[parent])
                    && row.object == RelationEndpoint::Entity(identities[child])
            })
            .collect();
        assert_eq!(edges.len(), 1, "{parent} must own {child}");
        assert_eq!(
            edges[0].id,
            rootlight_ir::derive_relation_record_id(edges[0]).unwrap()
        );
    }
    let changed = source.replace("first", "longer changed body");
    fs::write(&path, changed.as_bytes()).unwrap();
    let second = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(second.generation, first.generation);
    let changed_snapshot = service
        .loaded_generation_snapshot(second.generation)
        .unwrap();
    let changed_document = changed_snapshot.document();
    for (name, expected) in identities {
        let entity = changed_document
            .entities
            .iter()
            .find(|row| row.canonical_name == name)
            .unwrap();
        assert_eq!(entity.id, expected, "body edits must not redefine {name}");
        assert_eq!(entity.generation, second.generation);
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(changed.as_bytes())
        );
    }
}

#[test]
fn absent_duplicate_late_or_incomplete_module_never_owns_file_declarations() {
    let cases = [
        "render = 1\n",
        "-- module Ghost exposing (..)\nrender = 1\n",
        "module First exposing (..)\nmodule Second exposing (..)\nrender = 1\n",
        "import Html\nmodule Late exposing (..)\nrender = 1\n",
        "earlier = 0\nmodule Late exposing (..)\nrender = 1\n",
        "\"\"\"prefix\"\"\"\nmodule Late exposing (..)\nrender = 1\n",
        "module Incomplete\nrender = 1\n",
        "module Incomplete exposing (render\nrender = 1\n",
        "module Invalid exposing (..) trailing\nrender = 1\n",
        "module First exposing (..)\nearlier = 0\n  module Hidden exposing (..)\nrender = 1\n",
        "module Invalid \"masked literal\" exposing (..)\nrender = 1\n",
    ];
    for source in cases {
        let fixture = TempDir::new().unwrap();
        fs::write(fixture.path().join("Demo.elm"), source).unwrap();
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository(fixture.path(), &deadline())
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let render = document
            .entities
            .iter()
            .find(|row| row.kind == EntityKind::Function && row.canonical_name == "render")
            .unwrap();
        assert!(
            !document.entities.iter().any(|module| {
                module.kind == EntityKind::Module
                    && (render.container == Some(ContainerRef::Entity(module.id))
                        || document.relations.iter().any(|edge| {
                            edge.predicate == RelationPredicate::Contains
                                && edge.subject == RelationEndpoint::Entity(module.id)
                                && edge.object == RelationEndpoint::Entity(render.id)
                        }))
            }),
            "unproven file ownership: {source}"
        );
        let definition = document
            .occurrences
            .iter()
            .find(|row| {
                row.role == OccurrenceRole::Definition
                    && row.target == (OccurrenceTarget::Resolved { symbol: render.id })
            })
            .unwrap();
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![definition.source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, b"render");
    }
}
