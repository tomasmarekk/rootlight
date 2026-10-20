//! Exact original Q# contracts and finite native-argument qualification.
use super::*;
const ORIGINAL: &[u8] = include_bytes!("../fixtures/qsharp/Shorthand.qs");
const QS: LanguageCase = LanguageCase {
    name: "qsharp",
    path: "written.qs",
    frontend: "qsharp-native-source",
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
fn clean(bytes: &[u8]) {
    let provider = provider();
    let fixture = Fixture::new(QS, bytes);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, QS, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider,
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_ne!(
        parsed.report().coverage().status(),
        rootlight_ir::CoverageStatus::Unknown
    );
    assert!(
        !parsed
            .facts()
            .iter()
            .any(|f| f.kind() == rootlight_adapter_sdk::SyntaxFactKind::ErrorRecovery),
        "{}",
        String::from_utf8_lossy(bytes)
    );
}
#[test]
fn original_qsharp_namespace_operation_formals_contains_read_and_source_negatives_are_exact() {
    assert_eq!(ORIGINAL.len(), 1080);
    clean(ORIGINAL);
    let result = output(QS, ORIGINAL, EncodingId::utf8());
    let d = result.document();
    let namespace = entity_at(
        d,
        "Microsoft.Quantum.Arithmetic",
        EntityKind::Namespace,
        10,
        38,
    );
    let operation = entity_at(d, "MAJ", EntityKind::Function, 776, 779);
    let first = entity_at(d, "input0", EntityKind::Parameter, 780, 786);
    let second = entity_at(d, "input1", EntityKind::Parameter, 796, 802);
    contains(d, namespace, operation, 776, 779);
    contains(d, operation, first, 780, 786);
    contains(d, operation, second, 796, 802);
    resolved_at(d, OccurrenceRole::Read, 920, 926, first);
    no_definition(d, 920, 926);
    assert_eq!(ORIGINAL.windows(3).filter(|w| *w == b"MAJ").count(), 2);
    assert_eq!(
        ORIGINAL
            .windows(b"MAJ__absent_source_control__".len())
            .filter(|w| *w == b"MAJ__absent_source_control__")
            .count(),
        0
    );
    assert!(
        !d.relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::Calls)
    );
}
#[test]
fn direct_operation_and_function_arguments_resolve_only_to_their_written_owner() {
    for keyword in ["operation", "function"] {
        let text = format!(
            "namespace Written.Space {{ {keyword} Measure(value : Int) : Unit {{ External(value); }} }}\n"
        );
        clean(text.as_bytes());
        let result = output(QS, text.as_bytes(), EncodingId::utf8());
        let start = u64::try_from(text.find("value").unwrap()).unwrap();
        let formal = entity_at(
            result.document(),
            "value",
            EntityKind::Parameter,
            start,
            start + 5,
        );
        let usage = u64::try_from(text.rfind("value").unwrap()).unwrap();
        resolved_at(
            result.document(),
            OccurrenceRole::Read,
            usage,
            usage + 5,
            formal,
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|s| s.role == OccurrenceRole::Read
                    && s.source.span().start_byte()
                        == u64::try_from(text.find("External").unwrap()).unwrap())
        );
    }
}
#[test]
fn locals_selectors_tuple_binders_duplicate_owners_and_specialization_aliases_cannot_donate_reads()
{
    for body in [
        "let other = value; External(value);",
        "mutable other = value; External(value);",
        "set value = value; External(value);",
        "Holder.External(value);",
        "External(value + value);",
    ] {
        let text =
            format!("namespace Written {{ operation Measure(value : Int) : Unit {{ {body} }} }}\n");
        clean(text.as_bytes());
        let result = output(QS, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|o| o.role == OccurrenceRole::Read
                    && matches!(o.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|g| g.domain == rootlight_ir::FactDomain::Relations)
        );
    }
    for text in [
        "namespace Written { operation Measure((value : Int, other : Int)) : Unit { External(value); } }",
        "namespace Written { operation Measure(value : Int, value : Int) : Unit { External(value); } }",
        "namespace Written { operation Measure(value : Int) : Unit { External(value); } operation Measure(other : Int) : Unit { External(other); } }",
        "namespace Written { operation Measure(value : Int) : Unit { body(alias) { External(alias); } adjoint auto; } }",
        "namespace Written { operation Measure(value : Int) : Unit { body(...) { External(value); } controlled (controls, ...) { External(value); } } }",
    ] {
        clean(text.as_bytes());
        let result = output(QS, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|o| o.role == OccurrenceRole::Read
                    && matches!(o.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
    }
}
#[test]
fn utf8_bom_crlf_and_both_utf16_orders_keep_exact_physical_formal_reads() {
    let text = "// marker λ😀\r\nnamespace Written { operation Measure(value : Int) : Unit { External(value); } }\r\n";
    let formal = text.find("value").unwrap();
    let usage = text.rfind("value").unwrap();
    let mut bytes = b"\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(text.as_bytes());
    let result = output(QS, &bytes, EncodingId::utf8());
    let at = u64::try_from(formal + 3).unwrap();
    let target = entity_at(
        result.document(),
        "value",
        EntityKind::Parameter,
        at,
        at + 5,
    );
    let at = u64::try_from(usage + 3).unwrap();
    resolved_at(result.document(), OccurrenceRole::Read, at, at + 5, target);
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
            QS,
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let at = u64::try_from(2 + 2 * text[..formal].encode_utf16().count()).unwrap();
        let target = entity_at(
            result.document(),
            "value",
            EntityKind::Parameter,
            at,
            at + 10,
        );
        let at = u64::try_from(2 + 2 * text[..usage].encode_utf16().count()).unwrap();
        resolved_at(result.document(), OccurrenceRole::Read, at, at + 10, target);
    }
}
#[test]
fn exact_required_original_facts_keep_read_and_ownership_one_less_refuses_atomically() {
    let fixture = Fixture::new(QS, ORIGINAL);
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, QS);
    let base = limits();
    let initial = request(&fixture.snapshot, &fixture.source, QS, &base);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 1);
    let exact = limits_with_syntax_records(required);
    let (result, _) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, QS, &exact),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let owner = entity_at(result.document(), "MAJ", EntityKind::Function, 776, 779);
    let formal = entity_at(result.document(), "input0", EntityKind::Parameter, 780, 786);
    contains(result.document(), owner, formal, 780, 786);
    resolved_at(result.document(), OccurrenceRole::Read, 920, 926, formal);
    let small = limits_with_syntax_records(required - 1);
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, QS, &small),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::RequiredSyntaxFacts,observed,limit})if observed==required&&limit==required-1),
        "{error:?}"
    );
    let cancel = Cancellation::new();
    assert!(cancel.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(&initial, &cancel),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}
