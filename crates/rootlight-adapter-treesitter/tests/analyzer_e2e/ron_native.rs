//! Native RON data properties preserve source ownership without Rust semantics.
//! Original constructor spellings never create callable bindings.

use super::*;

const RON: LanguageCase = LanguageCase {
    name: "ron",
    path: "assets/written.ron",
    frontend: "tree-sitter-ron-0.2.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(RON, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, RON),
        &request(&fixture.snapshot, &fixture.source, RON, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn original_simple_data_has_source_owned_properties_without_callable_ghosts() {
    let source = include_bytes!("../fixtures/ron/simple_data.ron");
    assert_eq!(source.len(), 207);
    let result = output(source);
    let doc = result.document();
    let module = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Module && e.language == "ron")
        .expect("native RON owns its module");
    let properties: Vec<_> = doc
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 7, "{:?}", doc.entities);
    for entity in &properties {
        assert_eq!(entity.language, "ron");
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let bytes = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(definitions[0].syntactic_text_hash, content_hash(bytes));
        assert!(
            doc.relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Contains
                    && r.object == RelationEndpoint::Entity(entity.id))
        );
    }
    let root_property = properties
        .iter()
        .find(|e| e.canonical_name == "\"MyDataAsset\"")
        .unwrap();
    assert_eq!(
        root_property.id.to_string(),
        "sym1_33dzzpwe7xisb5vvy42gj762ozcltsoyint4yma"
    );
    assert_eq!(root_property.canonical_name, "\"MyDataAsset\"");
    assert_eq!(
        root_property.display_name, "MyDataAsset",
        "canonical:{} id:{}",
        root_property.canonical_name, root_property.id
    );
    assert_eq!(
        root_property.container,
        Some(rootlight_ir::ContainerRef::Entity(module.id))
    );
    let color = properties
        .iter()
        .find(|e| e.canonical_name == "color")
        .unwrap();
    let red = properties
        .iter()
        .find(|e| e.canonical_name == "red")
        .unwrap();
    assert_eq!(
        red.container,
        Some(rootlight_ir::ContainerRef::Entity(color.id))
    );
    assert!(
        doc.occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::StringEvidence)
    );
    assert!(
        !doc.occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::CallSite)
    );
    assert!(!doc.entities.iter().any(|e| matches!(
        e.kind,
        EntityKind::Function | EntityKind::Method | EntityKind::Struct
    )));
    assert!(!doc.skipped_regions.iter().any(|s| matches!(
        s.reason,
        SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
    )));
}

#[test]
fn original_animation_graph_keeps_repeated_properties_and_value_edit_identity() {
    let source = include_bytes!("../fixtures/ron/Fox.animgraph.ron");
    assert_eq!(source.len(), 1026);
    let provider = Arc::new(provider());
    let fixture = Fixture::new(RON, source);
    let budget = limits();
    let analyzer = analyzer(&provider, RON);
    let initial = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, RON, &budget),
        &ExtensionSupport::default(),
    );
    let doc = initial.document();
    validate_ir_document(doc, budget.ir(), &ExtensionSupport::default()).unwrap();
    let nodes = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Property && e.canonical_name == "nodes")
        .unwrap();
    for name in ["node_type", "mask", "weight"] {
        let properties: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Property && e.canonical_name == name)
            .collect();
        assert_eq!(properties.len(), 5, "{name}: {:?}", doc.entities);
        assert_eq!(
            properties
                .iter()
                .map(|e| e.id)
                .collect::<BTreeSet<_>>()
                .len(),
            5
        );
        for property in properties {
            assert_eq!(
                property.container,
                Some(rootlight_ir::ContainerRef::Entity(nodes.id))
            );
            let definitions: Vec<_> = doc
                .occurrences
                .iter()
                .filter(|o| {
                    o.role == OccurrenceRole::Definition
                        && o.target
                            == OccurrenceTarget::Resolved {
                                symbol: property.id,
                            }
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            let span = definitions[0].source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name.as_bytes()
            );
        }
    }
    assert!(
        !doc.entities
            .iter()
            .any(|e| matches!(e.kind, EntityKind::Function | EntityKind::Struct))
    );
    assert!(
        !doc.occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::CallSite)
    );
    let edited = std::str::from_utf8(source)
        .unwrap()
        .replacen("weight: 1.0", "weight: 2.0", 1);
    let changed = fixture.rewrite(edited.as_bytes());
    let updated = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, RON, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(symbol_ids(doc), symbol_ids(updated.document()));
}

