//! Closing names require the real current HTML element and its opening Definition.
//! Complete originals, encoded coordinates and native/artifact tampering exercise
//! the source proof without qualifying browser DOM or embedded runtime behavior.

use super::*;
use rootlight_adapter_sdk::{ParseOutput, SyntaxFact, testkit::MockParseProvider};

const ORIGINAL: &[u8] = include_bytes!("../fixtures/html_closing_tags/respond-proxy.html");
const LABEL: &str = "html.closing_tag.reference";
const MARKER: &str = "html.closing_tag_context.signature";
const SIMPLE: &str = "<main><part>value</part></main>";

fn case() -> LanguageCase {
    LanguageCase {
        name: "html",
        path: "view.html",
        frontend: "tree-sitter-html-0.23.2",
        source: "",
        generated: false,
        body_before: "",
        body_after: "",
    }
}

fn output(raw: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(case(), raw);
    let bounds = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case()),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &bounds,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case()),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn certified(
    document: &rootlight_ir::NormalizedIrDocument,
    opening: (u64, u64),
    closing: (u64, u64),
    name: &str,
) -> SymbolId {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == opening
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("opening Definition")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::MarkupElement);
    assert_eq!(entity.language, "html");
    assert_eq!(entity.canonical_name, name);
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == closing
        })
        .collect();
    assert_eq!(references.len(), 1);
    let site = references[0];
    assert_eq!(site.syntax_kind, LABEL);
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(site.enclosing, Some(symbol));
    assert_eq!(site.provenance, entity.provenance);
    assert_eq!(
        site.source.content_hash(),
        definitions[0].source.content_hash()
    );
    assert_eq!(
        site.id,
        rootlight_ir::derive_occurrence_record_id(site).unwrap()
    );
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&site.source)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].id,
        rootlight_ir::derive_relation_record_id(edges[0]).unwrap()
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|other| other.role == OccurrenceRole::Definition
                && other.source.span() == site.source.span())
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&site.source))
    );
    symbol
}

fn no_adoption(document: &rootlight_ir::NormalizedIrDocument) {
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == LABEL
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(!document.relations.iter().any(|edge| edge.predicate == RelationPredicate::RefersTo
        && matches!(edge.subject, RelationEndpoint::Occurrence(id) if document.occurrences.iter()
            .any(|site| site.id == id && site.syntax_kind == LABEL))));
}

fn unproved_at(document: &rootlight_ir::NormalizedIrDocument, at: (u64, u64)) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == LABEL
                && (
                    site.source.span().start_byte(),
                    site.source.span().end_byte(),
                ) == at
        })
        .collect();
    assert!(
        sites
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && sites
                .iter()
                .any(|site| edge.subject == RelationEndpoint::Occurrence(site.id))
    }));
}

#[test]
fn complete_original_head_close_has_exact_reference_and_preserves_markup_ownership() {
    assert_eq!(ORIGINAL.len(), 2587);
    assert_eq!(
        ORIGINAL.windows(2).filter(|part| *part == b"\r\n").count(),
        96
    );
    assert_eq!(&ORIGINAL[143..147], b"head");
    assert_eq!(&ORIGINAL[215..219], b"head");
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let head = certified(document, (143, 147), (215, 219), "head");
    for (start, end, name) in [
        (135, 139, "html"),
        (153, 157, "meta"),
        (158, 165, "charset"),
    ] {
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("original markup Definition")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.canonical_name, name);
        if name == "meta" {
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(head)
                        && edge.object == RelationEndpoint::Entity(symbol))
            );
        }
    }
}

#[test]
fn ascii_case_pairing_keeps_opening_spelling_and_repeated_sibling_targets_distinct() {
    let text = "<MAIN><Part>one</pArT><Part>two</Part></main>";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let opening = text.find("Part").unwrap() as u64;
    let closing = text.find("pArT").unwrap() as u64;
    let first = certified(
        result.document(),
        (opening, opening + 4),
        (closing, closing + 4),
        "Part",
    );
    let second_open = text.rfind("<Part>").unwrap() as u64 + 1;
    let second_close = text.rfind("</Part>").unwrap() as u64 + 2;
    let second = certified(
        result.document(),
        (second_open, second_open + 4),
        (second_close, second_close + 4),
        "Part",
    );
    assert_ne!(first, second);
}

