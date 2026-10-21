//! Exact original SML/WAT contracts and guarded native-source behavior.
use super::*;
const SML_SOURCE: &[u8] = include_bytes!("../fixtures/standard_ml/deephandle.sml");
const WAT_SOURCE: &[u8] = include_bytes!("../fixtures/webassembly/sieve.wat");
const SML: LanguageCase = LanguageCase {
    name: "standard-ml",
    path: "written.sml",
    frontend: "sml-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const WAT: LanguageCase = LanguageCase {
    name: "webassembly",
    path: "written.wat",
    frontend: "wat-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
fn output(case: LanguageCase, bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
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
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(bytes));
        assert_eq!(occurrence.source.repository(), fixture.source.repository());
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
        assert!(occurrence.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    result
}

fn entity_at(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        definitions.len(),
        1,
        "one exact {kind:?} definition: {name}@{start}..{end}"
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must resolve to its own materialized entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    symbol
}

fn contains(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    member: SymbolId,
    start: u64,
    end: u64,
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == member)
        .unwrap();
    assert_eq!(
        entity.container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
    );
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    let source = relations[0]
        .evidence
        .source
        .as_ref()
        .expect("native ownership has exact source evidence");
    assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
}

fn resolved_at(
    document: &rootlight_ir::NormalizedIrDocument,
    role: OccurrenceRole,
    start: u64,
    end: u64,
    target: SymbolId,
) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|s| {
            s.role == role
                && s.source.span().start_byte() == start
                && s.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
}
fn no_definition(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64) {
    assert!(
        !document
            .occurrences
            .iter()
            .any(|s| s.role == OccurrenceRole::Definition
                && s.source.span().start_byte() == start
                && s.source.span().end_byte() == end)
    );
}
fn exact_call(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    target: SymbolId,
    start: u64,
    end: u64,
) {
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|r| {
            r.predicate == RelationPredicate::Calls
                && r.subject == RelationEndpoint::Entity(owner)
                && r.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    let source = edges[0].evidence.source.as_ref().unwrap();
    assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
}
#[test]
fn original_standard_ml_function_formal_recursive_call_read_and_negatives_remain_exact() {
    assert_eq!(SML_SOURCE.len(), 139);
    let result = output(SML, SML_SOURCE, EncodingId::utf8());
    let d = result.document();
    let function = entity_at(d, "f", EntityKind::Function, 4, 5);
    let parameter = entity_at(d, "i", EntityKind::Parameter, 6, 7);
    contains(d, function, parameter, 6, 7);
    resolved_at(d, OccurrenceRole::Read, 16, 17, parameter);
    resolved_at(d, OccurrenceRole::CallSite, 65, 66, function);
    exact_call(d, function, function, 65, 66);
    no_definition(d, 16, 17);
    assert!(
        !d.relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::Contains
                && r.subject == RelationEndpoint::Entity(parameter)
                && r.object == RelationEndpoint::Entity(function))
    );
    assert_eq!(
        SML_SOURCE
            .windows(b"handle".len())
            .filter(|w| *w == b"handle")
            .count(),
        2
    );
    assert_eq!(
        SML_SOURCE
            .windows(b"handle__rootlight_literal_absent__".len())
            .filter(|w| *w == b"handle__rootlight_literal_absent__")
            .count(),
        0
    );
}
#[test]
fn original_wat_named_module_function_formal_owner_and_nondefinition_are_exact() {
    assert_eq!(WAT_SOURCE.len(), 1174);
    let result = output(WAT, WAT_SOURCE, EncodingId::utf8());
    let d = result.document();
    entity_at(d, "$sieve-raw", EntityKind::Module, 8, 18);
    let function = entity_at(d, "$sieve", EntityKind::Function, 45, 51);
    let parameter = entity_at(d, "$n", EntityKind::Parameter, 76, 78);
    contains(d, function, parameter, 76, 78);
    // The exact negative and literal denominators below are source-authored originals.
    no_definition(d, 217, 219);
    assert_eq!(
        WAT_SOURCE
            .windows("$sieve-raw".len())
            .filter(|w| *w == "$sieve-raw".as_bytes())
            .count(),
        1
    );
    assert_eq!(WAT_SOURCE.windows("__rootlight_absent_3b8673e8419cc00fdfc890b9b5e03378397fd66941dfbcbaa39932202e639d68__".len()).filter(|w| *w == "__rootlight_absent_3b8673e8419cc00fdfc890b9b5e03378397fd66941dfbcbaa39932202e639d68__".as_bytes()).count(), 0);
}
#[test]
fn sml_shadowing_open_fixity_and_unproved_binders_keep_structural_headers_without_false_bindings() {
    for text in [
        "open External; fun measure value = value;",
        "infix 6 custom; fun measure value = measure (value);",
        "fun measure value = (fn value => value) value;",
        "fun measure value = value handle value => value;",
        "fun measure value = let val local_value = value in local_value end;",
    ] {
        let result = output(SML, text.as_bytes(), EncodingId::utf8());
        let d = result.document();
        let at = u64::try_from(text.find("measure").unwrap()).unwrap();
        entity_at(d, "measure", EntityKind::Function, at, at + 7);
        assert!(
            !d.occurrences.iter().any(|s| matches!(
                s.role,
                OccurrenceRole::Read | OccurrenceRole::CallSite
            ) && matches!(
                s.target,
                OccurrenceTarget::Resolved { .. }
            )),
            "{text}"
        );
        assert!(
            !d.relations
                .iter()
                .any(|r| r.predicate == RelationPredicate::Calls),
            "{text}"
        );
        assert!(
            d.skipped_regions
                .iter()
                .any(|s| s.domain == rootlight_ir::FactDomain::Relations)
        );
    }
}
#[test]
fn wat_type_parameters_local_binders_and_opcodes_do_not_supply_written_entities() {
    let text = "(module (type $signature (func (param $documentation i32))) (func $measure (param $value i32) (local $local i32) local.get $value))";
    let result = output(WAT, text.as_bytes(), EncodingId::utf8());
    let d = result.document();
    for name in ["$signature", "$documentation", "$local", "local.get"] {
        assert!(
            !d.entities.iter().any(|e| e.canonical_name == name),
            "{name}"
        );
    }
    let at = u64::try_from(text.find("$measure").unwrap()).unwrap();
    entity_at(d, "$measure", EntityKind::Function, at, at + 8);
    let at = u64::try_from(text.find("$value").unwrap()).unwrap();
    entity_at(d, "$value", EntityKind::Parameter, at, at + 6);
}
#[test]
fn utf8_bom_crlf_and_both_utf16_orders_preserve_nonascii_prefix_and_exact_physical_formals() {
    for (case, text, name) in [
        (SML, "(* λ😀 *)\r\nfun measure value = value;\r\n", "value"),
        (
            WAT,
            "(; λ😀 ;)\r\n(module $written (func $measure (param $value i32) local.get $value))\r\n",
            "$value",
        ),
    ] {
        let at = text.find(name).unwrap();
        let mut raw = b"\xef\xbb\xbf".to_vec();
        raw.extend_from_slice(text.as_bytes());
        let result = output(case, &raw, EncodingId::utf8());
        let start = u64::try_from(at + 3).unwrap();
        entity_at(
            result.document(),
            name,
            EntityKind::Parameter,
            start,
            start + u64::try_from(name.len()).unwrap(),
        );
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
            let encoding = EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap();
            let result = output(case, &raw, encoding);
            let start = u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
            let target = entity_at(
                result.document(),
                name,
                EntityKind::Parameter,
                start,
                start + u64::try_from(name.len() * 2).unwrap(),
            );
            if case.name == "standard-ml" {
                let last = text.rfind(name).unwrap();
                let start = u64::try_from(2 + 2 * text[..last].encode_utf16().count()).unwrap();
                resolved_at(
                    result.document(),
                    OccurrenceRole::Read,
                    start,
                    start + 10,
                    target,
                );
            }
        }
    }
}
#[test]
fn exact_required_records_keep_original_semantic_proofs_and_one_less_is_a_typed_refusal() {
    for (case, bytes) in [(SML, SML_SOURCE), (WAT, WAT_SOURCE)] {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, bytes);
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let required = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        assert!(required > 1);
        let exact = limits_with_syntax_records(required);
        let query = request(&fixture.snapshot, &fixture.source, case, &exact);
        let (result, _) = analyzer
            .analyze_and_capture(
                &query,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        if case.name == "standard-ml" {
            let owner = entity_at(result.document(), "f", EntityKind::Function, 4, 5);
            let formal = entity_at(result.document(), "i", EntityKind::Parameter, 6, 7);
            contains(result.document(), owner, formal, 6, 7);
            resolved_at(result.document(), OccurrenceRole::Read, 16, 17, formal);
            exact_call(result.document(), owner, owner, 65, 66);
        } else {
            let owner = entity_at(result.document(), "$sieve", EntityKind::Function, 45, 51);
            let formal = entity_at(result.document(), "$n", EntityKind::Parameter, 76, 78);
            contains(result.document(), owner, formal, 76, 78);
        }
        let small = limits_with_syntax_records(required - 1);
        let error = analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &small),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
            "{error:?}"
        );
        let cancelled = Cancellation::new();
        assert!(cancelled.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            analyzer.prepare_artifact(&initial, &cancelled),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
#[test]
fn unrelated_recovery_keeps_a_clean_written_sml_header_and_both_profiles_report_gaps() {
    let text = b"fun measure value = value;\nval broken = ;\n";
    let result = output(SML, text, EncodingId::utf8());
    entity_at(result.document(), "measure", EntityKind::Function, 4, 11);
    for (case, source) in [(SML, SML_SOURCE), (WAT, WAT_SOURCE)] {
        let result = output(case, source, EncodingId::utf8());
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|r| r.domain == domain)
            );
        }
    }
}