#[test]
fn mixed_data_positions_count_scalars_and_tuples_but_exclude_comment_extras() {
    let source = "[None, (field: 1), Wrap((field: 2)), (), (field: 3)]";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(RON, source.as_bytes());
    let budget = limits();
    let analyzer = analyzer(&provider, RON);
    let analyze_fixture = |fixture: &Fixture| {
        analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, RON, &budget),
            &ExtensionSupport::default(),
        )
    };
    let initial = analyze_fixture(&fixture);
    let fields = |doc: &rootlight_ir::NormalizedIrDocument| {
        doc.entities
            .iter()
            .filter(|e| e.kind == EntityKind::Property && e.canonical_name == "field")
            .map(|e| e.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(fields(initial.document()).len(), 3);
    let comment = source.replace("(field: 1)", "/* position-neutral */ (field: 4)");
    let changed = fixture.rewrite(comment.as_bytes());
    let updated = analyze_fixture(&changed);
    assert_eq!(
        symbol_ids(initial.document()),
        symbol_ids(updated.document())
    );
    let removed_scalar = fixture.rewrite(source.replacen("None, ", "", 1).as_bytes());
    let shifted = analyze_fixture(&removed_scalar);
    assert_eq!(fields(shifted.document()).len(), 3);
    assert!(fields(initial.document()).is_disjoint(&fields(shifted.document())));
    let module = initial
        .document()
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Module)
        .unwrap();
    for field in initial
        .document()
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::Property)
    {
        assert_eq!(
            field.container,
            Some(rootlight_ir::ContainerRef::Entity(module.id))
        );
        assert!(
            initial
                .document()
                .relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Contains
                    && r.subject == RelationEndpoint::Entity(module.id)
                    && r.object == RelationEndpoint::Entity(field.id))
        );
    }
    assert!(
        !initial
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::CallSite)
    );
    assert!(
        !initial
            .document()
            .entities
            .iter()
            .any(|e| matches!(e.kind, EntityKind::Function | EntityKind::Struct))
    );
}

#[test]
fn unsupported_data_keys_and_recovery_keep_explicit_gaps_without_invented_bindings() {
    for source in [
        b"{1: Some(2), (3, 4): Clip(5)}".as_slice(),
        br#"{"escaped\nkey": 1}"#.as_slice(),
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|e| e.kind == EntityKind::Property)
        );
        assert!(!result.document().occurrences.iter().any(|o| matches!(
            o.role,
            OccurrenceRole::Definition | OccurrenceRole::CallSite
        )));
        assert!(!result.document().skipped_regions.is_empty());
    }
    for source in [
        b"(field: ".as_slice(),
        b"#![enable(implicit_some)]\n(field: 1)".as_slice(),
        br##"r#"unsupported raw value"#"##.as_slice(),
    ] {
        let result = output(source);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|s| s.reason == SkippedRegionReason::ParseError)
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|e| matches!(e.kind, EntityKind::Function | EntityKind::Struct))
        );
    }
}

#[test]
fn ron_fence_remains_explicitly_unavailable_instead_of_borrowing_host_language() {
    let source = b"# Data\n\n```ron\n(field: Some(1))\n```\n";
    let case = LanguageCase {
        name: "markdown",
        path: "docs/data.md",
        frontend: "tree-sitter-md-0.5.3",
        ..RON
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|e| e.language == "ron"
                || matches!(
                    e.kind,
                    EntityKind::Property | EntityKind::Function | EntityKind::Struct
                ))
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::CallSite)
    );
    for definition in result
        .document()
        .occurrences
        .iter()
        .filter(|o| o.role == OccurrenceRole::Definition)
    {
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("host definitions retain their explicit owner");
        };
        let owner = result
            .document()
            .entities
            .iter()
            .find(|e| e.id == symbol)
            .unwrap();
        assert_eq!(owner.language, "markdown");
        assert!(!matches!(
            owner.kind,
            EntityKind::Property | EntityKind::Function | EntityKind::Struct
        ));
    }
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|s| s.reason == SkippedRegionReason::UnsupportedConstruct)
    );
}