#[test]
fn body_only_context_is_not_symbol_identity_and_never_reuses_stale_source_hash() {
    let before =
        b"namespace Written { operation Measure(value : Int) : Unit { External(value); } }\n";
    let after =
        b"namespace Written { operation Measure(value : Int) : Unit { OtherExternal(value); } }\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, QS);
    let base = limits();
    let fixture = Fixture::new(QS, before);
    let next = fixture.rewrite(after);
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, QS, &base),
        &ExtensionSupport::default(),
    );
    let second = analyze(
        &analyzer,
        &request(&next.snapshot, &next.source, QS, &base),
        &ExtensionSupport::default(),
    );
    assert_ne!(fixture.source.generation(), next.source.generation());
    assert_ne!(fixture.source.content_hash(), next.source.content_hash());
    for (kind, name) in [
        (EntityKind::Function, "Measure"),
        (EntityKind::Parameter, "value"),
    ] {
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
        assert_eq!(left.id, right.id);
        assert_eq!(right.generation, next.source.generation());
        assert_eq!(
            right.evidence.source.as_ref().unwrap().content_hash(),
            next.source.content_hash()
        );
    }
    let text = std::str::from_utf8(after).unwrap();
    let at = u64::try_from(text.find("value").unwrap()).unwrap();
    let formal = entity_at(
        second.document(),
        "value",
        EntityKind::Parameter,
        at,
        at + 5,
    );
    let at = u64::try_from(text.rfind("value").unwrap()).unwrap();
    resolved_at(second.document(), OccurrenceRole::Read, at, at + 5, formal);
}
#[test]
fn complete_read_and_refers_to_output_obeys_record_relation_and_nested_admission() {
    let bytes =
        b"namespace Written { operation Measure(value : Int) : Unit { External(value); } }\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, QS);
    let fixture = Fixture::new(QS, bytes);
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
                &request(&fixture.snapshot, &fixture.source, QS, &limits),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    assert!(
        baseline
            .document()
            .occurrences
            .iter()
            .any(|o| o.role == OccurrenceRole::Read
                && matches!(o.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        baseline
            .document()
            .relations
            .iter()
            .any(|r| r.predicate == RelationPredicate::RefersTo)
    );
    for dimension in 0..4 {
        let default = IrLimits::default();
        let mut low = 0;
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
                Err(error) => panic!("unexpected resource prerequisite: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource,observed,limit})if observed>limit&&limit==low-1&&resource==if dimension<2{ResourceKind::Records}else{ResourceKind::NestedItems}),
            "{error:?}"
        );
    }
}

// Native facts are replayed through the public SDK boundary so raw duplicates
// cannot disappear behind a manually constructed materialized-entity fixture.

#[test]
fn raw_distinct_id_own_witnesses_cannot_reuse_materialized_formal_bindings() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

    let native = provider();
    let fixture = Fixture::new(QS, ORIGINAL);
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, QS, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
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
                "qsharp-witness-replay",
                "1.0",
                content_hash(b"qsharp-witness-replay"),
            )
            .unwrap(),
            language(QS),
            QS.frontend,
            content_hash(b"qsharp-witness-replay-binary"),
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    let owner = entity_at(baseline.document(), "MAJ", EntityKind::Function, 776, 779);
    let formal = entity_at(
        baseline.document(),
        "input0",
        EntityKind::Parameter,
        780,
        786,
    );
    contains(baseline.document(), owner, formal, 780, 786);
    resolved_at(baseline.document(), OccurrenceRole::Read, 920, 926, formal);
    let identities = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    let source_site = baseline
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 920
                && site.source.span().end_byte() == 926
        })
        .unwrap();
    for endpoint in [owner, formal] {
        let entity = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == endpoint)
            .unwrap();
        let source = entity.evidence.source.as_ref().unwrap();
        let declarations: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration && fact.span() == source.span()
            })
            .collect();
        assert_eq!(
            declarations.len(),
            1,
            "actual native declaration prerequisite"
        );
        let declaration = declarations[0];
        let definitions: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str() == "qsharp.name.definition"
                    && fact.parent() == Some(declaration.local_id())
            })
            .collect();
        assert_eq!(
            definitions.len(),
            1,
            "actual native Definition prerequisite"
        );
        for witness in [declaration, definitions[0]] {
            let mut facts = parsed.facts().to_vec();
            let next = facts
                .iter()
                .map(SyntaxFact::local_id)
                .max()
                .unwrap()
                .checked_add(1)
                .unwrap();
            assert_ne!(next, witness.local_id());
            facts.push(SyntaxFact::new(
                next,
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.depth(),
                witness.syntax_kind().clone(),
            ));
            let changed = replay(facts);
            assert_eq!(
                identities(changed.document()),
                identities(baseline.document()),
                "duplicate raw witness must pass through actual materialization with the same entities"
            );
            let sites: Vec<_> = changed
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Read
                        && site.source.span() == source_site.source.span()
                })
                .collect();
            assert_eq!(sites.len(), 1);
            assert_eq!(sites[0].source, source_site.source);
            assert_eq!(sites[0].provenance, source_site.provenance);
            assert!(
                matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
                "distinct-ID duplicate {:?} for {} must withhold the formal Read",
                witness.kind(),
                entity.canonical_name
            );
            assert!(!changed.document().relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::RefersTo
                    && relation.evidence.source.as_ref() == Some(&source_site.source)
            }));
            assert!(changed.document().skipped_regions.iter().any(|gap| {
                gap.domain == rootlight_ir::FactDomain::Relations
                    && gap.source == source_site.source
                    && gap.detail
                        == "qsharp-same-owner-argument-or-external-name-binding-unavailable"
            }));
        }
    }
}

