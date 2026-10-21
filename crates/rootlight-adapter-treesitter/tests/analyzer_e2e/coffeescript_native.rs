//! Full original CoffeeScript obligations and explicit finite structural boundaries.
//! These tests require the reviewed scanner status bridge before native admission.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/coffeescript/line.coffee");
const COFFEE: LanguageCase = LanguageCase {
    name: "coffeescript",
    path: "written.coffee",
    frontend: "coffeescript-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(COFFEE, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(COFFEE),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, COFFEE),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.repository(), fixture.source.repository());
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
        assert_eq!(occurrence.source.content_hash(), content_hash(bytes));
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
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact {kind:?} {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition must resolve to its own entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    symbol
}

fn original_contract(document: &rootlight_ir::NormalizedIrDocument, physical: impl Fn(u64) -> u64) {
    let class = entity_at(
        document,
        "Line",
        EntityKind::Class,
        physical(70),
        physical(74),
    );
    let first = entity_at(
        document,
        "_declareDefaults",
        EntityKind::Method,
        physical(89),
        physical(105),
    );
    let second = entity_at(
        document,
        "_draw",
        EntityKind::Method,
        physical(155),
        physical(160),
    );
    let local = entity_at(
        document,
        "radiusX",
        EntityKind::Variable,
        physical(168),
        physical(175),
    );
    for (parent, child) in [(class, first), (class, second), (second, local)] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|row| row.predicate == RelationPredicate::Contains
                    && row.subject == RelationEndpoint::Entity(parent)
                    && row.object == RelationEndpoint::Entity(child))
                .count(),
            1
        );
        assert_eq!(
            document
                .entities
                .iter()
                .find(|row| row.id == child)
                .unwrap()
                .container,
            Some(rootlight_ir::ContainerRef::Entity(parent))
        );
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|row| row.role == OccurrenceRole::Definition
                && row.source.span().start_byte() == physical(462)
                && row.source.span().end_byte() == physical(466))
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn original_full_file_preserves_four_exact_definitions_three_owners_and_export_negative() {
    assert_eq!(ORIGINAL.len(), 467);
    let result = output(ORIGINAL, EncodingId::utf8());
    original_contract(result.document(), |offset| offset);
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}

#[test]
fn original_bom_and_crlf_keep_physical_hash_and_all_written_owners() {
    let mut raw = b"\xef\xbb\xbf".to_vec();
    for byte in ORIGINAL {
        if *byte == b'\n' {
            raw.push(b'\r');
        }
        raw.push(*byte);
    }
    let result = output(&raw, EncodingId::utf8());
    original_contract(result.document(), |offset| {
        3 + offset
            + u64::try_from(
                ORIGINAL[..usize::try_from(offset).unwrap()]
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count(),
            )
            .unwrap()
    });
}

#[test]
fn outer_parameters_reassignments_members_and_constructors_never_become_local_variables() {
    for source in [
        b"value = 0\nclass Written\n  owned: ->\n    value = 1\n".as_slice(),
        b"class Written\n  owned: (value) ->\n    value = 1\n".as_slice(),
        b"class Written\n  owned: ->\n    value = 1\n    value = 2\n".as_slice(),
        b"class Written\n  constructor: ->\n    @value = 1\n".as_slice(),
    ] {
        let result = output(source, EncodingId::utf8());
        assert!(
            !result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.reason == SkippedRegionReason::ParseError),
            "{source:?}"
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable
                    && entity.canonical_name == "value")
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Method
                    && entity.canonical_name == "constructor")
        );
    }
}

