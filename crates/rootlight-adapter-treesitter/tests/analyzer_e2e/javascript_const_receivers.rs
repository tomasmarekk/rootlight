//! JavaScript object receivers retain exact lexical const ownership.
//! The complete original separates a nonbinding key from its same-spelling value.

use super::*;
use rootlight_ir::ContainerRef;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/javascript_const_receiver/webpack.common.js");

fn case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "javascript").unwrap()
}

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let case = case();
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
    let result = analyze(
        &analyzer(&native, case),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn assert_binding(
    result: &AnalysisOutput,
    bytes: &[u8],
    name: &str,
    definition: (u64, u64),
    receiver: (u64, u64),
    key: Option<(u64, u64)>,
    spelling: &[u8],
) {
    let document = result.document();
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == definition.0
                && site.source.span().end_byte() == definition.1
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1);
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("const definition resolves");
    };
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(variable.kind, EntityKind::Variable);
    assert_eq!(variable.canonical_name, name);
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(variable.container, Some(ContainerRef::Entity(module.id)));
    let references = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == receiver.0
                && site.source.span().end_byte() == receiver.1
        })
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 1);
    let reference = references[0];
    assert_eq!(reference.syntax_kind, "javascript.const_receiver.reference");
    assert_eq!(reference.target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(reference.enclosing, Some(module.id));
    assert_eq!(reference.provenance, variable.provenance);
    assert_eq!(reference.source.content_hash(), content_hash(bytes));
    assert_eq!(reference.syntactic_text_hash, content_hash(spelling));
    assert_eq!(reference.evidence.source.as_ref(), Some(&reference.source));
    assert_eq!(
        reference.id,
        rootlight_ir::derive_occurrence_record_id(reference).unwrap()
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(reference.id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&reference.source))
            .count(),
        1
    );
    if let Some(key) = key {
        assert!(
            document
                .occurrences
                .iter()
                .filter(|site| site.source.span().start_byte() == key.0
                    && site.source.span().end_byte() == key.1)
                .all(|site| site.target != (OccurrenceTarget::Resolved { symbol }))
        );
    }
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::Read)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != RelationPredicate::Calls)
    );
}

#[test]
fn full_original_javascript_resolves_const_receiver_and_keeps_same_spelling_key_nonbinding() {
    assert_eq!(ORIGINAL.len(), 199);
    assert_eq!(&ORIGINAL[6..10], b"path");
    assert_eq!(&ORIGINAL[106..110], b"path");
    assert_eq!(&ORIGINAL[112..116], b"path");
    let result = output(ORIGINAL, EncodingId::utf8());
    assert_binding(
        &result,
        ORIGINAL,
        "path",
        (6, 10),
        (112, 116),
        Some((106, 110)),
        b"path",
    );
}

#[test]
fn renamed_keys_methods_and_assignment_roots_preserve_only_the_written_lexical_target() {
    for text in [
        "const sample=load('pkg'); settings.result={sample:sample.convert(base,'out')};",
        "const $input2=1; target={outer:{'$input2':$input2.finish()}};",
        "const sample=1; settings.sample={sample:sample.sample()};",
        "const sample=1; settings={first:sample.use(),second:sample.other()};",
        "/* sample = 9; */ const sample=1; settings={sample:sample.use('sample')};",
    ] {
        let name = if text.contains("$input2") {
            "$input2"
        } else {
            "sample"
        };
        let definition = text.find(&format!("{name}=")).unwrap();
        let result = output(text.as_bytes(), EncodingId::utf8());
        for (receiver, _) in text.match_indices(&format!("{name}.")) {
            if receiver < definition {
                continue;
            }
            assert_binding(
                &result,
                text.as_bytes(),
                name,
                (
                    u64::try_from(definition).unwrap(),
                    u64::try_from(definition + name.len()).unwrap(),
                ),
                (
                    u64::try_from(receiver).unwrap(),
                    u64::try_from(receiver + name.len()).unwrap(),
                ),
                None,
                name.as_bytes(),
            );
        }
    }
}

