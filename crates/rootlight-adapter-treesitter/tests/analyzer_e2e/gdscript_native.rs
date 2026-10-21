//! Full original GDScript obligations and explicit finite structural boundaries.
//! These tests require the reviewed scanner status bridge before native admission.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/gdscript/GroupCel.gd");
const GD: LanguageCase = LanguageCase {
    name: "gdscript",
    path: "written.gd",
    frontend: "gdscript-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(GD, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(GD),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, GD),
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
    let owner = entity_at(
        document,
        "GroupCel",
        EntityKind::Class,
        physical(11),
        physical(19),
    );
    for (name, kind, start, end) in [
        ("_init", EntityKind::Constructor, 181, 186),
        ("get_image", EntityKind::Method, 281, 290),
        ("get_class_name", EntityKind::Method, 384, 398),
    ] {
        let member = entity_at(document, name, kind, physical(start), physical(end));
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
        let source = relations[0].evidence.source.as_ref().unwrap();
        assert!(
            source.span().start_byte() <= physical(start)
                && source.span().end_byte() >= physical(end)
        );
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == physical(422)
                && site.source.span().end_byte() == physical(430))
    );
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::RefersTo
    )));
}

#[test]
fn original_file_preserves_constructor_two_methods_three_contains_and_literal_negative() {
    assert_eq!(ORIGINAL.len(), 433);
    let result = output(ORIGINAL, EncodingId::utf8());
    original_contract(result.document(), |offset| offset);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.domain == FactDomain::Relations
            )
    );
}

#[test]
fn functions_without_class_name_remain_observable_with_an_explicit_owner_gap() {
    let bytes = b"extends Node\nfunc available(value):\n\treturn value\n";
    let result = output(bytes, EncodingId::utf8());
    let source = std::str::from_utf8(bytes).unwrap();
    let at = u64::try_from(source.find("available").unwrap()).unwrap();
    entity_at(
        result.document(),
        "available",
        EntityKind::Function,
        at,
        at + 9,
    );
    assert!(!result.document().entities.iter().any(|entity| matches!(
        entity.kind,
        EntityKind::Class | EntityKind::Method | EntityKind::Constructor
    )));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "gdscript-explicit-file-class-name-owner-unavailable")
    );
    let at = u64::try_from(source.rfind("value").unwrap()).unwrap();
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == at
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn duplicate_or_recovered_owner_cannot_promote_direct_functions_to_methods() {
    for source in [
        "class_name First\nclass_name Second\nfunc available(value):\n\treturn value\n",
        "class_name Written\nfunc broken(\n",
    ] {
        let result = output(source.as_bytes(), EncodingId::utf8());
        assert!(!result.document().entities.iter().any(|entity| matches!(
            entity.kind,
            EntityKind::Class | EntityKind::Method | EntityKind::Constructor
        )));
        assert!(!result.document().skipped_regions.is_empty());
    }
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
fn body_edits_preserve_written_ids_while_rebinding_generation_and_source_hash() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, GD);
    let fixture = Fixture::new(GD, ORIGINAL);
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("return \"GroupCel\"", "return \"OtherCel\"");
    let next = fixture.rewrite(changed.as_bytes());
    let limits = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, GD, &limits),
        &ExtensionSupport::default(),
    );
    let second = analyze(
        &analyzer,
        &request(&next.snapshot, &next.source, GD, &limits),
        &ExtensionSupport::default(),
    );
    assert_ne!(fixture.source.content_hash(), next.source.content_hash());
    assert_ne!(fixture.source.generation(), next.source.generation());
    for name in ["GroupCel", "_init", "get_image", "get_class_name"] {
        let left = first
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let right = second
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(left.id, right.id, "{name}");
        assert_eq!(right.generation, next.source.generation());
        assert_eq!(
            right.evidence.source.as_ref().unwrap().content_hash(),
            next.source.content_hash()
        );
    }
}

#[test]
fn native_required_artifact_exact_replay_and_one_less_admission_preserve_all_originals() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, GD);
    let fixture = Fixture::new(GD, ORIGINAL);
    let base = limits();
    let initial = request(&fixture.snapshot, &fixture.source, GD, &base);
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
    let exact = request(&fixture.snapshot, &fixture.source, GD, &exact_limits);
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
    let next_request = request(&next.snapshot, &next.source, GD, &exact_limits);
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
        &request(&fixture.snapshot, &fixture.source, GD, &small),
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
    let analyzer = analyzer(&provider, GD);
    let fixture = Fixture::new(GD, ORIGINAL);
    let base = limits();
    let stopped = Cancellation::new();
    assert!(stopped.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, GD, &base),
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
    let analyzer = analyzer(&provider, GD);
    let fixture = Fixture::new(GD, ORIGINAL);
    let base = limits();
    let artifact = analyzer
        .prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, GD, &base),
            &deadline(),
        )
        .unwrap();
    let changed = std::str::from_utf8(ORIGINAL)
        .unwrap()
        .replace("GroupCel", "OtherCel");
    let next = fixture.rewrite(changed.as_bytes());
    assert_ne!(next.source.content_hash(), fixture.source.content_hash());
    assert!(
        matches!(analyzer.analyze_from_artifact(&request(&next.snapshot, &next.source, GD, &base), &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
}

#[test]
fn complete_definition_contains_and_gap_closure_obeys_exact_ir_quotas() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, GD);
    let fixture = Fixture::new(GD, ORIGINAL);
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
                &request(&fixture.snapshot, &fixture.source, GD, &limits),
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