#[test]
fn body_only_edits_preserve_function_and_formal_identity_with_fresh_generation_and_hashes() {
    for (case, before, after, function, formal) in [
        (
            SML,
            "fun measure value = value + 1;\n",
            "fun measure value = value + 2;\n",
            "measure",
            "value",
        ),
        (
            WAT,
            "(module $written (func $measure (param $value i32) (drop (i32.const 1))))\n",
            "(module $written (func $measure (param $value i32) (drop (i32.const 2))))\n",
            "$measure",
            "$value",
        ),
    ] {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let budget = limits();
        let fixture = Fixture::new(case, before.as_bytes());
        let first = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        let next = fixture.rewrite(after.as_bytes());
        let second = analyze(
            &analyzer,
            &request(&next.snapshot, &next.source, case, &budget),
            &ExtensionSupport::default(),
        );
        assert_ne!(fixture.source.generation(), next.source.generation());
        assert_ne!(fixture.source.content_hash(), next.source.content_hash());
        for (name, kind) in [
            (function, EntityKind::Function),
            (formal, EntityKind::Parameter),
        ]
        .into_iter()
        .filter(|(_, kind)| case.name != "standard-ml" || *kind == EntityKind::Function)
        {
            let left = first
                .document()
                .entities
                .iter()
                .find(|e| e.kind == kind && e.canonical_name == name)
                .unwrap();
            let right = second
                .document()
                .entities
                .iter()
                .find(|e| e.kind == kind && e.canonical_name == name)
                .unwrap();
            assert_eq!(
                left.id, right.id,
                "proof context/body bytes cannot become signature identity"
            );
            assert_eq!(right.generation, next.source.generation());
            assert_eq!(
                right.evidence.source.as_ref().unwrap().content_hash(),
                next.source.content_hash()
            );
        }
    }
}
#[test]
fn wat_written_ownership_obeys_tight_record_relation_and_nested_admission() {
    let bytes = b"(module $written (func $measure (param $value i32) local.get $value))\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, WAT);
    let fixture = Fixture::new(WAT, bytes);
    let base = limits();
    let run = |ir: IrLimits| {
        let limits = AnalysisLimits::new(
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
        analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, WAT, &limits),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let owner = entity_at(
        baseline.document(),
        "$measure",
        EntityKind::Function,
        23,
        31,
    );
    let formal = entity_at(baseline.document(), "$value", EntityKind::Parameter, 39, 45);
    contains(baseline.document(), owner, formal, 39, 45);
    for dimension in 0..4 {
        let default = IrLimits::default();
        let mut low = 1;
        let mut high = match dimension {
            0 => default.max_relations,
            1 => default.max_total_records,
            2 => default.max_total_nested_items,
            _ => default.max_nested_items_per_record,
        };
        let set = |limit| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_relations = limit,
                1 => ir.max_total_records = limit,
                2 => ir.max_total_nested_items = limit,
                _ => ir.max_nested_items_per_record = limit,
            };
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected qualification error: {error:?}"),
            }
        }
        let one_less = low.checked_sub(1).expect("positive admission boundary");
        eprintln!("WAT IR admission boundary: dimension={dimension}, required={low}");
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(one_less)).unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit }) if observed > limit && limit == one_less && resource == if dimension < 2 { ResourceKind::Records } else { ResourceKind::NestedItems }),
            "{error:?}"
        );
    }
}