#[test]
fn utf16_property_definitions_keep_exact_physical_source_spans() {
    let source = "/* λ😀 */\r\n(field: \"value\")\r\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(RON, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(RON),
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, RON),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let property = result
            .document()
            .entities
            .iter()
            .find(|e| e.kind == EntityKind::Property && e.canonical_name == "field")
            .unwrap();
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|o| {
                o.role == OccurrenceRole::Definition
                    && o.target
                        == OccurrenceTarget::Resolved {
                            symbol: property.id,
                        }
            })
            .unwrap();
        let start = 2 + 2 * source[..source.find("field").unwrap()]
            .encode_utf16()
            .count();
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(start + 10).unwrap()
        );
        assert_eq!(definition.source.content_hash(), content_hash(&raw));
        assert_eq!(definition.syntactic_text_hash, content_hash(b"field"));
    }
}

#[test]
fn ron_artifact_boundary_preserves_every_scalar_position_or_refuses() {
    let source = b"[None, (field: 1), Wrap((field: 2)), (), (field: 3)]";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(RON, source);
    let budget = limits();
    let analyzer = analyzer(&provider, RON);
    let initial = request(&fixture.snapshot, &fixture.source, RON, &budget);
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap()
    );
    let changed = fixture.next_generation();
    let exact = limits_with_syntax_records(required);
    let bounded = request(&changed.snapshot, &changed.source, RON, &exact);
    let (_, bounded_artifact) = analyzer
        .analyze_and_capture(
            &bounded,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let reused = analyzer
        .analyze_from_artifact(
            &bounded,
            &bounded_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &bounded, &ExtensionSupport::default());
    assert_eq!(reused.document(), fresh.document());
    assert_eq!(
        reused
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Property)
            .count(),
        3
    );
    let too_small = limits_with_syntax_records(required - 1);
    assert!(
        analyzer
            .analyze_and_capture(
                &request(&changed.snapshot, &changed.source, RON, &too_small),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err(),
        "scalar census cannot be dropped to fit a limit"
    );
}

fn native_entry_census(source: &[u8]) -> BTreeMap<(u64, u64), Option<(u64, u64)>> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_ron::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(!tree.root_node().has_error());
    assert_eq!(tree.root_node().byte_range(), 0..source.len());
    let key = |node: tree_sitter::Node<'_>| {
        if !matches!(node.kind(), "struct_entry" | "map_entry") {
            return None;
        }
        let key = node.named_child(0)?;
        if !matches!(key.kind(), "identifier" | "string") {
            return None;
        }
        let text = &source[key.byte_range()];
        assert!(
            !text.contains(&b'\\'),
            "original plain-key census requires no escape evaluation"
        );
        Some((
            u64::try_from(key.start_byte()).unwrap(),
            u64::try_from(key.end_byte()).unwrap(),
        ))
    };
    let mut entries = BTreeMap::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        assert!(!node.is_error() && !node.is_missing());
        if let Some(span) = key(node) {
            let mut ancestor = node.parent();
            let mut owner = None;
            while let Some(parent) = ancestor {
                if let Some(span) = key(parent) {
                    owner = Some(span);
                    break;
                }
                ancestor = parent.parent();
            }
            assert!(entries.insert(span, owner).is_none());
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return entries;
            }
        }
    }
}

