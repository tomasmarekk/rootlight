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

fn trig_output(source: &[u8]) -> AnalysisOutput {
    let case = LanguageCase {
        name: "trig",
        path: "src/written.trig",
        frontend: "tree-sitter-trig-0.2.0",
        ..TURTLE
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source);
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &limits()),
        &ExtensionSupport::default(),
    );
    validate_ir_document(
        result.document(),
        limits().ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    result
}

#[test]
fn trig_native_graphs_keep_physical_owners_without_authored_label_definitions() {
    let source = b"@prefix ex: <https://example.test/> .\nGRAPH ex:g { ex:s ex:p ex:o . }\nex:g { ex:s ex:p ex:o }\n_:blank { }\n[] { }\n{ ex:s ex:p ex:o }\n{ }\n";
    let result = trig_output(source);
    let doc = result.document();
    assert!(
        !doc.diagnostics
            .iter()
            .any(|d| d.code == "syntax-error-recovery"),
        "{:?}",
        doc.diagnostics
    );
    let graphs: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Module && entity.canonical_name != "src/written.trig"
        })
        .collect();
    assert_eq!(graphs.len(), 6, "{:?}", doc.entities);
    assert_eq!(
        graphs
            .iter()
            .filter(|entity| entity.canonical_name == "ex:g")
            .count(),
        2
    );
    assert_eq!(
        graphs
            .iter()
            .filter(|entity| entity.canonical_name == "<default-graph>")
            .count(),
        2
    );
    assert_eq!(
        graphs
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        6
    );
    assert!(
        graphs
            .iter()
            .any(|entity| entity.canonical_name == "_:blank")
    );
    assert!(graphs.iter().any(|entity| entity.canonical_name == "[]"));
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
        .collect();
    assert_eq!(
        definitions.len(),
        1,
        "only the written prefix is a declaration"
    );
    assert_eq!(
        (
            definitions[0].source.span().start_byte(),
            definitions[0].source.span().end_byte()
        ),
        (8, 10)
    );
    assert!(graphs.iter().all(|graph| definitions.iter().all(|definition| !matches!(definition.target, OccurrenceTarget::Resolved { symbol } if symbol == graph.id))));
    let file_module = doc
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "src/written.trig")
        .unwrap();
    assert!(
        doc.relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::File(doc.files[0].id)
                && relation.object == RelationEndpoint::Entity(file_module.id))
    );
    for graph in &graphs {
        assert!(
            doc.relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(file_module.id)
                    && relation.object == RelationEndpoint::Entity(graph.id))
        );
    }
    assert_eq!(
        doc.relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Contains)
            .count(),
        8
    );
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("trig-rdf-assertions-"))
    );
}

#[test]
fn trig_recovered_graph_does_not_invent_a_clean_owner() {
    let result = trig_output(
        b"GRAPH <https://example.test/broken> { <https://example.test/s> <https://example.test/p> ",
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "<https://example.test/broken>")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition)
    );
}

#[test]
fn trig_nested_typed_resources_use_declared_metatype_iris_and_undotted_spans() {
    let source = br#"@prefix ex: <https://example.test/> .
@prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
graph ex:g { ex:C a rdfs:Class . ex:p rdf:type rdf:Property }
{ ex:D a rdfs:Class }
_:blank { ex:q a rdf:Property }
@prefix rdfs: <https://wrong.example/> .
GRAPH ex:wrong { ex:NotClass a rdfs:Class }
@prefix alias: <http://www.w3.org/2000/01/rdf-schema#> .
GrApH ex:alias { ex:Alias a alias:Class }
@prefix rdf: <https://wrong.example/> .
GRAPH ex:wrongType { ex:NotProperty rdf:type <http://www.w3.org/1999/02/22-rdf-syntax-ns#Property> }
GRAPH ex:ordinary { ex:Data a <https://example.test/UpdateData> }
"#;
    let result = trig_output(source);
    let doc = result.document();
    let resources: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| matches!(entity.kind, EntityKind::Class | EntityKind::Property))
        .collect();
    assert_eq!(resources.len(), 5, "{:#?}", doc.entities);
    let actual: BTreeSet<_> = resources
        .iter()
        .map(|entity| {
            (
                entity.kind,
                entity.canonical_name.as_str(),
                entity.qualified_name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        actual,
        BTreeSet::from([
            (EntityKind::Class, "ex:C", "https://example.test/C"),
            (EntityKind::Class, "ex:D", "https://example.test/D"),
            (EntityKind::Class, "ex:Alias", "https://example.test/Alias"),
            (EntityKind::Property, "ex:p", "https://example.test/p"),
            (EntityKind::Property, "ex:q", "https://example.test/q"),
        ])
    );
    for entity in resources {
        let definition = doc.occurrences.iter().find(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == entity.id)).unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name.as_bytes()
        );
        let graph = doc
            .entities
            .iter()
            .find(|graph| {
                graph.kind == EntityKind::Module
                    && graph.canonical_name != "src/written.trig"
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(graph.id))
            })
            .unwrap();
        assert!(
            doc.relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(graph.id)
                    && relation.object == RelationEndpoint::Entity(entity.id))
        );
        let owner = entity.evidence.source.as_ref().unwrap().span();
        assert!(owner.start_byte() <= span.start_byte() && owner.end_byte() > span.end_byte());
    }
    assert!(
        !doc.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
}

#[test]
#[ignore = "requires explicitly supplied pinned original TriG corpus root"]
fn trig_original_terminus_graph_retains_source_owners_without_data_as_declarations() {
    let root =
        Path::new(&std::env::var_os("ROOTLIGHT_TRIG_CORPUS").expect("explicit corpus parent"))
            .to_path_buf();
    let source = fs::read(root.join("185_TriG_terminusdb/tests/served/MW00KG01635.trig")).unwrap();
    assert_eq!(source.len(), 4223);
    let result = trig_output(&source);
    let doc = result.document();
    let graph = doc
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Module && entity.canonical_name == "bdg:MW00KG01635"
        })
        .unwrap();
    assert_eq!(
        doc.entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Module)
            .count(),
        2
    );
    assert!(
        !doc.entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Class | EntityKind::Property))
    );
    assert!(!doc.occurrences.iter().any(|occurrence| occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == graph.id)));
    assert!(
        doc.skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("trig-rdf-assertions-"))
    );
    assert!(
        !doc.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
}