#[test]
fn real_provider_and_captured_artifact_agree_on_required_original_formal_reads() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, QS);
    let fixture = Fixture::new(QS, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, QS, &budget);
    let (full, artifact) = analyzer
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
        required,
        "native required preflight and actual retained certified Reads must agree"
    );
    let formal = entity_at(full.document(), "input0", EntityKind::Parameter, 780, 786);
    resolved_at(full.document(), OccurrenceRole::Read, 920, 926, formal);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == "qsharp.formal_argument.reference"
                    && fact.span().start_byte() == 920
                    && fact.span().end_byte() == 926
            })
            .count(),
        1,
        "paired identity captures normalize to one exact source fact"
    );
    let exact_limits = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, QS, &exact_limits);
    let (bounded, retained) = analyzer
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
    assert_eq!(
        provider
            .required_syntax_fact_count(&exact.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    resolved_at(bounded.document(), OccurrenceRole::Read, 920, 926, formal);
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, QS, &exact_limits);
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
    let small = limits_with_syntax_records(required - 1);
    let Err(error) = analyzer.prepare_artifact(
        &request(&fixture.snapshot, &fixture.source, QS, &small),
        &deadline(),
    ) else {
        panic!("one missing mandatory fact refuses atomically");
    };
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
    }) if observed == required && limit == required - 1),
        "{error:?}"
    );
}

