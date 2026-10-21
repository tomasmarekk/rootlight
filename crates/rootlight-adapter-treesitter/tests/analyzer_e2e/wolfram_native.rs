//! Exact original Wolfram delayed bindings and finite Module formal reads.
use super::*;
const ORIGINAL: &[u8] = include_bytes!("../fixtures/wolfram/SocketUtilities.wl");
const WF: LanguageCase = LanguageCase {
    name: "wolfram",
    path: "written.wl",
    frontend: "wolfram-native-source",
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

fn original_contract(document: &rootlight_ir::NormalizedIrDocument) {
    let owner = entity_at(document, "hmac", EntityKind::Function, 2679, 2683);
    let key = entity_at(document, "key", EntityKind::Parameter, 2684, 2687);
    let message = entity_at(document, "message", EntityKind::Parameter, 2696, 2703);
    contains(document, owner, key, 2684, 2687);
    contains(document, owner, message, 2696, 2703);
    resolved_at(document, OccurrenceRole::Read, 3018, 3021, key);
    no_definition(document, 243, 247);
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn original_delayed_head_two_string_formals_contains_and_read_are_exact() {
    assert_eq!(ORIGINAL.len(), 3944);
    for (start, end, text) in [
        (2679, 2683, b"hmac".as_slice()),
        (2684, 2687, b"key"),
        (2696, 2703, b"message"),
        (3018, 3021, b"key"),
        (243, 247, b"hmac"),
    ] {
        assert_eq!(&ORIGINAL[start..end], text);
    }
    let result = output(WF, ORIGINAL, EncodingId::utf8());
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].byte_length, 3944);
    assert_eq!(
        result.document().files[0].content_hash,
        content_hash(ORIGINAL)
    );
    original_contract(result.document());
}

#[test]
fn ordinary_module_formal_reads_do_not_depend_on_function_or_callee_spelling() {
    for text in [
        "owned[value_String] := Module[{}, external[value]];\n",
        "different[argument_String, second_String] := Module[{local}, local = helper[argument]; second];\n",
    ] {
        let result = output(WF, text.as_bytes(), EncodingId::utf8());
        let name = if text.starts_with("owned") {
            "value"
        } else {
            "argument"
        };
        let start = u64::try_from(text.find(name).unwrap()).unwrap();
        let target = entity_at(
            result.document(),
            name,
            EntityKind::Parameter,
            start,
            start + name.len() as u64,
        );
        let usage = u64::try_from(text.rfind(name).unwrap()).unwrap();
        resolved_at(
            result.document(),
            OccurrenceRole::Read,
            usage,
            usage + name.len() as u64,
            target,
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations)
        );
    }
}

#[test]
fn local_shadow_writes_dynamic_bodies_nested_binders_and_multiple_clauses_do_not_certify_reads() {
    for text in [
        "owned[value_String] := Module[{value}, external[value]];\n",
        "owned[value_String] := Module[{}, value = other; external[value]];\n",
        "owned[value_String] := Module[{}, value++; external[value]];\n",
        "owned[value_String] := Module[{}, ToExpression[\"value=other\"]; external[value]];\n",
        "owned[value_String] := Module[{}, Block[{value}, external[value]]];\n",
        "owned[value_String] := Module[{}, Module[{value}, external[value]]];\n",
        "owned[value_String, value_String] := Module[{}, external[value]];\n",
        "owned[value_String] := Module[{}, external[value]]; owned[other_String] := Module[{}, external[other]];\n",
        "owned[value_String] := Module[{}, value[other]];\n",
        "owned[value_] := Module[{}, external[value]];\n",
    ] {
        let provider = provider();
        let fixture = Fixture::new(WF, text.as_bytes());
        let budget = limits();
        let parsed = rootlight_adapter_sdk::execute_parse(
            &provider,
            &request(&fixture.snapshot, &fixture.source, WF, &budget).to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "negative must exercise clean written syntax rather than recovery: {text}"
        );
        let result = output(WF, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations)
        );
    }
}

