//! Exercise the complete original module and conservative augmented-write boundary.
//! Initial lexical identity is independent of runtime values and of the still
//! mandatory exact-use target contract tested separately below.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/python_module_updates/summarize_eval.py");

fn case() -> LanguageCase {
    LanguageCase {
        name: "python",
        path: "written.py",
        frontend: "written-source-regression",
        source: "",
        generated: false,
        body_before: "",
        body_after: "",
    }
}

fn output(bytes: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let output = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files[0].content_hash, content_hash(bytes));
    assert_eq!(
        output.document().files[0].byte_length,
        u64::try_from(bytes.len()).unwrap()
    );
    output
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
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
    assert_eq!(
        sites.len(),
        1,
        "one original Definition {name}@{start}..{end}"
    );
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("the initial declaration owns its Definition")
    };
    let entities: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.id == symbol)
        .collect();
    assert_eq!(entities.len(), 1);
    assert_eq!(entities[0].kind, EntityKind::Variable);
    assert_eq!(entities[0].canonical_name, name);
    assert_eq!(entities[0].repository, sites[0].repository);
    assert_eq!(entities[0].generation, sites[0].generation);
    assert_eq!(entities[0].provenance, sites[0].provenance);
    assert_eq!(document.occurrences.iter().filter(|site| site.role == OccurrenceRole::Definition && matches!(site.target, OccurrenceTarget::Resolved { symbol: target } if target == symbol)).count(), 1, "updates never invent further Definitions");
    symbol
}

#[test]
fn original_module_total_has_one_initial_definition_after_augmented_write() {
    assert_eq!(ORIGINAL.len(), 1106);
    assert_eq!(&ORIGINAL[535..540], b"total");
    assert_eq!(&ORIGINAL[1010..1017], b"average");
    let output = output(ORIGINAL);
    definition(output.document(), "total", 535, 540);
    definition(output.document(), "average", 1010, 1017);
}

#[test]
fn original_module_total_reference_keeps_mandatory_target_contract() {
    assert_eq!(&ORIGINAL[1020..1025], b"total");
    let output = output(ORIGINAL);
    let document = output.document();
    let symbol = definition(document, "total", 535, 540);
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1020
                && site.source.span().end_byte() == 1025
        })
        .collect();
    assert_eq!(references.len(), 1);
    assert!(
        matches!(references[0].target, OccurrenceTarget::Resolved { symbol: target } if target == symbol),
        "the exact original use must bind to its written module slot"
    );
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(references[0].id)
                && edge.object == RelationEndpoint::Entity(symbol)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].evidence.source.as_ref(),
        Some(&references[0].source)
    );
    assert_eq!(edges[0].provenance, references[0].provenance);
}

#[test]
fn renamed_updates_keep_one_written_identity_across_generations() {
    let provider = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(
        case,
        b"amount = 7\nfor entry in []:\n    amount += entry\namount *= 3\n",
    );
    let budget = limits();
    let first = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let original = definition(first.document(), "amount", 0, 6);
    let changed =
        fixture.rewrite(b"amount = 987\nfor entry in []:\n    amount += entry\namount *= 5\n");
    let next = analyze(
        &analyzer(&provider, case),
        &request(&changed.snapshot, &changed.source, case, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(original, definition(next.document(), "amount", 0, 6));
    assert_ne!(
        first.document().files[0].generation,
        next.document().files[0].generation
    );
    assert_ne!(
        first.document().files[0].content_hash,
        next.document().files[0].content_hash
    );
}

#[test]
fn ordinary_rebinding_shadow_and_dynamic_namespaces_still_refuse_binding() {
    for source in [
        "value = 1\nvalue += 2\nvalue = 3\n",
        "value += 2\nvalue = 1\n",
        "value = 1\nfor value in []:\n    value += 2\n",
        "value = 1\ndef owned():\n    value += 2\n",
        "value = 1\nclass Owned:\n    value += 2\n",
        "value = 1\nimport other as value\nvalue += 2\n",
        "value = 1\nif (value := 2):\n    value += 3\n",
        "value = 1\ndel value\nvalue += 2\n",
        "value = 1\ndef owned():\n    global value\n    value += 2\n",
        "value = 1\ndef outer():\n    value = 2\n    def inner():\n        nonlocal value\n        value += 3\n",
        "value = 1\nglobals()['value'] = 2\nvalue += 3\n",
    ] {
        let output = output(source.as_bytes());
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable
                    && entity.canonical_name == "value"),
            "{source}"
        );
    }
}

