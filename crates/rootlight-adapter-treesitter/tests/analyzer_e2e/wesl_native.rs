//! Distinct WESL owners and conservative source-owned formal parameter reads.
//! Import, type, property and shader execution targets remain unresolved.

use super::*;
use rootlight_adapter_sdk::{ParseOutput, SyntaxFact, testkit::MockParseProvider};

const WESL: LanguageCase = LanguageCase {
    name: "wesl",
    path: "src/written.wesl",
    frontend: "tree-sitter-wesl-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(WESL, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, WESL),
        &request(&fixture.snapshot, &fixture.source, WESL, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn named_function_and_parameter_have_their_own_wesl_module() {
    let source = b"import package::math::value;\nfn apply(amount: f32) -> f32 { return amount; }\n";
    let result = output(source);
    let doc = result.document();
    let module = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Module && e.language == "wesl")
        .unwrap();
    let function = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Function && e.canonical_name == "apply")
        .unwrap();
    let parameter = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Parameter && e.canonical_name == "amount")
        .unwrap();
    assert_eq!(
        function.container,
        Some(rootlight_ir::ContainerRef::Entity(module.id))
    );
    assert_eq!(
        parameter.container,
        Some(rootlight_ir::ContainerRef::Entity(function.id))
    );
    assert_eq!(function.language, "wesl");
    assert_eq!(parameter.language, "wesl");
    assert_eq!(
        doc.occurrences
            .iter()
            .filter(|o| o.role == OccurrenceRole::Definition)
            .count(),
        2
    );
    assert!(
        doc.occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::ImportUse
                && matches!(o.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn ordinary_return_parameter_is_a_source_owned_read() {
    let source = b"fn apply(amount: f32) -> f32 { return amount; }\n";
    let result = output(source);
    let doc = result.document();
    let parameter = doc
        .entities
        .iter()
        .find(|e| e.kind == EntityKind::Parameter && e.canonical_name == "amount")
        .unwrap();
    assert!(
        doc.occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::Read
                && o.target
                    == OccurrenceTarget::Resolved {
                        symbol: parameter.id
                    })
    );
}

fn reads(doc: &rootlight_ir::NormalizedIrDocument, name: &str, expected: usize) {
    let parameters: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == name)
        .collect();
    assert!(!parameters.is_empty(), "written parameter {name}");
    let sites: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && parameters.iter().any(|parameter| {
                    site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
                })
        })
        .collect();
    assert_eq!(sites.len(), expected, "{name}");
    for site in sites {
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            unreachable!()
        };
        let parameter = parameters
            .iter()
            .find(|parameter| parameter.id == symbol)
            .unwrap();
        let Some(rootlight_ir::ContainerRef::Entity(owner)) = parameter.container else {
            panic!("parameter owner");
        };
        assert_eq!(site.enclosing, Some(owner));
        assert_eq!(site.syntactic_text_hash, content_hash(name.as_bytes()));
        assert!(
            doc.relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
    }
}

#[test]
fn bevy_originals_keep_written_imports_and_only_prove_local_formals() {
    for (source, parameter, count) in [
        (
            include_bytes!("../fixtures/wesl/manual_material.wesl").as_slice(),
            "mesh",
            1,
        ),
        (
            include_bytes!("../fixtures/wesl/wireframe2d.wesl").as_slice(),
            "in",
            0,
        ),
    ] {
        let result = output(source);
        let doc = result.document();
        reads(doc, parameter, count);
        assert!(doc.entities.iter().all(|entity| entity.language == "wesl"
            && matches!(
                entity.kind,
                EntityKind::Module | EntityKind::Function | EntityKind::Parameter
            )));
        assert_eq!(
            doc.occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Definition)
                .count(),
            2
        );
        assert!(
            doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::ImportUse
                    && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            doc.skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities)
        );
        for site in doc
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read)
        {
            assert_eq!(
                &source[usize::try_from(site.source.span().start_byte()).unwrap()
                    ..usize::try_from(site.source.span().end_byte()).unwrap()],
                parameter.as_bytes()
            );
        }
    }
}

#[test]
#[ignore = "requires the caller-supplied pinned external Lygia original"]
fn original_lygia_back_in_proves_three_parameter_reads() {
    let path =
        std::env::var_os("ROOTLIGHT_WESL_EXTERNAL_SOURCE").expect("explicit pinned original path");
    let source = fs::read(path).unwrap();
    assert_eq!(source.len(), 379);
    let result = output(&source);
    let doc = result.document();
    reads(doc, "t", 3);
    assert!(
        doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "backIn")
    );
    assert_eq!(
        doc.occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .count(),
        2
    );
    assert!(
        doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::ImportUse
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn neighboring_functions_keep_same_name_formals_separate() {
    let source = b"fn first(value: f32)->f32 { return value; }\nfn second(value: f32)->f32 { return helper(value) + value; }\n";
    let result = output(source);
    let doc = result.document();
    reads(doc, "value", 3);
    let parameters: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .collect();
    assert_eq!(parameters.len(), 2);
    assert_ne!(parameters[0].id, parameters[1].id);
    assert_ne!(parameters[0].container, parameters[1].container);
    for parameter in parameters {
        let owner = match parameter.container.unwrap() {
            rootlight_ir::ContainerRef::Entity(id) => {
                doc.entities.iter().find(|entity| entity.id == id).unwrap()
            }
            _ => panic!("function owner"),
        };
        let extent = owner.evidence.source.as_ref().unwrap().span();
        for site in doc.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Read
                && site.target
                    == OccurrenceTarget::Resolved {
                        symbol: parameter.id,
                    }
        }) {
            assert!(
                extent.start_byte() <= site.source.span().start_byte()
                    && site.source.span().end_byte() <= extent.end_byte()
            );
        }
    }
}