#[test]
fn recovery_and_overlapping_comments_cannot_donate_names_or_a_formal_read() {
    for text in [
        "(*(*)*) owned[value_String] := Module[{}, external[value]];\n",
        "owned[value_String] := Module[{}, external[value];\n",
    ] {
        let result = output(WF, text.as_bytes(), EncodingId::utf8());
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Parameter))
        );
        assert!(!result.document().occurrences.iter().any(|site| matches!(
            site.target,
            OccurrenceTarget::Resolved { .. }
        ) && site.role
            == OccurrenceRole::Read));
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
                    .any(|gap| gap.domain == domain)
            );
        }
    }
}

#[test]
fn utf8_bom_and_both_utf16_orders_keep_current_physical_definition_and_read_spans() {
    let text = "(* λ 😀 *)\r\nowned[value_String] := Module[{}, external[value]];\r\n";
    let formal = text.find("value").unwrap();
    let usage = text.rfind("value").unwrap();
    let bom = [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat();
    let result = output(WF, &bom, EncodingId::utf8());
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
            WF,
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
fn required_native_and_fresh_bounded_artifact_counts_preserve_original_then_refuse_one_less() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, WF);
    let fixture = Fixture::new(WF, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, WF, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 6);
    let exact_budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, WF, &exact_budget);
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
    original_contract(output.document());
    let replay = analyzer
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), output.document());
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, WF, &exact_budget);
    let rebound = analyzer
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        rebound.document(),
        analyze(&analyzer, &next_request, &ExtensionSupport::default()).document()
    );
    let small_budget = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, WF, &small_budget);
    assert_eq!(
        provider
            .required_syntax_fact_count(&small.to_parse_request(), &deadline())
            .unwrap(),
        required,
        "required preflight measures demand independently from bounded output admission"
    );
    for error in [
        analyzer.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
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
    let stop = Cancellation::new();
    assert!(stop.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(&initial, &stop),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn ordinary_optional_names_survive_the_required_formal_identity_partition() {
    let text = b"owned[value_String] := Module[{}, external[value]];\n";
    let provider = provider();
    let fixture = Fixture::new(WF, text);
    let budget = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider,
        &request(&fixture.snapshot, &fixture.source, WF, &budget).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let word = std::str::from_utf8(text).unwrap();
    let usage = word.rfind("value").unwrap() as u64;
    let external = word.find("external").unwrap() as u64;
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(
                |fact| fact.syntax_kind().as_str() == "wolfram.formal_argument.reference"
                    && fact.span().start_byte() == usage
                    && fact.span().end_byte() == usage + 5
            )
            .count(),
        1
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(
                |fact| fact.syntax_kind().as_str() == "wolfram.unproved_name.reference"
                    && fact.span().start_byte() == external
                    && fact.span().end_byte() == external + 8
            )
            .count(),
        1
    );
    let result = output(WF, text, EncodingId::utf8());
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == external
                && site.role == OccurrenceRole::Reference
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn actual_materialization_rejects_missing_and_distinct_id_duplicate_certificate_witnesses() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let native = provider();
    let fixture = Fixture::new(WF, ORIGINAL);
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, WF, &budget);
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
                "wolfram-witness-replay",
                "1.0",
                content_hash(b"wolfram-witness-replay"),
            )
            .unwrap(),
            language(WF),
            WF.frontend,
            content_hash(b"wolfram-witness-replay-binary"),
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
    original_contract(baseline.document());
    let identities = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    // Target certificates start at their own formal, so the later message
    // certificate also covers the earlier key read. Match the exact endpoint.
    let mut covering_targets: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| {
            fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "wolfram.formal_target_context.signature"
                && fact.span().start_byte() <= 3018
                && fact.span().end_byte() >= 3021
        })
        .map(|fact| (fact.span().start_byte(), fact.span().end_byte()))
        .collect();
    covering_targets.sort_unstable();
    assert_eq!(covering_targets, [(2684, 3021), (2696, 3065)]);
    let mut mutations = Vec::new();
    for (label, start, end) in [
        ("wolfram.formal_binding_context.signature", 2679, 3692),
        ("wolfram.formal_target_context.signature", 2684, 3021),
    ] {
        let mut facts = parsed.facts().to_vec();
        let removed: Vec<_> = facts
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .map(SyntaxFact::local_id)
            .collect();
        assert_eq!(removed.len(), 1, "actual native context prerequisite");
        facts.retain(|fact| !removed.contains(&fact.local_id()));
        assert_eq!(
            facts
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Signature
                        && fact.syntax_kind().as_str() == "wolfram.formal_target_context.signature"
                        && fact.span().start_byte() == 2696
                        && fact.span().end_byte() == 3065
                })
                .count(),
            1,
            "the distinct message certificate remains available"
        );
        mutations.push(facts);
    }
    for (kind, label, start) in [
        (
            SyntaxFactKind::Declaration,
            "wolfram.parameter.declaration",
            2684,
        ),
        (
            SyntaxFactKind::Declaration,
            "wolfram.function.declaration",
            2679,
        ),
        (SyntaxFactKind::Occurrence, "wolfram.name.definition", 2684),
    ] {
        let selected: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == kind
                    && fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
            })
            .collect();
        assert_eq!(selected.len(), 1, "actual raw witness prerequisite");
        let witness = selected[0];
        let mut facts = parsed.facts().to_vec();
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
        mutations.push(facts);
    }
    for facts in mutations {
        let result = replay(facts);
        assert_eq!(
            identities(result.document()),
            identities(baseline.document()),
            "the negative must reach the actual consumer after unchanged materialization"
        );
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == 3018
                    && site.source.span().end_byte() == 3021
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert_eq!(site.source.content_hash(), content_hash(ORIGINAL));
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations
                    && gap.source == site.source
                    && gap.reason == rootlight_ir::SkippedRegionReason::UnsupportedConstruct
                    && gap.detail == "wolfram-source-owned-pattern-formal-binding-not-certified")
        );
    }
}