#[test]
fn unproved_augmented_calls_and_lambda_shadow_stay_unresolved() {
    let source = b"value = 1\nvalue += 2\nvalue()\nshadow = lambda value: value\n";
    let output = output(source);
    let document = output.document();
    definition(document, "value", 0, 5);
    let call = source
        .windows(7)
        .position(|bytes| bytes == b"value()")
        .unwrap();
    let shadow = source
        .windows(5)
        .rposition(|bytes| bytes == b"value")
        .unwrap();
    for (start, role) in [
        (call, OccurrenceRole::Reference),
        (call, OccurrenceRole::CallSite),
        (shadow, OccurrenceRole::Reference),
    ] {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == role
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + 5).unwrap()
            })
            .collect();
        assert!(!sites.is_empty());
        assert!(
            sites
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
    }
}

#[test]
fn augmented_identity_projects_through_bom_and_utf16_without_update_definitions() {
    let text = format!("# λ 😀\n{}", std::str::from_utf8(ORIGINAL).unwrap());
    let logical = text.find("total = 0").unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut raw = match encoding {
            "utf-8" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            _ => vec![0xfe, 0xff],
        };
        if encoding == "utf-8" {
            raw.extend_from_slice(text.as_bytes());
        } else {
            for unit in text.encode_utf16() {
                raw.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let (start, end) = if encoding == "utf-8" {
            (3 + logical, 3 + logical + 5)
        } else {
            let start = 2 + 2 * text[..logical].encode_utf16().count();
            (start, start + 10)
        };
        let case = case();
        let fixture = Fixture::new(case, &raw);
        let provider = Arc::new(provider());
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(
            &analyzer(&provider, case),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(output.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let symbol = definition(
            output.document(),
            "total",
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        );
        let logical_read = text.rfind("total").unwrap();
        let (read_start, read_end) = if encoding == "utf-8" {
            (3 + logical_read, 3 + logical_read + 5)
        } else {
            let start = 2 + 2 * text[..logical_read].encode_utf16().count();
            (start, start + 10)
        };
        reference(
            output.document(),
            symbol,
            u64::try_from(read_start).unwrap(),
            u64::try_from(read_end).unwrap(),
        );
        assert_eq!(output.document().files[0].content_hash, content_hash(&raw));
        assert_eq!(
            output.document().files[0].byte_length,
            u64::try_from(raw.len()).unwrap()
        );
    }
}

#[test]
fn augmented_write_keeps_explicit_runtime_gaps() {
    let output = output(b"value = 1\nvalue += 2\n");
    definition(output.document(), "value", 0, 5);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(output.document().skipped_regions.iter().any(|gap| gap.domain == domain && gap.detail == "python-augmented-write-value-effects-execution-and-read-target-binding-unavailable"));
    }
}

fn reference(
    document: &rootlight_ir::NormalizedIrDocument,
    target: SymbolId,
    start: u64,
    end: u64,
) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.syntax_kind, "python.module_read.reference");
    assert!(matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == target));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].repository, site.repository);
    assert_eq!(edges[0].generation, site.generation);
    assert_eq!(edges[0].provenance, site.provenance);
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
    assert_eq!(
        rootlight_ir::derive_relation_record_id(edges[0]).unwrap(),
        edges[0].id
    );
}

#[test]
fn renamed_module_slots_resolve_flat_operands_without_calls_or_runtime_value_claims() {
    for source in [
        "amount = 3\nfor entry in []:\n    amount += entry\ncopy = (amount + 1) / 2\n",
        "amount = 3\namount *= 2\ncopy = amount",
    ] {
        let output = output(source.as_bytes());
        let document = output.document();
        let target = definition(document, "amount", 0, 6);
        let start = source.rfind("amount").unwrap();
        reference(
            document,
            target,
            u64::try_from(start).unwrap(),
            u64::try_from(start + 6).unwrap(),
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read)
        );
    }
}

