//! Full licensed Meson source and finite native binding qualifications.
//! Recovery elsewhere remains observable; the iterable certificate is lexical,
//! and does not evaluate get_option, includes or the project namespace.
use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/meson/meson.build");
const MESON: LanguageCase = LanguageCase {
    name: "meson",
    path: "meson.build",
    frontend: "meson-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const SIMPLE: &[u8] = b"items = [1]\nforeach item : items\nendforeach\n";

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(MESON, bytes);
    let bounds = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(MESON),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &bounds,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, MESON),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    for site in &result.document().occurrences {
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
        assert_eq!(site.source.content_hash(), content_hash(bytes));
        assert!(site.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    result
}

fn definition(result: &AnalysisOutput, name: &str, start: u64, end: u64) -> SymbolId {
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        sites.len(),
        1,
        "exact written variable {name}@{start}..{end}"
    );
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition resolves")
    };
    let entity = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Variable);
    assert_eq!(entity.language, "meson");
    assert_eq!(entity.canonical_name, name);
    assert_eq!(
        entity.container,
        Some(rootlight_ir::ContainerRef::File(
            sites[0].source.span().file()
        ))
    );
    symbol
}

fn reference(result: &AnalysisOutput, target: SymbolId, start: u64, end: u64) {
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].syntax_kind, "meson.adjacent_iterable.reference");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert!(sites[0].enclosing.is_none());
    assert_eq!(
        result
            .document()
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(sites[0].id)
                && relation.object == RelationEndpoint::Entity(target))
            .count(),
        1
    );
}

#[test]
fn original_full_file_keeps_two_exact_variables_and_the_foreach_iterable_reference() {
    assert_eq!(ORIGINAL.len(), 1095);
    let result = output(ORIGINAL, EncodingId::utf8());
    let wraps = definition(&result, "wraps", 516, 521);
    definition(&result, "w", 553, 554);
    reference(&result, wraps, 557, 562);
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 536
                && site.source.span().end_byte() == 541)
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn original_recovery_and_all_three_partial_domains_remain_explicit() {
    let result = output(ORIGINAL, EncodingId::utf8());
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(result.document().skipped_regions.iter().any(|gap|gap.domain==domain&&gap.reason==SkippedRegionReason::UnsupportedConstruct&&gap.detail=="meson-clean-written-top-level-bindings-and-adjacent-iterables-only-project-evaluation-and-recovered-regions-unavailable"));
    }
    for (start, end) in [
        (48, 49),
        (503, 504),
        (591, 616),
        (706, 731),
        (906, 907),
        (913, 914),
        (1078, 1079),
        (1091, 1092),
    ] {
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain
                        && gap.reason == SkippedRegionReason::ParseError
                        && gap.detail == "meson-native-recovered-syntax-region-not-certified"
                        && gap.source.span().start_byte() == start
                        && gap.source.span().end_byte() == end)
            );
        }
    }
    assert_eq!(
        ORIGINAL
            .windows(5)
            .enumerate()
            .filter_map(|(at, value)| (value == b"wraps").then_some(at))
            .collect::<Vec<_>>(),
        vec![516, 536, 557, 1086]
    );
    assert!(!ORIGINAL.windows(b"__rootlight_absent_d55ee6fd754af45ab2bd76778d404276df7e00db1a03af6b26321cd266131c41__".len()).any(|value|value==b"__rootlight_absent_d55ee6fd754af45ab2bd76778d404276df7e00db1a03af6b26321cd266131c41__"));
}

#[test]
fn ordinary_names_and_unrelated_siblings_preserve_only_the_adjacent_iterable() {
    for bytes in [
        SIMPLE,
        b"subdir('earlier')\nitems = get_option('items')\nforeach item : items\nendforeach\n"
            .as_ref(),
        b"project('x',)\nitems = [1]\nforeach item : items\nendforeach\n".as_ref(),
    ] {
        let text = std::str::from_utf8(bytes).unwrap();
        let result = output(bytes, EncodingId::utf8());
        let at = u64::try_from(text.find("items =").unwrap()).unwrap();
        let use_at = u64::try_from(text.rfind("items").unwrap()).unwrap();
        reference(
            &result,
            definition(&result, "items", at, at + 5),
            use_at,
            use_at + 5,
        );
    }
}

#[test]
fn intervening_effects_conditional_owners_augmented_assignments_and_shadowing_withhold_targets() {
    for bytes in [
        b"items = [1]\nsubdir('later')\nforeach item : items\nendforeach\n".as_ref(),
        b"items = [1]\nset_variable('items', [])\nforeach item : items\nendforeach\n".as_ref(),
        b"items = [1]\nother = 2\nforeach item : items\nendforeach\n".as_ref(),
        b"items += [1]\nforeach item : items\nendforeach\n".as_ref(),
        b"if true\nitems = [1]\nforeach item : items\nendforeach\nendif\n".as_ref(),
        b"items = [1]\nforeach items : items\nendforeach\n".as_ref(),
        b"items = [1]\nforeach item, item : items\nendforeach\n".as_ref(),
        b"items = [1]\nforeach item : different\nendforeach\n".as_ref(),
    ] {
        let result = output(bytes, EncodingId::utf8());
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{}",
            String::from_utf8_lossy(bytes)
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::RefersTo)
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations)
        );
    }
}

