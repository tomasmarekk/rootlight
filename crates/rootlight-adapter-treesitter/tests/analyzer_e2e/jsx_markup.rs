//! Public native JSX contracts keep written markup separate from lexical names.
//! These controls cover source containment only, not React or DOM evaluation.

use super::*;

const TSX: LanguageCase = LanguageCase {
    name: "typescript",
    path: "src/view.tsx",
    frontend: "tree-sitter-typescript-0.23.2",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const JSX: LanguageCase = LanguageCase {
    name: "javascript",
    path: "src/view.jsx",
    ..TSX
};

fn output(source: &str, case: LanguageCase) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    )
}

fn markup(document: &rootlight_ir::NormalizedIrDocument) -> Vec<&rootlight_ir::EntityRecord> {
    document
        .entities
        .iter()
        .filter(|entity| {
            matches!(
                entity.kind,
                EntityKind::MarkupElement | EntityKind::MarkupAttribute
            )
        })
        .collect()
}

fn assert_identity_unavailable(output: &AnalysisOutput, source: &str, start: usize, end: usize) {
    assert_identity_gap_domains(
        output,
        source,
        start,
        end,
        "jsx-written-identity-unavailable",
    );
}

fn assert_identity_gap_domains(
    output: &AnalysisOutput,
    source: &str,
    start: usize,
    end: usize,
    entity_detail: &str,
) {
    let document = output.document();
    for domain in [FactDomain::Entities, FactDomain::Occurrences] {
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.domain == domain
                    && gap.detail
                        == if domain == FactDomain::Entities {
                            entity_detail
                        } else {
                            "jsx-written-identity-unavailable"
                        }
                    && gap.source.span().start_byte() == start as u64
                    && gap.source.span().end_byte() == end as u64
            })
            .collect();
        assert_eq!(gaps.len(), 1, "{domain:?}: {:#?}", document.skipped_regions);
        let gap = gaps[0];
        assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(gap.source.repository(), document.repository);
        assert_eq!(gap.source.generation(), document.generation);
        assert_eq!(gap.source.span().file(), document.files[0].id);
        let reported = output
            .report()
            .coverage()
            .domains()
            .iter()
            .find(|coverage| coverage.domain() == domain)
            .unwrap();
        assert_eq!(reported.status(), CoverageStatus::Bounded);
        assert!(reported.skipped() > 0);
        let normalized = document
            .coverage_records
            .iter()
            .find(|coverage| coverage.domain == domain)
            .unwrap();
        assert_eq!(normalized.status, CoverageStatus::Bounded);
    }
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Relations
            && gap.detail == "jsx-runtime-and-component-binding-unavailable"
            && gap.source.span().start_byte() == start as u64
            && gap.source.span().end_byte() == end as u64
    }));
}

fn definition<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    source: &str,
    start: usize,
    name: &str,
) -> &'a rootlight_ir::EntityRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == (start + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "{name}: {:#?}", document.occurrences);
    assert_eq!(
        &source.as_bytes()[start..start + name.len()],
        name.as_bytes()
    );
    assert_eq!(
        sites[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition must have its exact entity")
    };
    document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap()
}

#[test]
fn native_jsx_names_and_direct_containers_preserve_both_languages() {
    let source = "function view(props) { return <UI.Card data-slot='body' enabled slot={<svg:path ns:flag='x' />} {...props}><custom-tag /></UI.Card>; }";
    for case in [TSX, JSX] {
        let result = output(source, case);
        let document = result.document();
        assert_eq!(markup(document).len(), 7, "{:#?}", document.entities);
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| { gap.detail == "jsx-written-identity-unavailable" }),
            "valid native JSX must not be reported unavailable"
        );
        let card = definition(document, source, source.find("UI.Card").unwrap(), "UI.Card");
        assert_eq!(
            (
                card.kind,
                card.language.as_str(),
                card.canonical_name.as_str()
            ),
            (EntityKind::MarkupElement, case.name, "UI.Card")
        );
        let attr = definition(
            document,
            source,
            source.find("data-slot").unwrap(),
            "data-slot",
        );
        assert_eq!(attr.kind, EntityKind::MarkupAttribute);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(card.id)
                    && edge.object == RelationEndpoint::Entity(attr.id))
        );
        for (name, kind) in [
            ("svg:path", EntityKind::MarkupElement),
            ("custom-tag", EntityKind::MarkupElement),
            ("ns:flag", EntityKind::MarkupAttribute),
        ] {
            assert_eq!(
                definition(document, source, source.find(name).unwrap(), name).kind,
                kind
            );
        }
        let close = source.rfind("UI.Card").unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == close as u64)
        );
        let spread = source.rfind("props").unwrap();
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == spread as u64)
        );
    }
}

