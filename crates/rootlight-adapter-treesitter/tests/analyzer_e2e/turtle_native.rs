//! Turtle declarations retain written names and independently qualified IRIs.
//! Data assertions and inference remain outside this native declaration slice.
use super::*;

const TURTLE: LanguageCase = LanguageCase {
    name: "turtle",
    path: "src/written.ttl",
    frontend: "tree-sitter-turtle-0.2.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

#[path = "../../../rootlight-service/src/source_inventory_example/admission_profile.rs"]
mod product_profile;

fn output(source: &[u8]) -> AnalysisOutput {
    output_with_profile(source, false)
}

fn output_with_profile(source: &[u8], original: bool) -> AnalysisOutput {
    let provider = Arc::new(if original {
        TreeSitterProvider::new(product_profile::parser_config(MAX_SOURCE_BYTES).unwrap()).unwrap()
    } else {
        provider()
    });
    let fixture = Fixture::new(TURTLE, source);
    let budget = if original {
        product_profile::analysis_limits(MAX_SOURCE_BYTES).unwrap()
    } else {
        limits()
    };
    let result = analyze(
        &analyzer(&provider, TURTLE),
        &request(&fixture.snapshot, &fixture.source, TURTLE, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn turtle_named_prefixes_keep_rebindings_and_default_prefix_is_context_only() {
    let source = b"@prefix ex: <https://first.example/> .\nPREFIX ex: <https://second.example/>\n@prefix : <https://default.example/> .\nex:s ex:p ex:o .\n";
    let result = output(source);
    let doc = result.document();
    let aliases: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Namespace)
        .collect();
    assert_eq!(aliases.len(), 2);
    assert!(aliases.iter().all(|e| e.canonical_name == "ex"));
    assert_ne!(aliases[0].id, aliases[1].id);
    let qualified: BTreeSet<_> = aliases.iter().map(|e| e.qualified_name.as_str()).collect();
    assert_eq!(
        qualified,
        BTreeSet::from(["https://first.example/", "https://second.example/"])
    );
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|o| o.role == OccurrenceRole::Definition)
        .collect();
    assert_eq!(definitions.len(), 2);
    for occurrence in definitions {
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            b"ex"
        );
    }
    assert!(doc.skipped_regions.iter().any(|g| g.detail
        == "turtle-rdf-assertions-collections-relative-iri-and-resource-resolution-unavailable"));
}

#[test]
fn turtle_explicit_metatypes_keep_written_terms_and_proved_current_iris() {
    let source = br#"@prefix : <https://schema.example/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
:3DModel # subject comment
 a # object comment
 rdfs:Class .
:accelerationTime rdf:type rdf:Property .
<https://absolute.example/Class> a <http://www.w3.org/2000/01/rdf-schema#Class> .
:reason :uses :cos, :sin .
:collection :uses (:nested "rdfs:Class") .
@prefix : <https://second.example/> .
:3DModel a rdfs:Class .
@prefix : <relative> .
:Unproved a rdfs:Class .
@prefix rdfs: <https://wrong.example/> .
<https://absolute.example/Wrong> a rdfs:Class .
"#;
    let result = output(source);
    let resources: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|e| matches!(e.kind, EntityKind::Class | EntityKind::Property))
        .collect();
    assert_eq!(resources.len(), 4, "{resources:#?}");
    let written: BTreeSet<_> = resources
        .iter()
        .map(|e| (e.kind, e.canonical_name.as_str(), e.qualified_name.as_str()))
        .collect();
    assert_eq!(
        written,
        BTreeSet::from([
            (
                EntityKind::Class,
                ":3DModel",
                "https://schema.example/3DModel"
            ),
            (
                EntityKind::Property,
                ":accelerationTime",
                "https://schema.example/accelerationTime"
            ),
            (
                EntityKind::Class,
                "<https://absolute.example/Class>",
                "https://absolute.example/Class"
            ),
            (
                EntityKind::Class,
                ":3DModel",
                "https://second.example/3DModel"
            ),
        ])
    );
    for entity in resources {
        let definitions: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|o| {
                matches!(o.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)
                    && o.role == OccurrenceRole::Definition
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name.as_bytes()
        );
    }
}

#[test]
#[ignore = "requires explicitly supplied pinned original Turtle corpus roots"]
fn turtle_original_schema_and_prolog_witnesses_keep_exact_declarations_and_negatives() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_TURTLE_CORPUS").expect("explicit corpus parent"),
    );
    for (path, byte_length, expected, absent) in [
        (
            "182_Turtle_schemaorg/data/ext/auto/auto.ttl",
            14_449,
            vec![
                (42, 45, "rdf", EntityKind::Namespace),
                (103, 107, "rdfs", EntityKind::Namespace),
                (152, 163, ":BusOrCoach", EntityKind::Class),
                (2394, 2411, ":accelerationTime", EntityKind::Property),
            ],
            vec![(8, 9), (401, 409)],
        ),
        (
            "182_Turtle_schemaorg/data/schema.ttl",
            757_076,
            vec![(3327, 3335, ":3DModel", EntityKind::Class)],
            vec![(8, 9)],
        ),
        (
            "45_Prolog_eye/reasoning/euler-identity/euler-identity-output.ttl",
            489,
            vec![(52, 56, "math", EntityKind::Namespace)],
            vec![],
        ),
    ] {
        let source = fs::read(root.join(path)).unwrap();
        assert_eq!(source.len(), byte_length, "{path}");
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_turtle::LANGUAGE.into())
            .unwrap();
        let native = parser.parse(&source, None).unwrap();
        let native_nodes = native.root_node().descendant_count();
        eprintln!(
            "original {path}: {native_nodes} native nodes, root error={}",
            native.root_node().has_error()
        );
        let result = output_with_profile(&source, true);
        let doc = result.document();
        for (start, end, name, kind) in expected {
            assert_eq!(
                &source[start..end],
                name.as_bytes(),
                "independent written span {path}"
            );
            let definition = doc
                .occurrences
                .iter()
                .find(|o| {
                    o.role == OccurrenceRole::Definition
                        && o.source.span().start_byte() == u64::try_from(start).unwrap()
                        && o.source.span().end_byte() == u64::try_from(end).unwrap()
                })
                .unwrap_or_else(|| {
                    panic!(
                        "missing original definition {path}:{name}; entities={}, diagnostics={:?}",
                        doc.entities.len(),
                        doc.diagnostics
                    )
                });
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("definition unresolved")
            };
            let entity = doc.entities.iter().find(|e| e.id == symbol).unwrap();
            assert_eq!(
                (entity.kind, entity.canonical_name.as_str()),
                (kind, name),
                "{path}"
            );
        }
        for (start, end) in absent {
            assert!(
                !doc.occurrences
                    .iter()
                    .any(|o| o.role == OccurrenceRole::Definition
                        && o.source.span().start_byte() == start
                        && o.source.span().end_byte() == end),
                "negative {path}:{start}"
            );
        }
        if path.starts_with("45_") {
            assert!(
                !doc.entities
                    .iter()
                    .any(|e| matches!(e.kind, EntityKind::Class | EntityKind::Property)),
                "generic original RDF assertions must not become declarations"
            );
        }
    }
}