#[test]
fn body_only_changes_keep_written_ids_but_never_reuse_stale_source_artifacts() {
    let before = b"owned[value_String] := Module[{}, external[value]];\n";
    let after = b"owned[value_String] := Module[{}, otherExternal[value]];\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, WF);
    let fixture = Fixture::new(WF, before);
    let changed = fixture.rewrite(after);
    let budget = limits();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, WF, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let next = request(&changed.snapshot, &changed.source, WF, &budget);
    let second = analyze(&analyzer, &next, &ExtensionSupport::default());
    let owner = entity_at(first.document(), "owned", EntityKind::Function, 0, 5);
    let formal = entity_at(first.document(), "value", EntityKind::Parameter, 6, 11);
    assert_eq!(
        entity_at(second.document(), "owned", EntityKind::Function, 0, 5),
        owner
    );
    assert_eq!(
        entity_at(second.document(), "value", EntityKind::Parameter, 6, 11),
        formal
    );
    assert_ne!(fixture.source.content_hash(), changed.source.content_hash());
    assert!(
        analyzer
            .analyze_from_artifact(
                &next,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let usage = std::str::from_utf8(after).unwrap().rfind("value").unwrap() as u64;
    resolved_at(
        second.document(),
        OccurrenceRole::Read,
        usage,
        usage + 5,
        formal,
    );
}

#[test]
fn complete_wolfram_read_obeys_relation_record_and_nested_exact_boundaries() {
    let bytes = b"owned[value_String] := Module[{}, external[value]];\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, WF);
    let fixture = Fixture::new(WF, bytes);
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
                &request(&fixture.snapshot, &fixture.source, WF, &limits),
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
