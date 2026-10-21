//! Exact ordinary C variables and value reads from a complete licensed original.
//! The separate observation retains native ancestry and current facts before any
//! new variable protocol is implemented; header calls remain unqualified.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/c_written_bindings/xorshift64.c");

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact written {name} definition");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("a definition must resolve to its own written entity");
    };
    let entity = document.entities.iter().find(|e| e.id == symbol).unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(sites[0].source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(entity.provenance, sites[0].provenance);
    symbol
}

#[test]
fn original_global_and_for_local_keep_exact_definitions_four_reads_and_internal_call() {
    assert_eq!(ORIGINAL.len(), 305);
    for (start, end, written) in [
        (54, 58, b"seed".as_slice()),
        (82, 92, b"xorshift64"),
        (193, 197, b"main"),
        (221, 222, b"i"),
        (115, 119, b"seed"),
        (140, 144, b"seed"),
        (171, 175, b"seed"),
        (228, 229, b"i"),
        (279, 289, b"xorshift64"),
    ] {
        assert_eq!(&ORIGINAL[start..end], written);
    }
    let case = *CASES.iter().find(|case| case.name == "c").unwrap();
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    let result = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, case, &bounds),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, bounds.ir(), &ExtensionSupport::default()).unwrap();
    // These already-passing declarations and dependency must survive the fix.
    let generator = definition(document, "xorshift64", EntityKind::Function, 82, 92);
    let main = definition(document, "main", EntityKind::Function, 193, 197);
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(main)
                && edge.object == RelationEndpoint::Entity(generator)
        })
        .collect();
    assert_eq!(calls.len(), 1);
    let call_source = calls[0].evidence.source.as_ref().unwrap();
    assert_eq!(call_source.content_hash(), content_hash(ORIGINAL));
    assert!(call_source.span().start_byte() <= 279);
    assert!(call_source.span().end_byte() >= 289);

    let seed = definition(document, "seed", EntityKind::Variable, 54, 58);
    let index = definition(document, "i", EntityKind::Variable, 221, 222);
    let local = document.entities.iter().find(|e| e.id == index).unwrap();
    assert_eq!(
        local.container,
        Some(rootlight_ir::ContainerRef::Entity(main))
    );
    let contains: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(main)
                && edge.object == RelationEndpoint::Entity(index)
        })
        .collect();
    assert_eq!(contains.len(), 1);
    assert_eq!(
        contains[0].evidence.source.as_ref().unwrap().content_hash(),
        content_hash(ORIGINAL)
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(index)
            && edge.object == RelationEndpoint::Entity(main)
    }));
    for (start, end, target, owner) in [
        (115, 119, seed, generator),
        (140, 144, seed, generator),
        (171, 175, seed, generator),
        (228, 229, index, main),
    ] {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(
            sites.len(),
            1,
            "one source-owned value read at {start}..{end}"
        );
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        assert_eq!(sites[0].enclosing, Some(owner));
        assert_eq!(sites[0].repository, fixture.source.repository());
        assert_eq!(sites[0].generation, fixture.source.generation());
        assert_eq!(sites[0].source.content_hash(), content_hash(ORIGINAL));
        assert_eq!(sites[0].source.span().file(), fixture.source.span().file());
        assert!(!document.occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        }));
    }
}

#[test]
fn native_variable_scope_observation_keeps_complete_original_and_current_raw_facts() {
    use rootlight_adapter_sdk::{SyntaxFactKind, execute_parse};
    assert_eq!(ORIGINAL.len(), 305);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ORIGINAL, None).unwrap();
    let root = tree.root_node();
    assert_eq!((root.start_byte(), root.end_byte()), (0, ORIGINAL.len()));
    assert!(
        !root.has_error(),
        "the complete original must be native-clean"
    );
    let selected = [
        (54, 58),
        (115, 119),
        (140, 144),
        (171, 175),
        (221, 222),
        (228, 229),
    ];
    let mut seen = [0; 6];
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= ORIGINAL.len());
        eprintln!(
            "C_NATIVE kind={} field={:?} range={}..{} parent={:?}",
            node.kind(),
            cursor.field_name(),
            node.start_byte(),
            node.end_byte(),
            node.parent()
                .map(|p| (p.kind(), p.start_byte(), p.end_byte()))
        );
        for (index, &(start, end)) in selected.iter().enumerate() {
            if node.kind() == "identifier" && node.start_byte() == start && node.end_byte() == end {
                seen[index] += 1;
                let mut ancestor = node.parent();
                while let Some(parent) = ancestor {
                    eprintln!(
                        "C_OWNER site={start}..{end} kind={} range={}..{}",
                        parent.kind(),
                        parent.start_byte(),
                        parent.end_byte()
                    );
                    ancestor = parent.parent();
                }
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                assert_eq!(seen, [1; 6]);
                let case = *CASES.iter().find(|case| case.name == "c").unwrap();
                let native = provider();
                let fixture = Fixture::new(case, ORIGINAL);
                let bounds = limits();
                let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
                let parsed = execute_parse(
                    &native,
                    &input.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                for fact in parsed.facts() {
                    eprintln!(
                        "C_RAW id={} parent={:?} kind={:?} label={} range={}..{} depth={}",
                        fact.local_id(),
                        fact.parent(),
                        fact.kind(),
                        fact.syntax_kind().as_str(),
                        fact.span().start_byte(),
                        fact.span().end_byte(),
                        fact.depth()
                    );
                }
                assert_eq!(
                    parsed
                        .facts()
                        .iter()
                        .filter(|fact| fact.kind() == SyntaxFactKind::Root)
                        .count(),
                    1
                );
                let analyzer = analyzer(&Arc::new(native), case);
                let output = analyze(&analyzer, &input, &ExtensionSupport::default());
                for entity in &output.document().entities {
                    eprintln!(
                        "C_ENTITY kind={:?} name={} container={:?} source={:?}",
                        entity.kind,
                        entity.canonical_name,
                        entity.container,
                        entity.evidence.source
                    );
                }
                for site in &output.document().occurrences {
                    eprintln!(
                        "C_OCCURRENCE role={:?} label={} range={}..{} enclosing={:?} target={:?}",
                        site.role,
                        site.syntax_kind,
                        site.source.span().start_byte(),
                        site.source.span().end_byte(),
                        site.enclosing,
                        site.target
                    );
                }
                return;
            }
        }
    }
}