#[test]
fn available_written_function_heads_keep_exact_physical_encoding_and_required_quota() {
    for (case, text, name) in [
        (
            SML,
            "(* λ😀 *)\r\nfun measure value = value;\r\n",
            "measure",
        ),
        (
            WAT,
            "(; λ😀 ;)\r\n(module $written (func $measure (param $value i32) local.get $value))\r\n",
            "$measure",
        ),
    ] {
        let at = text.find(name).unwrap();
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
            let result = output(
                case,
                &bytes,
                EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            );
            let start = u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
            entity_at(
                result.document(),
                name,
                EntityKind::Function,
                start,
                start + u64::try_from(name.len() * 2).unwrap(),
            );
        }
        let fixture = Fixture::new(case, text.as_bytes());
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let base = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &base);
        let required = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        assert!(required > 1);
        let exact = limits_with_syntax_records(required);
        let (result, _) = analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &exact),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let at = u64::try_from(at).unwrap();
        entity_at(
            result.document(),
            name,
            EntityKind::Function,
            at,
            at + u64::try_from(name.len()).unwrap(),
        );
        let small = limits_with_syntax_records(required - 1);
        let error = analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &small),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err();
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::RequiredSyntaxFacts,observed,limit})if observed==required&&limit==required-1),
            "{error:?}"
        );
    }
}