#[test]
fn module_name_spelling_does_not_qualify_lambda_member_call_subscript_or_conditional_uses() {
    for suffix in [
        "copy = lambda value: value\n",
        "copy = holder.value\n",
        "copy = value()\n",
        "copy = value[0]\n",
        "copy = value if ready else 0\n",
        "copy = [value for value in []]\n",
    ] {
        let source = format!("value = 1\nvalue += 2\n{suffix}");
        let output = output(source.as_bytes());
        definition(output.document(), "value", 0, 5);
        let start = source.rfind("value").unwrap();
        let sites: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + 5).unwrap()
            })
            .collect();
        assert!(!sites.is_empty(), "{source}");
        assert!(
            sites
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

#[test]
fn full_original_required_count_matches_artifact_rebind_and_exact_one_less_refusal() {
    let provider = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&provider, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 0);
    let exact_budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, case, &exact_budget);
    let (output, artifact) = frontend
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
    let target = definition(output.document(), "total", 535, 540);
    reference(output.document(), target, 1020, 1025);
    let replay = frontend
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
    let next_request = request(&next.snapshot, &next.source, case, &exact_budget);
    let rebound = frontend
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let target = definition(rebound.document(), "total", 535, 540);
    reference(rebound.document(), target, 1020, 1025);
    assert_eq!(
        rebound.document(),
        analyze(&frontend, &next_request, &ExtensionSupport::default()).document()
    );
    let small_budget = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, case, &small_budget);
    assert_eq!(
        provider
            .required_syntax_fact_count(&small.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    for error in [
        frontend.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &small.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
            "{error:?}"
        );
    }
}

#[test]
fn observe_complete_original_required_admission_and_rebound_reference_dispositions() {
    let provider = Arc::new(provider());
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let ordinary = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &ordinary);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let exact_budget = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, case, &exact_budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &exact.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    eprintln!(
        "required={required} coverage={:?} diagnostics={:?}",
        parsed.report().coverage(),
        parsed.diagnostics()
    );
    for fact in parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str().contains("module_"))
    {
        eprintln!(
            "required-frame id={} parent={:?} kind={:?} label={} span={}..{}",
            fact.local_id(),
            fact.parent(),
            fact.kind(),
            fact.syntax_kind().as_str(),
            fact.span().start_byte(),
            fact.span().end_byte()
        );
    }
    let frontend = analyzer(&provider, case);
    let (output, artifact) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    definition(output.document(), "total", 535, 540);
    eprintln!(
        "initial exact Reference={:?}",
        output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1020)
            .collect::<Vec<_>>()
    );
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, case, &exact_budget);
    let rebound = frontend
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    definition(rebound.document(), "total", 535, 540);
    eprintln!(
        "rebound exact Reference={:?}",
        rebound
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1020)
            .collect::<Vec<_>>()
    );
}

#[test]
fn comprehension_local_targets_preserve_outer_identity_without_qualifying_inner_uses() {
    for expression in [
        "[value for value in []]",
        "{value for value in []}",
        "{value: item for value, item in []}",
        "(value for value in [])",
        "[value for value in [] for item in []]",
        "[(value, other) for (value, other) in []]",
    ] {
        let source = format!("value = 1\nvalue += 2\ncopy = {expression}\nresult = value + 1\n");
        let result = output(source.as_bytes());
        let target = definition(result.document(), "value", 0, 5);
        let start = u64::try_from(source.rfind("value").unwrap()).unwrap();
        reference(result.document(), target, start, start + 5);
        let begin = source.find("copy =").unwrap();
        let end = begin + source[begin..].find('\n').unwrap();
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() >= u64::try_from(begin).unwrap()
                    && site.source.span().end_byte() <= u64::try_from(end).unwrap()
            })
            .collect();
        assert!(!sites.is_empty(), "{source}");
        assert!(
            sites
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

#[test]
fn comprehension_walrus_and_module_loop_writes_keep_explicit_rebinding_limits() {
    for suffix in [
        "for value in []:\n    pass\n",
        "copy = [(value := item) for item in []]\n",
        "copy = [[(value := item) for item in []] for unused in []]\n",
    ] {
        let source = format!("value = 1\nvalue += 2\n{suffix}result = value + 1\n");
        let result = output(source.as_bytes());
        assert!(
            !result.document().occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == 0
                    && site.source.span().end_byte() == 5
            }),
            "{source}"
        );
        let start = u64::try_from(source.rfind("value").unwrap()).unwrap();
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference && site.source.span().start_byte() == start
            })
            .collect();
        assert!(!sites.is_empty(), "{source}");
        assert!(
            sites
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{source}"
        );
    }
}

// Append to the existing Python module-update public regressions.
// Coverage admission cannot substitute for the exact native/source witness joins.