// Native target windows are tested through public materialization and artifacts.
// Written value mutation is deliberately distinct from lexical rebinding.

fn variable_case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "c").unwrap()
}

fn variable_output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let case = variable_case();
    let native = Arc::new(provider());
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let output = analyze(
        &analyzer(&native, case),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for site in &output.document().occurrences {
        assert_eq!(site.source.content_hash(), content_hash(bytes));
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
    }
    output
}

fn variable_definition(
    document: &rootlight_ir::NormalizedIrDocument,
    at: usize,
    word: &str,
) -> SymbolId {
    let found: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == at as u64
                && site.source.span().end_byte() == (at + word.len()) as u64
        })
        .collect();
    assert_eq!(found.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = found[0].target else {
        panic!("written definition")
    };
    let entity = document.entities.iter().find(|e| e.id == symbol).unwrap();
    assert_eq!(entity.kind, EntityKind::Variable);
    assert_eq!(entity.canonical_name, word);
    symbol
}

fn variable_read(
    document: &rootlight_ir::NormalizedIrDocument,
    at: usize,
    word: &str,
    target: SymbolId,
) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == at as u64
                && site.source.span().end_byte() == (at + word.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "one value read at {at}");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                && edge.object == RelationEndpoint::Entity(target))
            .count(),
        1
    );
}

#[test]
fn renamed_nested_shadow_and_mutating_lhs_preserve_exact_rhs_lexical_targets() {
    let text = "int state=1; int owned(void){ state ^= state<<1; { int state=2; state ^= state<<2; } return state; }\n";
    let result = variable_output(text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let global = variable_definition(document, text.find("state").unwrap(), "state");
    let local = variable_definition(document, text.find("state=2").unwrap(), "state");
    assert_ne!(global, local);
    variable_read(document, text.find("state<<1").unwrap(), "state", global);
    variable_read(document, text.find("state<<2").unwrap(), "state", local);
    variable_read(document, text.rfind("state").unwrap(), "state", global);
    for marker in ["state ^= state<<1", "state ^= state<<2"] {
        let at = text.find(marker).unwrap() as u64;
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == at)
        );
    }
}

#[test]
fn for_initializer_scope_ends_before_the_following_call_and_keeps_ordinary_names() {
    let text = "int owned(void){ for(int cursor=0;cursor<3;++cursor){ external(cursor); } external(cursor); return 0; }\n";
    let result = variable_output(text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let target = variable_definition(document, text.find("cursor").unwrap(), "cursor");
    variable_read(document, text.find("cursor<3").unwrap(), "cursor", target);
    variable_read(document, text.find("cursor);").unwrap(), "cursor", target);
    let outside = text.rfind("cursor").unwrap() as u64;
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.source.span().start_byte() == outside)
        .collect();
    assert!(!sites.is_empty());
    assert!(
        sites
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == text.find("external").unwrap() as u64)
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn duplicate_macro_recovery_and_non_scalar_declarators_never_gain_variable_certificates() {
    for text in [
        "int state=1; int state=2; int owned(void){ return state; }\n",
        "#define state other\nint state=1; int owned(void){ return state; }\n",
        "int state=1; int owned(void){ return state + ; }\n",
        "int (*state)(void)=0; int owned(void){ return state(); }\n",
    ] {
        let result = variable_output(text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "c.written_variable.reference")
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable
                    && entity.canonical_name == "state")
        );
    }
}

#[test]
fn original_variable_certificates_keep_bom_and_both_utf16_physical_coordinates() {
    let text = std::str::from_utf8(ORIGINAL).unwrap();
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(ORIGINAL);
    let output = variable_output(&bom, EncodingId::utf8());
    let target = variable_definition(output.document(), 57, "seed");
    variable_read(output.document(), 118, "seed", target);
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let output = variable_output(
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let document = output.document();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == 2 + 2 * 54
                    && site.source.span().end_byte() == 2 + 2 * 58
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("encoded definition")
        };
        let reads: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == 2 + 2 * 115
                    && site.source.span().end_byte() == 2 + 2 * 119
            })
            .collect();
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].target, OccurrenceTarget::Resolved { symbol });
    }
}