#[test]
fn unsupported_binding_owner_and_member_shapes_never_gain_the_native_receiver_label() {
    for text in [
        "let value=1; target={key:value.use()};",
        "var value=1; target={key:value.use()};",
        "target={key:value.use()}; const value=1;",
        "const value=1; const value=2; target={key:value.use()};",
        "const value=1; {const value=2; target={key:value.use()};}",
        "const value=1; value=2; target={key:value.use()};",
        "const value=1; value++; target={key:value.use()};",
        "const value=1; [value]=other; target={key:value.use()};",
        "const value=1; eval('value=2'); target={key:value.use()};",
        "const value=1; with(other){target={key:value.use()};}",
        "const value=1; const later=()=>value; target={key:value.use()};",
        "const value=1; target={key:value?.use()};",
        "const value=1; target={key:value.use?.()};",
        "const value=1; target={key:value['use']()};",
        "const value=1; target={key:value.inner.use()};",
        "const value=1; target={key:(value).use()};",
        "const value=1; target={key:value.use(...args)};",
        "const value=1; target={key:value.use(other())};",
        "const value=1; target={value};",
        "const value=1; target={key:flag?value.use():other};",
        "const value=1; value: target={key:value.use()};",
        "const value=1; target={key:/value/.test('x')};",
        "const value=1; target={key:`${value}`};",
        "const value=1; target={key:<Widget value={value}/>};",
        "const value:number=1; target={key:value.use()};",
        "const value=1; target={key:value.use<string>()};",
        "const value=1; target={key:value.use()}; value.member={};",
    ] {
        let result = output(text.as_bytes(), EncodingId::utf8());
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.syntax_kind != "javascript.const_receiver.reference"),
            "{text}"
        );
    }
}

#[test]
fn bom_and_utf16_receiver_coordinates_keep_exact_physical_spelling_and_identity() {
    let text = "const value=1; target={value:value.use()};";
    let definition = text.find("value").unwrap();
    let receiver = text.rfind("value.use").unwrap();
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(text.as_bytes());
    let result = output(&bom, EncodingId::utf8());
    assert_binding(
        &result,
        &bom,
        "value",
        (
            u64::try_from(definition + 3).unwrap(),
            u64::try_from(definition + 8).unwrap(),
        ),
        (
            u64::try_from(receiver + 3).unwrap(),
            u64::try_from(receiver + 8).unwrap(),
        ),
        None,
        b"value",
    );
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for scalar in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                scalar.to_le_bytes()
            } else {
                scalar.to_be_bytes()
            });
        }
        let result = output(&bytes, EncodingId::new(encoding).unwrap());
        assert_binding(
            &result,
            &bytes,
            "value",
            (
                u64::try_from(2 + definition * 2).unwrap(),
                u64::try_from(2 + (definition + 5) * 2).unwrap(),
            ),
            (
                u64::try_from(2 + receiver * 2).unwrap(),
                u64::try_from(2 + (receiver + 5) * 2).unwrap(),
            ),
            None,
            b"value",
        );
    }
}

#[test]
fn native_generated_and_refined_receivers_have_one_required_chain() {
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &request(&fixture.snapshot, &fixture.source, case, &budget).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(
                |fact| fact.syntax_kind().as_str() == "javascript.const_receiver.reference"
                    && fact.span().start_byte() == 112
                    && fact.span().end_byte() == 116
            )
            .count(),
        1
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str()
                == "javascript.const_receiver_target.signature"
                && fact.span().start_byte() == 6
                && fact.span().end_byte() == 116)
            .count(),
        1
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str()
                == "javascript.const_receiver_nonbinding.signature"
                && fact.span().start_byte() == 106
                && fact.span().end_byte() == 110)
            .count(),
        1
    );
}

#[test]
fn javascript_native_and_artifact_required_counts_keep_receiver_partition_at_exact_fit() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case(), &budget);
    let (_, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap()
    );
    let exact = limits_with_syntax_records(required);
    let (_, exact_artifact) = frontend
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, case(), &exact),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        exact_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        required
    );
    let smaller = limits_with_syntax_records(required - 1);
    assert!(matches!(frontend.analyze_and_capture(
        &request(&fixture.snapshot, &fixture.source, case(), &smaller), ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }))
            if observed == required && limit == required - 1));
    let successor = fixture.next_generation();
    let next = request(&successor.snapshot, &successor.source, case(), &budget);
    let reused = frontend
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&frontend, &next, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
}

