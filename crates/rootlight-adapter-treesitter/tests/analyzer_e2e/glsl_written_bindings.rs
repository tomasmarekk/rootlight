//! Original written GLSL binding obligations and bounded native witness controls.
//! These checks keep shader execution and unresolved external calls separate.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/glsl_written_binding/common.glsl");
const GLSL: LanguageCase = LanguageCase {
    name: "glsl",
    path: "written.glsl",
    frontend: "tree-sitter-glsl-0.2.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const SIMPLE: &str = "float selected(float input0){return input0;} float owned(float input1){return selected(input1);}";

fn run(raw: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(GLSL, raw);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(GLSL),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let output = analyze(
        &analyzer(&native, GLSL),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files[0].content_hash, content_hash(raw));
    for site in &output.document().occurrences {
        assert_eq!(site.source.content_hash(), content_hash(raw));
        assert_eq!(site.source.repository(), output.document().repository);
        assert_eq!(site.source.generation(), output.document().generation);
        assert!(site.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    output
}

fn defined(
    output: &AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "{name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition is resolved")
    };
    let entities: Vec<_> = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.id == symbol)
        .collect();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].kind, kind);
    assert_eq!(entities[0].canonical_name, name);
    symbol
}

fn use_at(output: &AnalysisOutput, role: OccurrenceRole, start: u64, end: u64, target: SymbolId) {
    let sites: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == role
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "{role:?}@{start}..{end}");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span() == sites[0].source.span())
    );
}

#[test]
fn original_glsl_formal_and_same_file_call_keep_all_exact_obligations() {
    assert_eq!(ORIGINAL.len(), 5169);
    assert_eq!(&ORIGINAL[2473..2478], b"color");
    assert_eq!(&ORIGINAL[2788..2797], b"luminance");
    assert_eq!(
        ORIGINAL.windows(9).filter(|s| *s == b"luminance").count(),
        10
    );
    assert!(
        !ORIGINAL
            .windows(b"secondary-next-glsl-050-20261020_absent_literal".len())
            .any(|s| s == b"secondary-next-glsl-050-20261020_absent_literal")
    );
    let output = run(ORIGINAL, EncodingId::utf8());
    let function = defined(&output, "luminance", EntityKind::Function, 2434, 2443);
    let formal = defined(&output, "color", EntityKind::Parameter, 2449, 2454);
    let caller = defined(&output, "contrast_ratio", EntityKind::Function, 2722, 2736);
    use_at(&output, OccurrenceRole::Reference, 2473, 2478, formal);
    use_at(&output, OccurrenceRole::CallSite, 2788, 2797, function);
    assert!(
        output
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(function)
                && edge.object == RelationEndpoint::Entity(formal))
    );
    let calls: Vec<_> = output
        .document()
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(caller)
                && edge.object == RelationEndpoint::Entity(function)
                && edge.evidence.source.as_ref().is_some_and(|source| {
                    source.span().start_byte() == 2788 && source.span().end_byte() == 2797
                })
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(function)
                && edge.object == RelationEndpoint::Entity(caller))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "glsl-shader-linking-and-interface-binding-unavailable")
    );
}

#[test]
fn ordinary_names_exact_types_zero_arguments_and_recursion_are_source_owned() {
    let output = run(SIMPLE.as_bytes(), EncodingId::utf8());
    let target = defined(&output, "selected", EntityKind::Function, 6, 14);
    let start = u64::try_from(SIMPLE.rfind("selected").unwrap()).unwrap();
    use_at(&output, OccurrenceRole::CallSite, start, start + 8, target);
    for source in [
        "float selected(){return 1;} float owned(){return selected();}",
        "float owned(float input0){return owned(input0);}",
    ] {
        let output = run(source.as_bytes(), EncodingId::utf8());
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .count(),
            1,
            "{source}"
        );
    }
}