#[test]
fn actual_materialization_requires_unique_file_target_declaration_and_definition_witnesses() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let native = provider();
    let case = variable_case();
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts| {
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new("c-variable-replay", "1", content_hash(b"c-variable-replay"))
                .unwrap(),
            language(case),
            case.frontend,
            content_hash(b"c-variable-binary"),
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    let seed = variable_definition(baseline.document(), 54, "seed");
    variable_read(baseline.document(), 115, "seed", seed);
    for (label, start, end, duplicate) in [
        ("c.variable_file_context.signature", 0, 305, false),
        ("c.variable_target.signature", 54, 119, false),
        ("c.variable_target.signature", 54, 119, true),
        ("c.variable.declaration", 54, 70, true),
        ("c.variable_name.definition", 54, 58, true),
    ] {
        let mut facts = parsed.facts().to_vec();
        let selected: Vec<_> = facts
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .cloned()
            .collect();
        assert_eq!(selected.len(), 1, "exact own witness {label}");
        if duplicate {
            let fact = &selected[0];
            let id = facts.iter().map(|f| f.local_id()).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                id,
                fact.parent(),
                fact.kind(),
                fact.span(),
                fact.depth(),
                fact.syntax_kind().clone(),
            ));
        } else {
            facts.retain(|fact| fact.local_id() != selected[0].local_id());
        }
        let output = replay(facts);
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == 115
                    && site.source.span().end_byte() == 119
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert!(matches!(
            sites[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations
                    && gap.source.content_hash() == content_hash(ORIGINAL)
                    && gap.detail == "c-native-written-variable-binding-unavailable")
        );
    }
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "c.unproven_variable.reference")
    );
}

#[test]
fn required_provider_count_and_fresh_exact_artifact_keep_reads_then_one_less_refuses() {
    let native = Arc::new(provider());
    let case = variable_case();
    let analyzer = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let wide = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &wide);
    let required = native
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 4);
    let budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, case, &budget);
    let (output, artifact) = analyzer
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let target = variable_definition(output.document(), 54, "seed");
    variable_read(output.document(), 115, "seed", target);
    let rebound = analyzer
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(rebound.document(), output.document());
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, case, &budget);
    let rebound = analyzer
        .analyze_from_artifact(
            &next_input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        rebound.document(),
        analyze(&analyzer, &next_input, &ExtensionSupport::default()).document()
    );
    let small_budget = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, case, &small_budget);
    assert_eq!(
        native
            .required_syntax_fact_count(&small.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    for error in [
        analyzer.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            native.as_ref(),
            &small.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })
            if observed == required && limit == required-1),
            "{error:?}"
        );
    }
    let stopped = Cancellation::new();
    stopped.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.prepare_artifact(&exact, &stopped),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn variable_value_edits_keep_ids_and_real_ir_boundaries_refuse_atomically() {
    let text = b"int state=1; int owned(void){ state ^= state<<1; return state; }\n";
    let changed = b"int state=9; int owned(void){ state ^= state<<2; return state; }\n";
    let first = variable_output(text, EncodingId::utf8());
    let second = variable_output(changed, EncodingId::utf8());
    let ids = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|e| (e.id, (e.kind, e.canonical_name.clone(), e.container)))
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(ids(&first), ids(&second));
    let native = Arc::new(provider());
    let case = variable_case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, text);
    let base = limits();
    let run = |ir| {
        let bounds = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            ir,
        )
        .unwrap();
        frontend
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let target = variable_definition(baseline.document(), 4, "state");
    variable_read(
        baseline.document(),
        std::str::from_utf8(text).unwrap().find("state<<").unwrap(),
        "state",
        target,
    );
    for relations in [true, false] {
        let defaults = IrLimits::default();
        let mut low = 0;
        let mut high = if relations {
            defaults.max_relations
        } else {
            defaults.max_total_records
        };
        let set = |cap| {
            let mut ir = IrLimits::default();
            if relations {
                ir.max_relations = cap;
            } else {
                ir.max_total_records = cap;
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected admission failure: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::Records, observed, limit })
            if observed > limit && limit == low-1),
            "{error:?}"
        );
    }
}

#[test]
fn sizeof_assignment_and_call_arguments_remain_observed_without_value_reads() {
    for text in [
        "int state=1; int owned(void){ return sizeof(state = state); }\n",
        "int state=1; int owned(void){ return sizeof(external(state)); }\n",
    ] {
        let output = variable_output(text.as_bytes(), EncodingId::utf8());
        variable_definition(output.document(), 4, "state");
        let at = text.rfind("state").unwrap() as u64;
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.source.span().start_byte() == at)
            .collect();
        assert!(!sites.is_empty());
        assert!(sites.iter().all(|site| site.role != OccurrenceRole::Read
            && matches!(site.target, OccurrenceTarget::Unresolved { .. })));
    }
}