#[test]
fn jsx_expressions_fragments_and_literal_lookalikes_do_not_define_markup() {
    let source = "const div = 1; const text = '<Fake wrong />'; /* <Comment /> */ function view(props) { return <><div flag={props.value}><span />{`<Template />`}</div></>; }";
    for case in [TSX, JSX] {
        let result = output(source, case);
        let document = result.document();
        let names: BTreeSet<_> = markup(document)
            .into_iter()
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(names, BTreeSet::from(["div", "flag", "span"]));
        let expression = source.find("props.value").unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == expression as u64)
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "jsx-runtime-and-component-binding-unavailable"),
            "{source}: {:#?}",
            document.skipped_regions
        );
    }
    let generic = "const value = <Widget<Result> note='v' />;";
    let result = output(generic, TSX);
    assert_eq!(markup(result.document()).len(), 2);
    let argument = generic.find("Result").unwrap();
    assert!(!result.document().occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == argument as u64
    }));
    let plain = LanguageCase {
        path: "src/view.ts",
        ..TSX
    };
    assert!(markup(output("const cast = <Result>value;", plain).document()).is_empty());
}

#[test]
fn repeated_jsx_markup_keeps_distinct_ids_and_body_independent_identity() {
    let source = "function view() { return <div id='one' id='two'><span />text<span /></div>; }";
    let changed = source
        .replace("one", "longer-value")
        .replace("text", "longer child text");
    let before = output(source, TSX);
    let after = output(&changed, TSX);
    let ids = |output: &AnalysisOutput| -> Vec<SymbolId> {
        let mut rows = markup(output.document());
        rows.sort_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte());
        rows.into_iter().map(|entity| entity.id).collect()
    };
    assert_eq!(ids(&before), ids(&after));
    assert_eq!(ids(&before).iter().collect::<BTreeSet<_>>().len(), 5);
}

#[test]
fn mismatched_or_recovered_jsx_cannot_certify_descendant_markup() {
    {
        let source = "const value = <A><B /></C>;";
        let result = output(source, TSX);
        let document = result.document();
        assert!(markup(document).is_empty(), "{:#?}", document.entities);
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "jsx-runtime-and-component-binding-unavailable"),
            "{source}: {:#?}",
            document.skipped_regions
        );
        assert_identity_unavailable(
            &result,
            source,
            source.find('<').unwrap(),
            source.rfind('>').unwrap() + 1,
        );
    }
    let source = "const value = <A enabled={ />;";
    let result = output(source, TSX);
    let document = result.document();
    assert!(markup(document).is_empty());
    let errors: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.reason == rootlight_ir::SkippedRegionReason::ParseError
                && gap.detail == "syntax-error-recovery"
                && gap.domain == FactDomain::Diagnostics
        })
        .collect();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].source.span().start_byte(), 0);
    assert_eq!(errors[0].source.span().end_byte(), source.len() as u64);
    assert_eq!(
        errors[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    assert_eq!(errors[0].source.repository(), document.repository);
    assert_eq!(errors[0].source.generation(), document.generation);
    assert_eq!(errors[0].source.span().file(), document.files[0].id);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert_eq!(
            result
                .report()
                .coverage()
                .domains()
                .iter()
                .find(|row| row.domain() == domain)
                .unwrap()
                .status(),
            CoverageStatus::Unknown
        );
        assert_eq!(
            document
                .coverage_records
                .iter()
                .find(|row| row.domain == domain)
                .unwrap()
                .status,
            CoverageStatus::Unknown
        );
    }
    for case in [TSX, JSX] {
        let escaped_element = r"const value = <\u0041 />;";
        let result = output(escaped_element, case);
        assert!(markup(result.document()).is_empty());
        assert_identity_unavailable(
            &result,
            escaped_element,
            escaped_element.find('<').unwrap(),
            escaped_element.rfind('>').unwrap() + 1,
        );
        let escaped_attribute = r"const value = <A \u0066lag='v' />;";
        let result = output(escaped_attribute, case);
        let start = escaped_attribute.find('\\').unwrap();
        let end = escaped_attribute.find("'v'").unwrap() + 3;
        assert_eq!(markup(result.document()).len(), 1);
        assert_eq!(markup(result.document())[0].canonical_name, "A");
        assert!(!result.document().occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start as u64
        }));
        assert_identity_unavailable(&result, escaped_attribute, start, end);
    }
}