#[test]
fn original_sml_recursive_call_has_its_own_independent_exact_source_gate() {
    assert_eq!(SML_SOURCE.len(), 139);
    let result = output(SML, SML_SOURCE, EncodingId::utf8());
    let d = result.document();
    let function = entity_at(d, "f", EntityKind::Function, 4, 5);
    resolved_at(d, OccurrenceRole::CallSite, 65, 66, function);
    exact_call(d, function, function, 65, 66);
    let site = d
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::CallSite
                && site.source.span().start_byte() == 65
                && site.source.span().end_byte() == 66
        })
        .unwrap();
    assert_eq!(site.syntax_kind, "sml.own_function_call.reference");
    assert_eq!(site.syntactic_text_hash, content_hash(b"f"));
    assert_eq!(site.enclosing, Some(function));
    let edge = d
        .relations
        .iter()
        .find(|edge| edge.predicate == RelationPredicate::Calls)
        .unwrap();
    assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
    assert!(
        edge.evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Entity(function))
    );
    assert!(
        edge.evidence
            .derivation
            .contains(&rootlight_ir::FactRef::Fact(site.id))
    );
    no_definition(d, 16, 17);
    assert!(
        d.skipped_regions
            .iter()
            .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations
                && gap.detail.contains("incoming-basis"))
    );
}