#[test]
fn macros_overloads_prototypes_shadow_writes_scopes_and_unknown_types_do_not_bind() {
    for source in [
        "float target(float x); float target(float x){return x;} float owned(float x){return target(x);}",
        "float target(int x){return x;} float owned(float x){return target(x);}",
        "float target(float x){return x;} float owned(float x){float target=0;return target(x);}",
        "#define target replacement\nfloat target(float x){return x;} float owned(float x){return target(x);}",
        "#version 430 core\n#version 430 core\nfloat target(float x){return x;} float owned(float x){return target(x);}",
        "#extension GL_ARB_gpu_shader5 : enable\nfloat target(float x){return x;} float owned(float x){return target(x);}",
        "#if ENABLED\nfloat target(float x){return x;}\n#endif\nfloat owned(float x){return target(x);}",
        "float target(out float x){return x;} float owned(float x){return target(x);}",
        "float target(float x){return x;} float owned(float x){x=1;return target(x);}",
        "float target(float x){return x;} float owned(float x){if(true){return target(x);}return x;}",
        "float target(float x){return x;} float owned(float x){return target(unknown);}",
        "float owned(float x){return target(x);} float target(float x){return x;}",
    ] {
        let output = run(source.as_bytes(), EncodingId::utf8());
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{source}"
        );
        let start = u64::try_from(source.find("return target(").unwrap() + 7).unwrap();
        assert!(
            output
                .document()
                .occurrences
                .iter()
                .any(|site| site.source.span().start_byte() == start
                    && site.syntax_kind == "glsl.unproven_call.reference"
                    && matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "ordinary optional reference survives: {source}"
        );
    }
    let source =
        "float donor(float value){return external(value);} float owned(){return external(value);}";
    let output = run(source.as_bytes(), EncodingId::utf8());
    let last = u64::try_from(source.rfind("value").unwrap()).unwrap();
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == last
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn utf8_bom_crlf_and_both_utf16_orders_preserve_physical_certificates() {
    let text = format!("// λ😀\r\n{SIMPLE}");
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
        let output = run(
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let physical = |byte| u64::try_from(2 + 2 * text[..byte].encode_utf16().count()).unwrap();
        let declaration = text.find("selected").unwrap();
        let target = defined(
            &output,
            "selected",
            EntityKind::Function,
            physical(declaration),
            physical(declaration + 8),
        );
        let usage = text.rfind("selected").unwrap();
        use_at(
            &output,
            OccurrenceRole::CallSite,
            physical(usage),
            physical(usage + 8),
            target,
        );
        let formal_declaration = text.find("input1").unwrap();
        let formal = defined(
            &output,
            "input1",
            EntityKind::Parameter,
            physical(formal_declaration),
            physical(formal_declaration + 6),
        );
        let formal_usage = text.rfind("input1").unwrap();
        use_at(
            &output,
            OccurrenceRole::Reference,
            physical(formal_usage),
            physical(formal_usage + 6),
            formal,
        );
    }
    let bom = format!("\u{feff}{text}");
    let output = run(bom.as_bytes(), EncodingId::utf8());
    let start = u64::try_from(bom.rfind("selected").unwrap()).unwrap();
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == start
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn required_preflight_fresh_exact_artifact_replay_and_one_less_agree() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, GLSL);
    let fixture = Fixture::new(GLSL, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, GLSL, &budget);
    let artifact = frontend.prepare_artifact(&initial, &deadline()).unwrap();
    let required = native
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, GLSL, &budget);
    let (bounded, retained) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        retained.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let formal = defined(&bounded, "color", EntityKind::Parameter, 2449, 2454);
    use_at(&bounded, OccurrenceRole::Reference, 2473, 2478, formal);
    let function = defined(&bounded, "luminance", EntityKind::Function, 2434, 2443);
    use_at(&bounded, OccurrenceRole::CallSite, 2788, 2797, function);
    let next = fixture.next_generation();
    let input = request(&next.snapshot, &next.source, GLSL, &budget);
    let replayed = frontend
        .analyze_from_artifact(
            &input,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        replayed.document(),
        analyze(&frontend, &input, &ExtensionSupport::default()).document()
    );
    let low = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, GLSL, &low),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit {resource: ResourceKind::RequiredSyntaxFacts, observed, limit}) if observed == required && limit == required-1),
        "{error:?}"
    );
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.prepare_artifact(&initial, &cancel),
        Err(AdapterError::Cancelled { .. })
    ));
    let changed = fixture.rewrite(&[ORIGINAL, b" "].concat());
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, GLSL, &budget),
                &retained,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}