#[test]
fn callee_type_and_member_names_are_not_formal_value_reads() {
    for source in [
        "fn apply(amount: f32)->f32 { return amount(); }\n",
        "fn apply(amount: f32)->amount { return 0.0; }\n",
        "fn apply(amount: f32)->f32 { return vec4<amount>(0.0); }\n",
        "fn apply(amount: f32, other: T)->f32 { return other.amount; }\n",
    ] {
        reads(output(source.as_bytes()).document(), "amount", 0);
    }
}

#[test]
fn unsupported_shadow_updates_qualification_and_preprocessing_refuse_reads() {
    for source in [
        "fn apply(amount: f32)->f32 { let amount=1.0; return amount; }\n",
        "fn apply(amount: f32)->f32 { { let amount=1.0; } return amount; }\n",
        "fn apply(amount: f32)->f32 { amount=1.0; return amount; }\n",
        "fn apply(amount: f32)->f32 { amount++; return amount; }\n",
        "fn apply(amount: f32)->f32 { return pkg::amount; }\n",
        "#if ENABLED\nfn apply(amount: f32)->f32 { return amount; }\n#endif\n",
        "@if(ENABLED) fn apply(amount: f32)->f32 { return amount; }\n",
        "fn apply(amount: f32)->f32 { return &amount; }\n",
    ] {
        reads(output(source.as_bytes()).document(), "amount", 0);
    }
    let broken = output(b"fn apply(amount: f32)->f32 { return amount + ; }\n");
    assert!(
        broken
            .document()
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::Read)
    );
}

fn parsed(source: &[u8], budget: &AnalysisLimits) -> (Fixture, ParseOutput) {
    let fixture = Fixture::new(WESL, source);
    let native = provider();
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &request(&fixture.snapshot, &fixture.source, WESL, budget).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    (fixture, parsed)
}
fn replay(
    fixture: &Fixture,
    budget: &AnalysisLimits,
    parsed: &ParseOutput,
    facts: Vec<SyntaxFact>,
) -> Result<AnalysisOutput, AdapterError> {
    let native = provider();
    let provider = MockParseProvider::new(
        native.capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        Arc::new(provider),
        producer_identity(),
        language(WESL),
        WESL.frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    execute_analysis(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, WESL, budget),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
}
fn refused(result: Result<AnalysisOutput, AdapterError>) {
    if let Ok(result) = result {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::Read)
        );
    }
}

#[test]
fn missing_duplicate_parent_and_depth_certificates_refuse_binding() {
    let budget = limits();
    let (fixture, native) = parsed(b"fn apply(amount: f32)->f32 { return amount; }\n", &budget);
    reads(
        replay(&fixture, &budget, &native, native.facts().to_vec())
            .unwrap()
            .document(),
        "amount",
        1,
    );
    let labels = [
        "wesl.file.root",
        "wesl.file.module",
        "wesl.file_scope.scope",
        "wesl.function.declaration",
        "wesl.function_scope.scope",
        "wesl.header.signature",
        "wesl.parameters_scope.scope",
        "wesl.parameter.declaration",
        "wesl.name.definition",
        "wesl.block_scope.scope",
        "wesl.identifier.reference",
    ];
    let reference = native
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "wesl.identifier.reference")
        .max_by_key(|fact| fact.span().start_byte())
        .unwrap()
        .local_id();
    for witness in native.facts().iter().filter(|fact| {
        labels.contains(&fact.syntax_kind().as_str())
            && (fact.syntax_kind().as_str() != "wesl.identifier.reference"
                || fact.local_id() == reference)
    }) {
        for variant in 0..4 {
            let mut facts = native.facts().to_vec();
            match variant {
                0 => facts.retain(|fact| fact.local_id() != witness.local_id()),
                1 => facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                )),
                _ => {
                    let index = facts
                        .iter()
                        .position(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    facts[index] = SyntaxFact::new(
                        witness.local_id(),
                        if variant == 2 {
                            Some(witness.local_id())
                        } else {
                            witness.parent()
                        },
                        witness.kind(),
                        witness.span(),
                        if variant == 3 {
                            witness.depth() + 1
                        } else {
                            witness.depth()
                        },
                        witness.syntax_kind().clone(),
                    );
                }
            }
            refused(replay(&fixture, &budget, &native, facts));
        }
    }
}