#[test]
fn javascript_const_receiver_ir_reservations_keep_exact_fit_and_one_less_boundaries() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), b"const value=1; target={key:value.use()};");
    let base = limits();
    let run = |ir| {
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
            &request(&fixture.snapshot, &fixture.source, case(), &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = run(IrLimits::default()).unwrap();
    assert_eq!(
        baseline
            .document()
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "javascript.const_receiver.reference")
            .count(),
        1
    );
    for dimension in 0..4 {
        let make = |bound| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_occurrences = bound,
                1 => ir.max_relations = bound,
                2 => ir.max_total_records = bound,
                _ => ir.max_total_string_bytes = bound,
            }
            ir
        };
        let mut low = 0;
        let mut high = 16_384;
        while low < high {
            let middle = low + (high - low) / 2;
            match run(make(middle)) {
                Ok(result) => {
                    assert_eq!(result.document(), baseline.document());
                    high = middle;
                }
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected capacity result: {error:?}"),
            }
        }
        assert!(low > 0 && low < 16_384);
        assert_eq!(run(make(low)).unwrap().document(), baseline.document());
        assert!(
            matches!(run(make(low - 1)), Err(AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit }))
            if observed > limit && limit == low - 1
                && resource == if dimension == 3 { ResourceKind::StringBytes } else { ResourceKind::Records })
        );
    }
}

#[test]
fn receiver_metadata_preserves_entity_identity_and_rejects_stale_or_cancelled_artifacts() {
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("path.resolve", "path.compute");
    let initial = output(ORIGINAL, EncodingId::utf8());
    let updated = output(changed.as_bytes(), EncodingId::utf8());
    let identities = |result: &AnalysisOutput| {
        result
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
    assert_eq!(identities(&initial), identities(&updated));
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case());
    let fixture = Fixture::new(case(), ORIGINAL);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case(), &budget);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let successor = fixture.rewrite(changed.as_bytes());
    let next = request(&successor.snapshot, &successor.source, case(), &budget);
    assert!(
        frontend
            .analyze_prepared_artifact(
                &next,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let cancelled = deadline();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.analyze_prepared_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn included_range_requests_never_promote_module_receiver_binding() {
    let native = Arc::new(provider());
    let fixture = Fixture::new(case(), ORIGINAL);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case()),
        EncodingId::utf8(),
        vec![IncludedRange::new(fixture.source.span(), language(case()))],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case()),
        &input,
        &ExtensionSupport::default(),
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "javascript.const_receiver.reference")
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

fn encoded_request<'a>(
    fixture: &'a Fixture,
    budget: &'a AnalysisLimits,
    encoding: &str,
) -> AnalysisRequest<'a> {
    AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case()),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        budget,
    )
    .unwrap()
    .with_generated_status(false)
}

#[test]
fn both_bom_bound_utf16_frames_replay_only_their_current_artifact_and_preserve_receiver_evidence() {
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let encode = |text: &str| {
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
            bytes
        };
        let text = std::str::from_utf8(ORIGINAL).unwrap();
        let bytes = encode(text);
        let fixture = Fixture::new(case(), &bytes);
        let native = Arc::new(provider());
        let frontend = analyzer(&native, case());
        let budget = limits();
        let input = encoded_request(&fixture, &budget, encoding);
        let (initial, artifact) = frontend
            .analyze_and_capture(
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_binding(
            &initial,
            &bytes,
            "path",
            (14, 22),
            (226, 234),
            Some((214, 222)),
            b"path",
        );
        let successor = fixture.next_generation();
        let next = encoded_request(&successor, &budget, encoding);
        let reused = frontend
            .analyze_from_artifact(
                &next,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let fresh = analyze(&frontend, &next, &ExtensionSupport::default());
        assert_eq!(reused.document(), fresh.document());
        assert_eq!(reused.report(), fresh.report());
        assert_binding(
            &reused,
            &bytes,
            "path",
            (14, 22),
            (226, 234),
            Some((214, 222)),
            b"path",
        );
        let changed = encode(&text.replace("path.resolve", "path.compute"));
        let rewritten = fixture.rewrite(&changed);
        let stale = encoded_request(&rewritten, &budget, encoding);
        assert!(
            frontend
                .analyze_from_artifact(
                    &stale,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline()
                )
                .is_err()
        );
    }
}