#[test]
fn comments_attributes_and_script_strings_never_create_closing_references() {
    let text = "<html><head data-note='</head>'><!-- </head> --><script>const value='</head>';</script></head><body></body></html>";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let real = text.rfind("</head>").unwrap() as u64 + 2;
    certified(result.document(), (7, 11), (real, real + 4), "head");
    for (at, _) in text
        .match_indices("</head>")
        .filter(|(at, _)| (*at as u64 + 2) != real)
    {
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == LABEL
                    && site.source.span().start_byte() == at as u64 + 2)
        );
    }
}

#[test]
fn malformed_implicit_foreign_and_noscript_contexts_withhold_new_targets() {
    for text in [
        "<div><span></div>",
        "<div></span>",
        "<div>",
        "<div/>",
        "<br></br>",
        "<main><svg><path/></svg></main>",
        "<main><math><mi>x</mi></math></main>",
        "<main><noscript><p>x</p></noscript></main>",
    ] {
        no_adoption(output(text.as_bytes(), EncodingId::utf8()).document());
    }
}

#[test]
fn html_host_proof_survives_an_unrelated_embedded_javascript_parse_failure() {
    let text = "<html><head><script>function (</script></head><body></body></html>";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let close = text.find("</head>").unwrap() as u64 + 2;
    certified(result.document(), (7, 11), (close, close + 4), "head");
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "html-embedded-parse-error")
        .collect();
    assert_eq!(gaps.len(), 1);
    let body = gaps[0].source.span();
    assert_eq!(
        &text[usize::try_from(body.start_byte()).unwrap()
            ..usize::try_from(body.end_byte()).unwrap()],
        "function ("
    );
}

#[test]
fn crlf_bom_utf8_and_both_utf16_orders_preserve_physical_closing_names() {
    let text = "<main>\u{03bb}\u{1f600}\r\n<Part>value</pArT></main>\r\n";
    let opening = text.find("Part").unwrap();
    let closing = text.find("pArT").unwrap();
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let at = |offset| u64::try_from(2 + 2 * text[..offset].encode_utf16().count()).unwrap();
        let result = output(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        certified(
            result.document(),
            (at(opening), at(opening + 4)),
            (at(closing), at(closing + 4)),
            "Part",
        );
    }
    for bom in [false, true] {
        let raw = format!("{}{text}", if bom { "\u{feff}" } else { "" });
        let shift = if bom { 3 } else { 0 };
        certified(
            output(raw.as_bytes(), EncodingId::utf8()).document(),
            ((opening + shift) as u64, (opening + shift + 4) as u64),
            ((closing + shift) as u64, (closing + shift + 4) as u64),
            "Part",
        );
    }
}

#[test]
fn explicit_full_partial_and_split_ranges_cannot_supply_complete_host_authority() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), SIMPLE.as_bytes());
    let bounds = limits();
    for selected in [
        vec![(0, SIMPLE.len())],
        vec![(6, SIMPLE.len() - 7)],
        vec![(0, 6), (6, SIMPLE.len())],
    ] {
        let ranges = selected
            .into_iter()
            .map(|(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(
                        fixture.source.span().file(),
                        u64::try_from(start).unwrap(),
                        u64::try_from(end).unwrap(),
                    )
                    .unwrap(),
                    language(case()),
                )
            })
            .collect();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case()),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &bounds,
        )
        .unwrap()
        .with_generated_status(false);
        no_adoption(analyze(&frontend, &input, &ExtensionSupport::default()).document());
    }
}

fn parse() -> (Fixture, AnalysisLimits, ParseOutput) {
    let native = provider();
    let fixture = Fixture::new(case(), SIMPLE.as_bytes());
    let bounds = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &request(&fixture.snapshot, &fixture.source, case(), &bounds).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    (fixture, bounds, parsed)
}