#[test]
fn utf8_bom_and_both_utf16_orders_keep_physical_original_binding() {
    let text = std::str::from_utf8(ORIGINAL).unwrap();
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
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let physical = |at| u64::try_from(2 + 2 * text[..at].encode_utf16().count()).unwrap();
        let wraps = definition(&result, "wraps", physical(516), physical(521));
        definition(&result, "w", physical(553), physical(554));
        reference(&result, wraps, physical(557), physical(562));
    }
    let mut bytes = b"\xef\xbb\xbf".to_vec();
    bytes.extend_from_slice(ORIGINAL);
    let result = output(&bytes, EncodingId::utf8());
    reference(&result, definition(&result, "wraps", 519, 524), 560, 565);
}

#[test]
fn provider_artifact_required_partition_replay_and_typed_one_less_keep_the_original() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, MESON);
    let fixture = Fixture::new(MESON, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, MESON, &bounds);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert!(required > 1);
    assert_eq!(
        required,
        native
            .required_syntax_fact_count(&input.to_parse_request(), &deadline())
            .unwrap()
    );
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, MESON, &exact);
    let (result, retained) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    reference(&result, definition(&result, "wraps", 516, 521), 557, 562);
    assert_eq!(
        retained.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, MESON, &exact);
    let replay = frontend
        .analyze_from_artifact(
            &next_input,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        replay.document(),
        analyze(&frontend, &next_input, &ExtensionSupport::default()).document()
    );
    let smaller = limits_with_syntax_records(required - 1);
    let error = frontend
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, MESON, &smaller),
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::RequiredSyntaxFacts,observed,limit})if observed==required&&limit==required-1),
        "{error:?}"
    );
}

#[test]
fn cancellation_and_changed_source_do_not_reuse_the_original_certificate() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, MESON);
    let fixture = Fixture::new(MESON, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, MESON, &bounds);
    let artifact = frontend.prepare_artifact(&input, &deadline()).unwrap();
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        frontend.analyze_from_artifact(
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
    let changed = fixture.rewrite(b"items = []\nforeach item : items\nendforeach\n");
    let input = request(&changed.snapshot, &changed.source, MESON, &bounds);
    assert!(
        matches!(frontend.analyze_from_artifact(&input,&artifact,ExtensionSupport::default(),MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,&deadline()),Err(AdapterError::ProviderFailed{code})if code.as_str()=="treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn optional_uncertified_uses_survive_without_generic_target_repair() {
    let bytes = b"items = [1]\nexternal(items)\n";
    let result = output(bytes, EncodingId::utf8());
    let at = u64::try_from(std::str::from_utf8(bytes).unwrap().rfind("items").unwrap()).unwrap();
    let uses: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().start_byte() == at
                && site.source.span().end_byte() == at + 5
                && site.role == OccurrenceRole::Reference
        })
        .collect();
    assert_eq!(uses.len(), 1);
    assert_eq!(uses[0].syntax_kind, "meson.unproved_name.reference");
    assert!(matches!(
        uses[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::RefersTo
                || relation.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn raw_duplicate_or_missing_witnesses_withhold_binding_after_a_valid_native_baseline() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
    let native = provider();
    let fixture = Fixture::new(MESON, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, MESON, &bounds);
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
            language(MESON),
            MESON.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    reference(
        &baseline,
        definition(&baseline, "wraps", 516, 521),
        557,
        562,
    );
    for (label, start) in [
        ("meson.assignment.declaration", 516),
        ("meson.name.definition", 516),
        ("meson.adjacent_iterable_context.signature", 516),
        ("meson.adjacent_iterable.reference", 557),
    ] {
        let witnesses: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == label && fact.span().start_byte() == start
            })
            .collect();
        assert_eq!(witnesses.len(), 1);
        let witness = witnesses[0];
        let mut facts = parsed.facts().to_vec();
        let id = facts
            .iter()
            .map(SyntaxFact::local_id)
            .max()
            .unwrap()
            .checked_add(1)
            .unwrap();
        facts.push(SyntaxFact::new(
            id,
            witness.parent(),
            witness.kind(),
            witness.span(),
            witness.depth(),
            witness.syntax_kind().clone(),
        ));
        let changed = replay(facts);
        assert!(
            !changed
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "duplicate {label}"
        );
    }
    let facts = parsed
        .facts()
        .iter()
        .filter(|fact| {
            !(fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "meson.adjacent_iterable_context.signature")
        })
        .cloned()
        .collect();
    let changed = replay(facts);
    assert!(
        !changed
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Reference
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn exact_relation_admission_reserves_the_certified_refers_to_before_lowering() {
    let native = Arc::new(provider());
    let frontend = analyzer(&native, MESON);
    let fixture = Fixture::new(MESON, SIMPLE);
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
                &request(&fixture.snapshot, &fixture.source, MESON, &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let count = baseline.document().relations.len();
    assert_eq!(count, 3);
    let mut exact = IrLimits::default();
    exact.max_relations = count;
    let accepted = run(exact).unwrap();
    let at = u64::try_from(std::str::from_utf8(SIMPLE).unwrap().rfind("items").unwrap()).unwrap();
    reference(&accepted, definition(&accepted, "items", 0, 5), at, at + 5);
    let mut smaller = IrLimits::default();
    smaller.max_relations = count - 1;
    let error = run(smaller).unwrap_err();
    assert!(
        matches!(error,AdapterError::Sink(SinkError::StreamLimit{resource:ResourceKind::Records,observed,limit})if observed==count&&limit==count-1),
        "{error:?}"
    );
}