#[test]
fn sml_recursion_does_not_cross_argument_local_handler_or_fixity_bindings() {
    for text in [
        "fun measure measure = measure 1;",
        "fun measure value = (fn measure => measure value) measure;",
        "fun measure value = let val measure = external in measure value end;",
        "fun measure value = value handle measure => measure value;",
        "fun measure value = External.measure value;",
        "fun measure value = external value;",
        "open External; fun measure value = measure value;",
        "infix 6 custom; fun measure value = measure value;",
        "fun measure (value, other) = measure value;",
    ] {
        let result = output(SML, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls),
            "{text}"
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
    }
}

#[test]
fn sml_recursion_keeps_both_utf16_orders_in_physical_source_coordinates() {
    let text = "(* λ😀 *)\r\nfun measure value = measure value;\r\n";
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
        let result = output(
            SML,
            &raw,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let declaration = text.find("measure").unwrap();
        let usage = text.rfind("measure").unwrap();
        let function = entity_at(
            result.document(),
            "measure",
            EntityKind::Function,
            physical(declaration),
            physical(declaration + 7),
        );
        resolved_at(
            result.document(),
            OccurrenceRole::CallSite,
            physical(usage),
            physical(usage + 7),
            function,
        );
        exact_call(
            result.document(),
            function,
            function,
            physical(usage),
            physical(usage + 7),
        );
    }
}