fn assert_original_module_read_unresolved(document: &rootlight_ir::NormalizedIrDocument) {
    definition(document, "total", 535, 540);
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 1020
                && site.source.span().end_byte() == 1025
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].syntax_kind, "python.module_read.reference");
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
    }));
}

#[test]
fn original_module_read_coverage_and_required_witnesses_are_independent() {
    use rootlight_adapter_sdk::{
        CoverageReport, SyntaxFact, SyntaxFactKind, testkit::MockParseProvider,
    };

    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let original = parsed.facts();
    let root = original
        .iter()
        .find(|fact| fact.kind() == SyntaxFactKind::Root)
        .unwrap();
    for label in [
        "python.module_binding_context.signature",
        "python.module_target.signature",
        "python.module_read.reference",
    ] {
        assert_eq!(
            original
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == label)
                .count(),
            1,
            "the real native baseline supplies {label}"
        );
    }

    for status in [
        CoverageStatus::Complete,
        CoverageStatus::Bounded,
        CoverageStatus::Sampled,
        CoverageStatus::Unknown,
    ] {
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            status,
            ORIGINAL.len(),
            ORIGINAL.len(),
            usize::from(status != CoverageStatus::Complete),
            Vec::new(),
        )
        .unwrap();
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            original.to_vec(),
            parsed.diagnostics().to_vec(),
            coverage,
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            producer_identity(),
            language(case),
            case.frontend,
            content_hash(BINARY_SEED),
        )
        .unwrap();
        let (output, artifact) = frontend
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &budget);
        let rebound = frontend
            .analyze_from_artifact(
                &next_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        for document in [output.document(), rebound.document()] {
            validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
            assert_eq!(document.files[0].content_hash, content_hash(ORIGINAL));
            assert_eq!(document.files[0].byte_length, 1106);
            if matches!(status, CoverageStatus::Complete | CoverageStatus::Bounded) {
                let target = definition(document, "total", 535, 540);
                reference(document, target, 1020, 1025);
            } else {
                assert_original_module_read_unresolved(document);
            }
        }
        assert_eq!(
            output.document().files[0].generation,
            fixture.source.generation()
        );
        assert_eq!(
            rebound.document().files[0].generation,
            next.source.generation()
        );
        assert_ne!(
            output.document().files[0].generation,
            rebound.document().files[0].generation
        );
    }

    for label in [
        "python.module_binding_context.signature",
        "python.module_target.signature",
    ] {
        for mutation in ["missing", "duplicate", "reparented"] {
            let mut facts = original.to_vec();
            let index = facts
                .iter()
                .position(|fact| fact.syntax_kind().as_str() == label)
                .unwrap();
            let fact = facts[index].clone();
            match mutation {
                "missing" => {
                    facts.remove(index);
                }
                "duplicate" => {
                    let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                    facts.push(SyntaxFact::new(
                        id,
                        fact.parent(),
                        fact.kind(),
                        fact.span(),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    ));
                }
                "reparented" => {
                    facts[index] = SyntaxFact::new(
                        fact.local_id(),
                        Some(root.local_id()),
                        fact.kind(),
                        fact.span(),
                        root.depth() + 1,
                        fact.syntax_kind().clone(),
                    );
                }
                _ => unreachable!(),
            }
            let coverage = CoverageReport::new(
                AnalysisTier::TierD,
                CoverageStatus::Bounded,
                ORIGINAL.len(),
                ORIGINAL.len(),
                1,
                Vec::new(),
            )
            .unwrap();
            let mock = MockParseProvider::new(
                native.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                coverage,
            );
            let frontend = TreeSitterAnalyzer::new(
                Arc::new(mock),
                producer_identity(),
                language(case),
                case.frontend,
                content_hash(BINARY_SEED),
            )
            .unwrap();
            match rootlight_adapter_sdk::execute_analysis(
                &frontend,
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            ) {
                Ok(output) => {
                    assert_original_module_read_unresolved(output.document());
                    validate_ir_document(
                        output.document(),
                        budget.ir(),
                        &ExtensionSupport::default(),
                    )
                    .unwrap();
                }
                Err(AdapterError::ProviderFailed { code })
                    if mutation == "reparented"
                        && code.as_str() == "treesitter-lowering-parent" => {}
                Err(error) => panic!("{label}/{mutation}: unexpected refusal {error:?}"),
            }
        }
    }
}