#[test]
fn unresolved_names_and_all_three_semantic_gaps_remain_explicit() {
    let result = output(ORIGINAL, EncodingId::utf8());
    original_contract(result.document(), |offset| offset);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(result.document().skipped_regions.iter().any(|gap|gap.domain==domain&&gap.reason==SkippedRegionReason::UnsupportedConstruct&&gap.detail=="coffeescript-clean-written-class-method-and-unique-local-assignment-owners-only-binding-inheritance-imports-and-runtime-unavailable"));
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Reference)
    );
    assert!(!result.document().occurrences.iter().any(|site| matches!(
        site.role,
        OccurrenceRole::Reference | OccurrenceRole::Read | OccurrenceRole::CallSite
    ) && matches!(
        site.target,
        OccurrenceTarget::Resolved { .. }
    )));
}
#[test]
fn original_utf16_bom_definitions_keep_exact_physical_offsets_and_negative_content() {
    let mut bytes = vec![0xff, 0xfe];
    for unit in std::str::from_utf8(ORIGINAL).unwrap().encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    let result = output(&bytes, EncodingId::new("utf-16le").unwrap());
    original_contract(result.document(), |offset| 2 + offset * 2);
}

#[test]
fn native_required_artifact_exact_replay_and_one_less_admission_preserve_all_originals() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, COFFEE);
    let fixture = Fixture::new(COFFEE, ORIGINAL);
    let base = limits();
    let initial = request(&fixture.snapshot, &fixture.source, COFFEE, &base);
    let (full, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    original_contract(full.document(), |offset| offset);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 0);
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert!(
        artifact.syntax_fact_count() > required,
        "ordinary unresolved identifiers stay in the optional partition"
    );
    let exact_limits = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, COFFEE, &exact_limits);
    let (bounded, retained) = analyzer
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    original_contract(bounded.document(), |offset| offset);
    assert_eq!(
        retained.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, COFFEE, &exact_limits);
    let replay = analyzer
        .analyze_from_artifact(
            &next_request,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
    assert_eq!(replay.document(), fresh.document());
    let small = limits_with_syntax_records(required - 1);
    let Err(error) = analyzer.prepare_artifact(
        &request(&fixture.snapshot, &fixture.source, COFFEE, &small),
        &deadline(),
    ) else {
        panic!("one missing required fact must refuse atomically")
    };
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
        "{error:?}"
    );
}

#[test]
fn cancelled_native_request_returns_typed_cancellation_without_an_artifact() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, COFFEE);
    let fixture = Fixture::new(COFFEE, ORIGINAL);
    let base = limits();
    let stopped = Cancellation::new();
    assert!(stopped.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, COFFEE, &base),
            &stopped
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn captured_artifact_rejects_changed_source_instead_of_reusing_old_owner_proof() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, COFFEE);
    let fixture = Fixture::new(COFFEE, ORIGINAL);
    let base = limits();
    let artifact = analyzer
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, COFFEE, &base),
            &deadline(),
        )
        .unwrap();
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("Line", "OtherLine");
    let next = fixture.rewrite(changed.as_bytes());
    assert_ne!(next.source.content_hash(), fixture.source.content_hash());
    assert!(
        matches!(analyzer.analyze_from_artifact(&request(&next.snapshot, &next.source, COFFEE, &base), &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn complete_definition_contains_and_gap_closure_obeys_exact_ir_quotas() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, COFFEE);
    let fixture = Fixture::new(COFFEE, ORIGINAL);
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
                &request(&fixture.snapshot, &fixture.source, COFFEE, &limits),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    original_contract(run(IrLimits::default()).unwrap().document(), |offset| {
        offset
    });
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
            if run(set(middle)).is_ok() {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        assert!(low > 0);
        let exact = run(set(low)).unwrap();
        validate_ir_document(exact.document(), &set(low), &ExtensionSupport::default()).unwrap();
        original_contract(exact.document(), |offset| offset);
        let error = run(set(low - 1))
            .expect_err("one less than actual complete IR demand refuses atomically");
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource, observed, limit }) if observed > limit && limit == low - 1 && resource == if dimension < 2 { ResourceKind::Records } else { ResourceKind::NestedItems }),
            "{error:?}"
        );
    }
}