#[test]
fn sml_required_recursion_native_preflight_artifact_exact_and_replay_agree() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SML);
    let fixture = Fixture::new(SML, SML_SOURCE);
    let initial_limits = limits();
    let initial = request(&fixture.snapshot, &fixture.source, SML, &initial_limits);
    let (baseline, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let syntax = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, SML, &syntax);
    let (bounded, retained) = analyzer
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let owner = entity_at(bounded.document(), "f", EntityKind::Function, 4, 5);
    resolved_at(bounded.document(), OccurrenceRole::CallSite, 65, 66, owner);
    exact_call(bounded.document(), owner, owner, 65, 66);
    assert_eq!(
        retained.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, SML, &syntax);
    let replayed = analyzer
        .analyze_from_artifact(
            &next_request,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
    assert_eq!(replayed.document(), fresh.document());
    assert!(!baseline.document().relations.is_empty());
    let low = limits_with_syntax_records(required.checked_sub(1).unwrap());
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, SML, &low),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })
        if observed == required && limit + 1 == observed),
        "{error:?}"
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.prepare_artifact(&initial, &cancelled),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let mut changed = SML_SOURCE.to_vec();
    changed[65] = b'g';
    let stale = Fixture::new(SML, &changed);
    assert!(
        analyzer
            .analyze_from_artifact(
                &request(&stale.snapshot, &stale.source, SML, &syntax),
                &retained,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}

#[test]
fn sml_recursion_rejects_missing_duplicate_and_disconnected_native_witnesses() {
    use rootlight_adapter_sdk::{
        SyntaxFact, SyntaxFactKind, execute_parse, testkit::MockParseProvider,
    };
    let native = provider();
    let fixture = Fixture::new(SML, SML_SOURCE);
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, SML, &budget);
    let parsed = execute_parse(
        &native,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(parser),
            ProducerIdentity::new(
                "sml-witness-replay",
                "1.0",
                content_hash(b"sml-witness-replay"),
            )
            .unwrap(),
            language(SML),
            SML.frontend,
            content_hash(b"sml-witness-replay-binary"),
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = replay(parsed.facts().to_vec()).unwrap();
    let owner = entity_at(baseline.document(), "f", EntityKind::Function, 4, 5);
    resolved_at(baseline.document(), OccurrenceRole::CallSite, 65, 66, owner);
    let witnesses: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            matches!(
                (fact.kind(), fact.syntax_kind().as_str()),
                (SyntaxFactKind::Module, "sml.file.module")
                    | (SyntaxFactKind::Scope, "sml.file.scope")
                    | (SyntaxFactKind::Declaration, "sml.function.declaration")
                    | (SyntaxFactKind::Occurrence, "sml.name.definition")
                    | (
                        SyntaxFactKind::Signature,
                        "sml.own_function_target.signature"
                    )
            )
        })
        .collect();
    assert_eq!(
        witnesses.len(),
        5,
        "one actual file Module, file Scope, owner Declaration, Definition and call marker"
    );
    for witness in witnesses {
        for mode in 0..3 {
            let mut facts = parsed.facts().to_vec();
            match mode {
                0 => {
                    facts.retain(|fact| fact.local_id() != witness.local_id());
                    if witness.kind() == SyntaxFactKind::Declaration {
                        // Exercise an absent owner without introducing dangling parent IDs.
                        for fact in &mut facts {
                            if fact.parent() == Some(witness.local_id()) {
                                *fact = SyntaxFact::new(
                                    fact.local_id(),
                                    witness.parent(),
                                    fact.kind(),
                                    fact.span(),
                                    fact.depth(),
                                    fact.syntax_kind().clone(),
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
                _ => {
                    let slot = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    *slot = SyntaxFact::new(
                        witness.local_id(),
                        None,
                        witness.kind(),
                        witness.span(),
                        0,
                        witness.syntax_kind().clone(),
                    );
                }
            }
            let changed = match replay(facts) {
                Ok(changed) => changed,
                Err(rootlight_adapter_sdk::AdapterError::ProviderFailed { code })
                    if matches!(
                        witness.kind(),
                        SyntaxFactKind::Module | SyntaxFactKind::Scope
                    ) && code.as_str() == "treesitter-lowering-parent" =>
                {
                    // Broken file-owner frames may be rejected before the call consumer.
                    continue;
                }
                Err(error) => panic!("{:?} mode{mode}: {error:?}", witness.kind()),
            };
            assert!(
                !changed
                    .document()
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls),
                "{:?} mode{mode}",
                witness.kind()
            );
            assert!(
                !changed
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.role == OccurrenceRole::CallSite
                        && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                "{:?} mode{mode}",
                witness.kind()
            );
        }
    }
}

#[test]
fn sml_recursion_relation_records_and_nested_admission_have_real_exact_boundaries() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SML);
    let fixture = Fixture::new(SML, SML_SOURCE);
    let run = |ir| {
        let base = limits();
        let limits = AnalysisLimits::new(
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
        analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, SML, &limits),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let owner = entity_at(baseline.document(), "f", EntityKind::Function, 4, 5);
    resolved_at(baseline.document(), OccurrenceRole::CallSite, 65, 66, owner);
    exact_call(baseline.document(), owner, owner, 65, 66);
    for dimension in 0..4 {
        let default = IrLimits::default();
        let mut low = 1;
        let mut high = match dimension {
            0 => default.max_relations,
            1 => default.max_total_records,
            2 => default.max_total_nested_items,
            _ => default.max_nested_items_per_record,
        };
        let set = |limit| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_relations = limit,
                1 => ir.max_total_records = limit,
                2 => ir.max_total_nested_items = limit,
                _ => ir.max_nested_items_per_record = limit,
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected SML admission error: {error:?}"),
            }
        }
        let one_less = low.checked_sub(1).unwrap();
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(one_less)).unwrap_err();
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit })
            if observed > limit && limit == one_less && resource == if dimension < 2 { ResourceKind::Records } else { ResourceKind::NestedItems }),
            "{error:?}"
        );
    }
}