#[test]
fn body_only_edits_preserve_written_identity_and_change_current_source_hash() {
    let first = run(SIMPLE.as_bytes(), EncodingId::utf8());
    let changed = SIMPLE.replace("return selected(input1);", "return selected(input1)+1.0;");
    let second = run(changed.as_bytes(), EncodingId::utf8());
    let identities = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|entity| {
                (
                    entity.id,
                    (entity.kind, entity.canonical_name.clone(), entity.container),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    assert_eq!(identities(&first), identities(&second));
    assert_ne!(
        first.document().files[0].content_hash,
        second.document().files[0].content_hash
    );
    let target = defined(&second, "selected", EntityKind::Function, 6, 14);
    let start = u64::try_from(changed.rfind("selected").unwrap()).unwrap();
    use_at(&second, OccurrenceRole::CallSite, start, start + 8, target);
    let signatures = |output: &AnalysisOutput| {
        output
            .document()
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let value = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                (value.kind() == rootlight_ir::LexicalEvidenceKind::Signature)
                    .then(|| (value.subject(), value.text().to_owned()))
            })
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(signatures(&first), signatures(&second));
}

#[test]
fn raw_missing_duplicate_foreign_and_disconnected_witnesses_cannot_bind() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, SyntaxKindLabel, testkit::MockParseProvider,
    };
    let native = provider();
    let fixture = Fixture::new(GLSL, ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, GLSL, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(GLSL),
            GLSL.frontend,
            content_hash(b"glsl-witness-replay"),
        )
        .unwrap();
        analyze(&frontend, &input, &ExtensionSupport::default())
    };
    let baseline = replay(parsed.facts().to_vec());
    let formal = defined(&baseline, "color", EntityKind::Parameter, 2449, 2454);
    use_at(&baseline, OccurrenceRole::Reference, 2473, 2478, formal);
    let function = defined(&baseline, "luminance", EntityKind::Function, 2434, 2443);
    use_at(&baseline, OccurrenceRole::CallSite, 2788, 2797, function);
    let witnesses: Vec<_> = parsed
        .facts()
        .iter()
        .filter_map(|fact| {
            let end = match (
                fact.kind(),
                fact.syntax_kind().as_str(),
                fact.span().start_byte(),
                fact.span().end_byte(),
            ) {
                (SyntaxFactKind::Signature, "glsl.call_target.signature", _, 2797) => 2797,
                (SyntaxFactKind::Signature, "glsl.formal_target.signature", _, 2478) => 2478,
                (SyntaxFactKind::Declaration, "glsl.function.declaration", 2428 | 2716, _) => 2797,
                (SyntaxFactKind::Occurrence, "glsl.name.definition", 2434 | 2722, _) => 2797,
                (SyntaxFactKind::Declaration, "glsl.parameter.declaration", 2444, _) => 2478,
                (SyntaxFactKind::Occurrence, "glsl.name.definition", 2449, _) => 2478,
                _ => return None,
            };
            Some((fact, end))
        })
        .collect();
    assert_eq!(witnesses.len(), 8);
    for (witness, end) in witnesses {
        let use_role = if end == 2797 {
            OccurrenceRole::CallSite
        } else {
            OccurrenceRole::Reference
        };
        for mode in 0..4 {
            let mut facts = parsed.facts().to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.local_id() == witness.local_id())
                .unwrap();
            match mode {
                0 => {
                    facts.remove(index);
                    if witness.kind() == SyntaxFactKind::Declaration {
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
                    }
                }
                1 => {
                    let next = facts
                        .iter()
                        .map(SyntaxFact::local_id)
                        .max()
                        .unwrap()
                        .checked_add(1)
                        .unwrap();
                    facts.push(SyntaxFact::new(
                        next,
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    ));
                }
                2 => {
                    facts[index] = SyntaxFact::new(
                        witness.local_id(),
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        SyntaxKindLabel::new("glsl.foreign_target.signature").unwrap(),
                    );
                }
                _ => {
                    let root = parsed
                        .facts()
                        .iter()
                        .find(|fact| fact.kind() == SyntaxFactKind::Root)
                        .unwrap()
                        .local_id();
                    facts[index] = SyntaxFact::new(
                        witness.local_id(),
                        Some(root),
                        witness.kind(),
                        witness.span(),
                        1,
                        witness.syntax_kind().clone(),
                    );
                }
            }
            let output = replay(facts);
            assert!(
                !output
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.role == use_role
                        && site.source.span().end_byte() == end
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                "{:?} mode{mode}",
                witness.kind()
            );
        }
    }
}