#[test]
#[ignore = "requires the explicitly supplied immutable original RON corpus"]
fn original_ron_corpus_matches_native_entry_definition_and_owner_census() {
    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_RON_CORPUS")
            .expect("explicit external corpus root is required"),
    );
    let provider = Arc::new(provider());
    let budget = limits();
    let mut total = 0;
    for path in [
        ".github/example-run/ambiguity_detection.ron",
        ".github/example-run/no_renderer.ron",
        ".github/example-run/testbed_2d.ron",
        ".github/example-run/testbed_3d.ron",
        ".github/example-run/testbed_ui.ron",
        ".github/example-run/world_serialization.ron",
        "assets/animation_graphs/Fox.animgraph.ron",
        "assets/data/simple_data.ron",
        "assets/serialized_worlds/load_scene_example.scn.ron",
        "examples/asset/processing/assets/a.cool.ron",
        "examples/asset/processing/assets/d.cool.ron",
        "examples/asset/processing/assets/foo/b.cool.ron",
        "examples/asset/processing/assets/foo/c.cool.ron",
    ] {
        let source = fs::read(root.join(path)).unwrap();
        assert!(!source.is_empty() && source.len() <= MAX_SOURCE_BYTES);
        let expected = native_entry_census(&source);
        let case = LanguageCase { path, ..RON };
        let fixture = Fixture::new(case, &source);
        let result = analyze(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        let doc = result.document();
        validate_ir_document(doc, budget.ir(), &ExtensionSupport::default()).unwrap();
        let modules: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::Module && e.language == "ron")
            .collect();
        assert_eq!(modules.len(), 1, "{path}");
        let module = modules[0];
        assert_eq!(
            module.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(&source)
        );
        assert_eq!(
            doc.entities.len(),
            expected.len() + 1,
            "{path}: only actual data fields and one source module"
        );
        let definitions: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|o| o.role == OccurrenceRole::Definition)
            .collect();
        assert_eq!(definitions.len(), expected.len(), "{path}");
        let mut actual = BTreeMap::new();
        for definition in definitions {
            let span = definition.source.span();
            let key = (span.start_byte(), span.end_byte());
            assert!(
                expected.contains_key(&key),
                "{path}: extra definition {key:?}"
            );
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("data definitions have exact source-owned targets");
            };
            assert!(actual.insert(key, symbol).is_none());
            let raw = &source[usize::try_from(key.0).unwrap()..usize::try_from(key.1).unwrap()];
            assert_eq!(definition.syntactic_text_hash, content_hash(raw));
            let property = doc.entities.iter().find(|e| e.id == symbol).unwrap();
            assert_eq!(property.kind, EntityKind::Property);
            assert_eq!(property.language, "ron");
            assert_eq!(property.canonical_name, std::str::from_utf8(raw).unwrap());
        }
        assert_eq!(
            actual.values().copied().collect::<BTreeSet<_>>().len(),
            expected.len(),
            "{path}: no collapsed duplicate fields"
        );
        for (key, parent) in &expected {
            let id = actual[key];
            let owner = parent.map_or(module.id, |span| actual[&span]);
            let property = doc.entities.iter().find(|e| e.id == id).unwrap();
            assert_eq!(
                property.container,
                Some(rootlight_ir::ContainerRef::Entity(owner)),
                "{path}: {key:?}"
            );
            let owners: Vec<_> = doc
                .relations
                .iter()
                .filter(|r| {
                    r.predicate == RelationPredicate::Contains
                        && r.object == RelationEndpoint::Entity(id)
                })
                .map(|r| r.subject)
                .collect();
            assert_eq!(
                owners,
                [RelationEndpoint::Entity(owner)],
                "{path}: no cross-sibling ownership"
            );
        }
        assert!(
            !doc.occurrences
                .iter()
                .any(|o| o.role == OccurrenceRole::CallSite),
            "{path}"
        );
        assert!(
            !doc.entities.iter().any(|e| matches!(
                e.kind,
                EntityKind::Function | EntityKind::Method | EntityKind::Struct
            )),
            "{path}"
        );
        total += expected.len();
        println!(
            "{path}: {} exact data keys, Definitions and source-owned Contains",
            expected.len()
        );
    }
    assert!(total > 15);
    println!("13 original RON files: {total} independently censused data fields");
}