fn replay(
    fixture: &Fixture,
    bounds: &AnalysisLimits,
    parsed: &ParseOutput,
    facts: Vec<SyntaxFact>,
) -> Result<AnalysisOutput, AdapterError> {
    let native = provider();
    let parser = MockParseProvider::new(
        native.capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(parser),
        producer_identity(),
        language(case()),
        case().frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    execute_analysis(
        &frontend,
        &request(&fixture.snapshot, &fixture.source, case(), bounds),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
}

#[test]
fn missing_duplicate_reparented_and_relabelled_native_witnesses_never_adopt_a_target() {
    let (fixture, bounds, parsed) = parse();
    let close = SIMPLE.find("</part>").unwrap() as u64 + 2;
    certified(
        replay(&fixture, &bounds, &parsed, parsed.facts().to_vec())
            .unwrap()
            .document(),
        (7, 11),
        (close, close + 4),
        "part",
    );
    for label in [
        "html.file.root",
        "html.file.module",
        "html.element.declaration",
        "html.tag_name.definition",
        MARKER,
        LABEL,
    ] {
        let witness = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == label
                    && (label != "html.tag_name.definition" || fact.span().start_byte() == 7)
            })
            .unwrap();
        for variant in 0..4 {
            let mut facts = parsed.facts().to_vec();
            if variant == 0 {
                facts.retain(|fact| fact.local_id() != witness.local_id());
                for child in &mut facts {
                    if child.parent() == Some(witness.local_id()) {
                        *child = SyntaxFact::new(
                            child.local_id(),
                            witness.parent(),
                            child.kind(),
                            child.span(),
                            child.depth(),
                            child.syntax_kind().clone(),
                        );
                    }
                }
            } else if variant == 1 {
                let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                facts.push(SyntaxFact::new(
                    next,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            } else {
                let index = facts
                    .iter()
                    .position(|fact| fact.local_id() == witness.local_id())
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    witness.local_id(),
                    if variant == 2 {
                        // Detaching a root would otherwise leave its valid None parent unchanged.
                        witness.parent().is_none().then_some(witness.local_id())
                    } else {
                        witness.parent()
                    },
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    if variant == 3 {
                        rootlight_adapter_sdk::SyntaxKindLabel::new(
                            "html.refused_context.signature",
                        )
                        .unwrap()
                    } else {
                        witness.syntax_kind().clone()
                    },
                );
                if variant == 2 {
                    assert_ne!(facts[index].parent(), witness.parent());
                }
            }
            let replayed = replay(&fixture, &bounds, &parsed, facts);
            if variant == 2 && witness.parent().is_none() {
                // A root self-reference fails the shared fact graph before binding.
                assert!(
                    matches!(replayed, Err(AdapterError::ProviderFailed { code })
                    if code.as_str() == "treesitter-lowering-parent")
                );
                continue;
            }
            match replayed {
                Ok(result) => unproved_at(result.document(), (close, close + 4)),
                Err(AdapterError::Sink(_)) => {}
                Err(error) => panic!("unexpected non-structural refusal: {error:?}"),
            }
        }
    }
}

#[test]
fn exact_required_artifact_replay_and_changed_source_keep_the_same_proof_contract() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), SIMPLE.as_bytes());
    let wide = limits();
    let required = native
        .required_syntax_fact_count(
            &request(&fixture.snapshot, &fixture.source, case(), &wide).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    let bounds = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, case(), &bounds);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let closing = SIMPLE.find("</part>").unwrap() as u64 + 2;
    certified(result.document(), (7, 11), (closing, closing + 4), "part");
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, case(), &bounds);
    let rebound = frontend
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
        analyze(&frontend, &next_input, &ExtensionSupport::default()).document()
    );
    let low = limits_with_syntax_records(required - 1);
    assert!(
        matches!(frontend.prepare_artifact(&request(&fixture.snapshot, &fixture.source, case(), &low), &deadline()),
        Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }))
            if observed == required && limit == required - 1)
    );
    let changed = fixture.rewrite(SIMPLE.replacen("</part>", "</fake>", 1).as_bytes());
    assert!(
        matches!(frontend.analyze_from_artifact(&request(&changed.snapshot, &changed.source, case(), &bounds),
        &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
    let parsed = native_parse_for_replay(&fixture, &wide);
    no_adoption(
        replay(&changed, &wide, &parsed, parsed.facts().to_vec())
            .unwrap()
            .document(),
    );
}

fn native_parse_for_replay(fixture: &Fixture, bounds: &AnalysisLimits) -> ParseOutput {
    rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request(&fixture.snapshot, &fixture.source, case(), bounds).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap()
}

#[test]
fn closing_reference_and_relation_have_real_exact_ir_admission_boundaries() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), SIMPLE.as_bytes());
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
                &request(&fixture.snapshot, &fixture.source, case(), &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let close = SIMPLE.find("</part>").unwrap() as u64 + 2;
    certified(baseline.document(), (7, 11), (close, close + 4), "part");
    for relations in [false, true] {
        let defaults = IrLimits::default();
        let mut low = 0;
        let mut high = if relations {
            defaults.max_relations
        } else {
            defaults.max_total_records
        };
        let cap = |maximum| {
            let mut ir = IrLimits::default();
            if relations {
                ir.max_relations = maximum
            } else {
                ir.max_total_records = maximum
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(cap(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected refusal: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(cap(low)).unwrap().document(), baseline.document());
        assert!(matches!(
            run(cap(low - 1)),
            Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
        ));
    }
}