#[test]
fn real_relation_record_and_nested_limits_have_exact_then_one_less_boundaries() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, GLSL);
    let fixture = Fixture::new(GLSL, SIMPLE.as_bytes());
    let run = |ir| {
        let base = limits();
        let budget = AnalysisLimits::new(
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
        execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, GLSL, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = run(IrLimits::default()).unwrap();
    assert_eq!(
        baseline
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    for dimension in 0..4 {
        let default = IrLimits::default();
        let mut low = 1;
        let mut high = match dimension {
            0 => default.max_relations,
            1 => default.max_total_records,
            2 => default.max_total_nested_items,
            _ => default.max_nested_items_per_record,
        };
        let set = |maximum| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_relations = maximum,
                1 => ir.max_total_records = maximum,
                2 => ir.max_total_nested_items = maximum,
                _ => ir.max_nested_items_per_record = maximum,
            };
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("{error:?}"),
            }
        }
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource,observed,limit}) if observed>limit && limit==low-1 && resource==if dimension<2 {ResourceKind::Records}else{ResourceKind::NestedItems}),
            "{error:?}"
        );
    }
}

#[test]
fn same_owner_call_marker_requires_the_exact_function_declaration_parent() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let source = b"float owned(float input0){return owned(input0);}";
    let native = provider();
    let fixture = Fixture::new(GLSL, source);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, GLSL, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let owner = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "glsl.function.declaration"
        })
        .unwrap();
    let marker = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "glsl.call_target.signature")
        .unwrap();
    let file_scope = parsed
        .facts()
        .iter()
        .find(|fact| {
            fact.kind() == SyntaxFactKind::Scope && fact.syntax_kind().as_str() == "glsl.file.scope"
        })
        .unwrap();
    assert_eq!(marker.parent(), Some(owner.local_id()));
    assert_ne!(owner.local_id(), file_scope.local_id());
    let replay = |facts| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(parser),
            producer_identity(),
            language(GLSL),
            GLSL.frontend,
            content_hash(b"glsl-same-owner-marker-replay"),
        )
        .unwrap();
        analyze(&frontend, &input, &ExtensionSupport::default())
    };
    let baseline = replay(parsed.facts().to_vec());
    let function = defined(&baseline, "owned", EntityKind::Function, 6, 11);
    let call_start =
        u64::try_from(std::str::from_utf8(source).unwrap().rfind("owned").unwrap()).unwrap();
    use_at(
        &baseline,
        OccurrenceRole::CallSite,
        call_start,
        call_start + 5,
        function,
    );
    assert_eq!(
        baseline
            .document()
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(function)
                && edge.object == RelationEndpoint::Entity(function))
            .count(),
        1
    );
    let mut forged = parsed.facts().to_vec();
    let index = forged
        .iter()
        .position(|fact| fact.local_id() == marker.local_id())
        .unwrap();
    forged[index] = SyntaxFact::new(
        marker.local_id(),
        Some(file_scope.local_id()),
        marker.kind(),
        marker.span(),
        file_scope.depth().checked_add(1).unwrap(),
        marker.syntax_kind().clone(),
    );
    let rejected = replay(forged);
    assert!(
        !rejected
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
    assert!(
        rejected
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == call_start
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}