#[test]
fn jsx_native_spelling_and_configured_name_bound_keep_both_identity_domains() {
    for case in [TSX, JSX] {
        for source in [
            "const value = <UI . Card />;",
            "const value = <svg : path />;",
        ] {
            let result = output(source, case);
            assert!(markup(result.document()).is_empty());
            assert_identity_unavailable(
                &result,
                source,
                source.find('<').unwrap(),
                source.rfind('>').unwrap() + 1,
            );
        }
        let source = "const value = <A ns : flag='v' />;";
        let result = output(source, case);
        assert_eq!(markup(result.document()).len(), 1);
        assert_eq!(markup(result.document())[0].canonical_name, "A");
        assert_identity_unavailable(
            &result,
            source,
            source.find("ns : flag").unwrap(),
            source.find("'v'").unwrap() + 3,
        );

        let source = format!("const value = <{} />;", "A".repeat(96));
        let provider = Arc::new(provider());
        let fixture = Fixture::new(case, source.as_bytes());
        let ordinary = limits();
        let mut ir = IrLimits::default();
        ir.max_string_bytes = 64;
        let budget = AnalysisLimits::new(
            ordinary.max_source_bytes(),
            ordinary.max_syntax_nodes(),
            ordinary.max_syntax_depth(),
            ordinary.max_embedded_ranges(),
            ordinary.max_reported_memory_bytes(),
            ordinary.syntax_stream().clone(),
            ordinary.ir_stream().clone(),
            ir,
        )
        .unwrap();
        match execute_analysis(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        ) {
            Ok(result) => {
                assert!(markup(result.document()).is_empty());
                assert_identity_gap_domains(
                    &result,
                    &source,
                    source.find('<').unwrap(),
                    source.rfind('>').unwrap() + 1,
                    "declaration-name-unavailable",
                );
            }
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::StringBytes,
                observed,
                limit: 64,
            })) => assert!(observed > 64),
            Err(error) => panic!(
                "unexpected failure instead of explicit gap or exact caller string quota: {error:?}"
            ),
        }
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn jsx_bom_and_both_utf16_orders_keep_physical_leafs_hash_and_contains() {
    let source = "// 𝄞é\r\nfunction view() { return <Fancy data-slot='value' />; }";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut raw = if encoding == "utf-8" {
            vec![0xef, 0xbb, 0xbf]
        } else if encoding == "utf-16le" {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        if encoding == "utf-8" {
            raw.extend_from_slice(source.as_bytes());
        } else {
            for unit in source.encode_utf16() {
                raw.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(TSX, &raw);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(TSX),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(TSX.generated);
        let result = analyze(
            &analyzer(&provider, TSX),
            &input,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        let mut ids = Vec::new();
        for (name, kind) in [
            ("Fancy", EntityKind::MarkupElement),
            ("data-slot", EntityKind::MarkupAttribute),
        ] {
            let offset = source.find(name).unwrap();
            let start = if encoding == "utf-8" {
                3 + offset
            } else {
                2 + source[..offset].encode_utf16().count() * 2
            };
            let end = start
                + if encoding == "utf-8" {
                    name.len()
                } else {
                    name.encode_utf16().count() * 2
                };
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == start as u64
                        && site.source.span().end_byte() == end as u64
                })
                .unwrap();
            assert_eq!(site.source.content_hash(), content_hash(&raw));
            let OccurrenceTarget::Resolved { symbol } = site.target else {
                panic!("exact encoded definition")
            };
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!((entity.kind, entity.canonical_name.as_str()), (kind, name));
            let expected = if encoding == "utf-8" {
                name.as_bytes().to_vec()
            } else {
                name.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            };
            assert_eq!(&raw[start..end], expected);
            ids.push(symbol);
        }
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(ids[0])
                    && edge.object == RelationEndpoint::Entity(ids[1]))
        );
    }
}

#[test]
fn partial_source_ranges_do_not_certify_jsx_markup() {
    let source = "// omitted\nconst included = <A flag />;\n// omitted again\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(TSX, source.as_bytes());
    let budget = limits();
    let start = source.find("const").unwrap();
    let end = source.find(';').unwrap() + 1;
    let ranges = vec![IncludedRange::new(
        SourceSpan::new(fixture.source.span().file(), start as u64, end as u64).unwrap(),
        language(TSX),
    )];
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(TSX),
        EncodingId::utf8(),
        ranges,
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(TSX.generated);
    let result = analyze(
        &analyzer(&provider, TSX),
        &input,
        &ExtensionSupport::default(),
    );
    assert!(markup(result.document()).is_empty());
    assert_identity_unavailable(
        &result,
        source,
        source.find('<').unwrap(),
        source.find('>').unwrap() + 1,
    );
    let attribute = source.find("flag").unwrap();
    assert_identity_unavailable(&result, source, attribute, attribute + "flag".len());
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "jsx-runtime-and-component-binding-unavailable")
    );
}

#[test]
fn jsx_cancellation_and_identity_record_limits_remain_strict() {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(TSX, b"function view(){return <A first second><B/><C/></A>}");
    let budget = limits();
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    let result = execute_analysis(
        &analyzer(&provider, TSX),
        &request(&fixture.snapshot, &fixture.source, TSX, &budget),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &cancellation,
    );
    assert!(matches!(
        result,
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let budget = limits_with_syntax_records(2);
    let result = execute_analysis(
        &analyzer(&provider, TSX),
        &request(&fixture.snapshot, &fixture.source, TSX, &budget),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    );
    assert!(
        matches!(
            result,
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::RequiredSyntaxFacts,
                observed: 17,
                limit: 2
            }))
        ),
        "exact configured record refusal: {result:?}"
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}