#[test]
fn uncertified_names_survive_identity_partition_without_duplicating_certified_reads() {
    for (text, certified) in [
        (
            "namespace Written { operation Measure(value : Int) : Unit { External(value); } }\n",
            true,
        ),
        (
            "namespace Written { operation Measure(value : Int) : Unit { let other = 1; External(value); } }\n",
            false,
        ),
    ] {
        clean(text.as_bytes());
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, QS);
        let fixture = Fixture::new(QS, text.as_bytes());
        let budget = limits();
        let request = request(&fixture.snapshot, &fixture.source, QS, &budget);
        let (result, artifact) = analyzer
            .analyze_and_capture(
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let required = provider
            .required_syntax_fact_count(&request.to_parse_request(), &deadline())
            .unwrap();
        assert_eq!(
            artifact.required_syntax_fact_count(&deadline()).unwrap(),
            required
        );
        assert!(artifact.syntax_fact_count() > required);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let formal_start = u64::try_from(text.find("value").unwrap()).unwrap();
        let formal = entity_at(
            result.document(),
            "value",
            EntityKind::Parameter,
            formal_start,
            formal_start + 5,
        );
        let argument_start = u64::try_from(text.rfind("value").unwrap()).unwrap();
        let callee_start = u64::try_from(text.find("External").unwrap()).unwrap();
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.syntax_kind().as_str() == "qsharp.formal_argument.reference"
                        && fact.span().start_byte() == argument_start
                        && fact.span().end_byte() == argument_start + 5
                })
                .count(),
            usize::from(certified),
            "separate identity and optional scans normalize one certified source fact",
        );
        for (start, length) in [(callee_start, 8), (argument_start, 5)] {
            let proved_argument = certified && start == argument_start;
            let sites: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role != OccurrenceRole::Definition
                        && site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + length
                })
                .collect();
            assert_eq!(
                sites.len(),
                1,
                "ordinary source evidence must survive: {text}@{start}"
            );
            let site = sites[0];
            assert_eq!(site.source.repository(), fixture.source.repository());
            assert_eq!(site.source.generation(), fixture.source.generation());
            assert_eq!(site.source.span().file(), fixture.source.span().file());
            assert_eq!(site.source.content_hash(), content_hash(text.as_bytes()));
            assert_eq!(
                result
                    .document()
                    .provenance
                    .iter()
                    .filter(|proof| { proof.id == site.provenance })
                    .count(),
                1
            );
            if proved_argument {
                assert_eq!(site.role, OccurrenceRole::Read);
                assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: formal });
            } else {
                assert_eq!(site.role, OccurrenceRole::Reference);
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                assert_eq!(
                    parsed
                        .facts()
                        .iter()
                        .filter(|fact| {
                            fact.syntax_kind().as_str() == "qsharp.unresolved_name.reference"
                                && fact.span().start_byte() == start
                                && fact.span().end_byte() == start + length
                        })
                        .count(),
                    1
                );
                assert!(!result.document().relations.iter().any(|relation| {
                    relation.predicate == RelationPredicate::RefersTo
                        && relation.evidence.source.as_ref() == Some(&site.source)
                }));
            }
        }
    }
}