#[test]
fn equal_width_current_source_shadow_cannot_reuse_old_capture_proof() {
    let budget = limits();
    let prefix = "fn apply(amount: f32)->f32 { ";
    let binder = "let amount=0;";
    let suffix = "return amount; }\n";
    let original = format!("{prefix}{}{suffix}", " ".repeat(binder.len()));
    let changed = format!("{prefix}{binder}{suffix}");
    assert_eq!(original.len(), changed.len());
    let (fixture, native) = parsed(original.as_bytes(), &budget);
    reads(
        replay(&fixture, &budget, &native, native.facts().to_vec())
            .unwrap()
            .document(),
        "amount",
        1,
    );
    let rewritten = fixture.rewrite(changed.as_bytes());
    refused(replay(
        &rewritten,
        &budget,
        &native,
        native.facts().to_vec(),
    ));
}

#[test]
fn complete_native_source_can_exhaust_only_the_lexical_proof_budget() {
    let default = limits();
    let budget = AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        64,
        MAX_SYNTAX_DEPTH,
        32,
        16 * 1024 * 1024,
        default.syntax_stream().clone(),
        default.ir_stream().clone(),
        default.ir().clone(),
    )
    .unwrap();
    let short = b"fn apply(amount: f32)->f32 { return amount; }\n";
    let native = Arc::new(provider());
    let fixture = Fixture::new(WESL, short);
    reads(
        analyze(
            &analyzer(&native, WESL),
            &request(&fixture.snapshot, &fixture.source, WESL, &budget),
            &ExtensionSupport::default(),
        )
        .document(),
        "amount",
        1,
    );
    let source = format!(
        "/*{}*/\n{}",
        " ".repeat(600 * 1024),
        std::str::from_utf8(short).unwrap()
    );
    let (fixture, parsed) = parsed(source.as_bytes(), &budget);
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    let result = analyze(
        &analyzer(&native, WESL),
        &request(&fixture.snapshot, &fixture.source, WESL, &budget),
        &ExtensionSupport::default(),
    );
    reads(result.document(), "amount", 0);
}

#[test]
fn extraction_clipping_and_included_context_refuse_parameter_reads() {
    let source = b"fn apply(amount: f32)->f32 { return amount; }\n";
    let budget = limits_with_syntax_records(4);
    let native = Arc::new(provider());
    let fixture = Fixture::new(WESL, source);
    let clipped = execute_analysis(
        &analyzer(&native, WESL),
        &request(&fixture.snapshot, &fixture.source, WESL, &budget),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    );
    assert!(matches!(
        clipped,
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            ..
        }))
    ));
    let budget = limits();
    let (capture_fixture, captured) = parsed(source, &budget);
    assert_eq!(
        captured.report().coverage().status(),
        CoverageStatus::Complete
    );
    let mut diagnostics = captured.diagnostics().to_vec();
    diagnostics.push(AdapterDiagnostic::new(
        DiagnosticCode::new("syntax-extraction-limit").unwrap(),
        DiagnosticSeverity::Warning,
        None,
        CoverageStatus::Bounded,
    ));
    let mock = MockParseProvider::new(
        native.capabilities().clone(),
        captured.facts().to_vec(),
        diagnostics,
        captured.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(mock),
        producer_identity(),
        language(WESL),
        WESL.frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    refused(execute_analysis(
        &frontend,
        &request(
            &capture_fixture.snapshot,
            &capture_fixture.source,
            WESL,
            &budget,
        ),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    ));
    let included = IncludedRange::new(
        SourceSpan::new(
            fixture.source.span().file(),
            0,
            u64::try_from(source.len()).unwrap(),
        )
        .unwrap(),
        language(WESL),
    );
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(WESL),
        EncodingId::utf8(),
        vec![included],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, WESL),
        &request,
        &ExtensionSupport::default(),
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::Read)
    );
}

#[test]
fn utf16_formal_reads_keep_exact_original_physical_spans() {
    let source = "// λ😀\r\nfn apply(amount: f32)->f32 { return amount; }\r\n";
    for (encoding, big_endian) in [("utf-16le", false), ("utf-16be", true)] {
        let mut bytes = if big_endian {
            vec![0xfe, 0xff]
        } else {
            vec![0xff, 0xfe]
        };
        for unit in source.encode_utf16() {
            bytes.extend_from_slice(&if big_endian {
                unit.to_be_bytes()
            } else {
                unit.to_le_bytes()
            });
        }
        let fixture = Fixture::new(WESL, &bytes);
        let budget = limits();
        let native = Arc::new(provider());
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(WESL),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&native, WESL),
            &input,
            &ExtensionSupport::default(),
        );
        let doc = result.document();
        reads(doc, "amount", 1);
        let logical = source.rfind("amount").unwrap();
        let start = u64::try_from(2 + 2 * source[..logical].encode_utf16().count()).unwrap();
        let site = doc
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::Read)
            .unwrap();
        assert_eq!(
            (
                site.source.span().start_byte(),
                site.source.span().end_byte()
            ),
            (start, start + 12)
        );
        assert_eq!(site.source.content_hash(), content_hash(&bytes));
    }
}
