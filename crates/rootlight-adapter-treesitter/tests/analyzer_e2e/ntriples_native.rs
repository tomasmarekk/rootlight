//! Native N-Triples lowering preserves typed RDF roles over complete physical lines.
//! Resource terms remain RDF data rather than invented software definitions.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, testkit::MockParseProvider};
use rootlight_ir::{
    RDF_STATEMENT_NAMESPACE, RdfStatement, RdfTermKind, decode_rdf_statement_envelope,
};

const NTRIPLES: LanguageCase = LanguageCase {
    name: "ntriples",
    path: "src/written.nt",
    frontend: "rootlight-tree-sitter-ntriples-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(NTRIPLES, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, NTRIPLES),
        &request(&fixture.snapshot, &fixture.source, NTRIPLES, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn statements(document: &rootlight_ir::NormalizedIrDocument) -> Vec<RdfStatement> {
    let mut statements: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| extension.namespace == RDF_STATEMENT_NAMESPACE)
        .map(|extension| decode_rdf_statement_envelope(extension).unwrap())
        .collect();
    statements.sort_by_key(|statement| statement.source().span().start_byte());
    statements
}

#[test]
fn native_ntriples_emits_source_backed_roles_without_software_definitions() {
    let source =
        "_:same<https://example.test/p>\"λ😀\"@EN .\n_:same <https://example.test/p> _:same .\n"
            .as_bytes();
    let result = output(source);
    let document = result.document();
    let statements = statements(document);
    assert_eq!(statements.len(), 2);
    assert_eq!(statements[0].subject().kind(), RdfTermKind::BlankNode);
    assert_eq!(
        statements[0].predicate().iri(),
        Some("https://example.test/p")
    );
    assert_eq!(statements[0].object().literal_value(), Some("λ😀"));
    assert_eq!(statements[0].object().language(), Some("en"));
    assert!(
        statements[0]
            .subject()
            .same_blank_node(statements[1].subject())
    );
    assert!(
        statements[1]
            .subject()
            .same_blank_node(statements[1].object())
    );
    for statement in statements {
        let span = statement.source().span();
        let text = std::str::from_utf8(
            source
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap(),
        )
        .unwrap();
        statement.verify_source_text(text).unwrap();
    }
    assert!(
        document
            .occurrences
            .iter()
            .all(|occurrence| occurrence.role != OccurrenceRole::Definition)
    );
    assert!(document.entities.is_empty(), "{:?}", document.entities);
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("ntriples-")
                || gap.detail == "declaration-name-unavailable"),
        "{:?}",
        document.skipped_regions
    );
}

fn metadata(fact: &SyntaxFact) -> bool {
    fact.syntax_kind().as_str().starts_with("ntriples.")
        && fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Signature
}

fn mocked_output(
    source: &[u8],
    mutate: impl FnOnce(&mut Vec<SyntaxFact>),
) -> Result<AnalysisOutput, AdapterError> {
    let native = provider();
    let fixture = Fixture::new(NTRIPLES, source);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, NTRIPLES, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let mut facts = parsed.facts().to_vec();
    mutate(&mut facts);
    let provider = MockParseProvider::new(
        native.capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(provider),
        producer_identity(),
        language(NTRIPLES),
        NTRIPLES.frontend,
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
}

fn has_gap(result: &AnalysisOutput, start: usize, end: usize) -> bool {
    result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Extensions
            && gap.detail == "ntriples-native-statement-evidence-unavailable"
            && gap.source.span().start_byte() <= u64::try_from(start).unwrap()
            && gap.source.span().end_byte() >= u64::try_from(end).unwrap()
    })
}

#[test]
fn missing_duplicate_and_swapped_roles_leave_exact_line_gaps() {
    let first = "<s:><p:><o:>.";
    let source = format!("{first}\n<next:s><next:p>\"next\".\n");
    let baseline = mocked_output(source.as_bytes(), |_| {}).unwrap();
    assert_eq!(statements(baseline.document()).len(), 2);
    for role in [
        "ntriples.statement.expression",
        "ntriples.subject.expression",
        "ntriples.predicate.expression",
        "ntriples.object.expression",
    ] {
        for duplicate in [false, true] {
            let result = mocked_output(source.as_bytes(), |facts| {
                let witness = facts
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == role
                            && fact.span().start_byte() < u64::try_from(first.len()).unwrap()
                    })
                    .unwrap()
                    .clone();
                if duplicate {
                    facts.push(SyntaxFact::new(
                        facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    ));
                } else {
                    facts.retain(|fact| fact.local_id() != witness.local_id());
                }
            })
            .unwrap();
            assert_eq!(
                statements(result.document()).len(),
                1,
                "{role} duplicate={duplicate}"
            );
            assert!(
                has_gap(&result, 0, first.len()),
                "{role} duplicate={duplicate}"
            );
        }
    }
    let swapped = mocked_output(source.as_bytes(), |facts| {
        let subject = facts
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "ntriples.subject.expression")
            .unwrap()
            .clone();
        let object = facts
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == "ntriples.object.expression")
            .unwrap()
            .clone();
        for fact in facts.iter_mut() {
            let other = if fact.local_id() == subject.local_id() {
                &object
            } else if fact.local_id() == object.local_id() {
                &subject
            } else {
                continue;
            };
            *fact = SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                fact.span(),
                fact.depth(),
                other.syntax_kind().clone(),
            );
        }
    })
    .unwrap();
    assert_eq!(statements(swapped.document()).len(), 1);
    assert!(has_gap(&swapped, 0, first.len()));
    let omitted = mocked_output(source.as_bytes(), |facts| {
        facts.retain(|fact| {
            !metadata(fact) || fact.span().start_byte() >= u64::try_from(first.len()).unwrap()
        });
    })
    .unwrap();
    assert_eq!(statements(omitted.document()).len(), 1);
    assert!(has_gap(&omitted, 0, first.len()));
}

#[test]
fn forged_comment_literal_and_substring_statements_cannot_be_promoted() {
    let fragment = "<s:><p:><o:>.";
    for (prefix, suffix) in [
        ("# ", ""),
        ("\"", "\""),
        ("<outer:s><outer:p>\"", "\" ."),
        ("unproven ", ""),
    ] {
        let decoy = format!("{prefix}{fragment}{suffix}");
        let source = format!("{decoy}\n{fragment}\n");
        let real_start = u64::try_from(decoy.len() + 1).unwrap();
        let result = mocked_output(source.as_bytes(), |facts| {
            let templates: Vec<_> = facts
                .iter()
                .filter(|fact| metadata(fact) && fact.span().start_byte() >= real_start)
                .cloned()
                .collect();
            assert_eq!(templates.len(), 4);
            let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
            for (next, template) in (next..).zip(templates) {
                let start = template.span().start_byte() - real_start
                    + u64::try_from(prefix.len()).unwrap();
                let end =
                    template.span().end_byte() - real_start + u64::try_from(prefix.len()).unwrap();
                facts.push(SyntaxFact::new(
                    next,
                    template.parent(),
                    template.kind(),
                    SourceSpan::new(template.span().file(), start, end).unwrap(),
                    template.depth(),
                    template.syntax_kind().clone(),
                ));
            }
        })
        .unwrap();
        let decoded = statements(result.document());
        assert_eq!(decoded.len(), 1, "{decoy}");
        assert_eq!(
            decoded[0].source().span().start_byte(),
            real_start,
            "{decoy}"
        );
        assert!(has_gap(&result, 0, decoy.len()), "{decoy}");
    }
}

#[test]
fn physical_line_census_rejects_uncovered_tails_and_preserves_neighbors() {
    for invalid in [
        "<s:><p:><o:>. trailing",
        "<s:><p:><o:>. <graph:>",
        "<s:><p:><o:>. <a:><b:><c:>.",
        "@prefix ex: <https://example.test/> .",
    ] {
        let source = format!("{invalid}\r\n<next:s><next:p>\"valid\".\r\n");
        let result = output(source.as_bytes());
        let decoded = statements(result.document());
        assert_eq!(decoded.len(), 1, "{invalid}");
        assert_eq!(decoded[0].subject().iri(), Some("next:s"));
        assert!(has_gap(&result, 0, invalid.len()), "{invalid}");
    }
    let source =
        "\u{feff} \t<s:><p:>\"λ\". \t# comment\r\n\t# comment\r\n\r\n  <next:s><next:p><next:o>.\r";
    let result = output(source.as_bytes());
    assert_eq!(statements(result.document()).len(), 2);
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("ntriples-")
                || gap.detail == "declaration-name-unavailable"),
        "{:?}",
        result.document().skipped_regions
    );
}

#[test]
fn retained_native_artifact_matches_fresh_ir_and_requires_all_four_roles() {
    let source = "<s:><p:>\"written λ\".\n";
    let native = Arc::new(provider());
    let frontend = analyzer(&native, NTRIPLES);
    let fixture = Fixture::new(NTRIPLES, source.as_bytes());
    let wide = limits();
    let required = native
        .required_syntax_fact_count(
            &request(&fixture.snapshot, &fixture.source, NTRIPLES, &wide).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(required, 6);
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, NTRIPLES, &exact);
    let (fresh, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert_eq!(artifact.syntax_fact_count(), required);
    assert_eq!(statements(fresh.document()).len(), 1);
    for current in [&fixture, &fixture.next_generation()] {
        let current_input = request(&current.snapshot, &current.source, NTRIPLES, &exact);
        let replay = frontend
            .analyze_from_artifact(
                &current_input,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            replay.document(),
            analyze(&frontend, &current_input, &ExtensionSupport::default()).document()
        );
        assert_eq!(statements(replay.document()).len(), 1);
    }
    let low = limits_with_syntax_records(required - 1);
    assert!(matches!(frontend.prepare_artifact(
        &request(&fixture.snapshot, &fixture.source, NTRIPLES, &low), &deadline(),
    ), Err(AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
    })) if observed == required && limit == required - 1));
    let changed = fixture.rewrite(source.replace("written", "changed").as_bytes());
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, NTRIPLES, &exact),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err()
    );
}

fn with_ir_limits(ir: IrLimits) -> AnalysisLimits {
    let base = limits();
    AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        ir,
    )
    .unwrap()
}

#[test]
fn typed_rdf_extensions_are_required_and_admission_refusal_is_atomic() {
    let source = b"<s:><p:>\"written\".\n<next:s><next:p><next:o>.\n";
    let native = Arc::new(provider());
    let frontend = analyzer(&native, NTRIPLES);
    let fixture = Fixture::new(NTRIPLES, source);
    let baseline = output(source);
    let required: Vec<_> = baseline
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .collect();
    assert_eq!(statements(baseline.document()).len(), 2);
    assert!(baseline.document().entities.is_empty());
    assert_eq!(required.len(), 3);
    let mut floor = IrLimits::default();
    floor.max_extensions = required.len();
    let floor = with_ir_limits(floor);
    let result = analyze(
        &frontend,
        &request(&fixture.snapshot, &fixture.source, NTRIPLES, &floor),
        &ExtensionSupport::default(),
    );
    assert_eq!(statements(result.document()).len(), 2);
    assert_eq!(result.document().extensions.len(), required.len());
    let largest = required
        .iter()
        .filter(|extension| extension.namespace == RDF_STATEMENT_NAMESPACE)
        .map(|extension| extension.payload.len())
        .max()
        .unwrap();
    let mut short_count = IrLimits::default();
    short_count.max_extensions = required.len() - 1;
    let mut short_payload = IrLimits::default();
    short_payload.max_extension_payload_bytes = largest - 1;
    for (bounds, expected) in [
        (short_count, ResourceKind::Records),
        (short_payload, ResourceKind::ExtensionBytes),
    ] {
        let bounded = with_ir_limits(bounds);
        let refused = execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, NTRIPLES, &bounded),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        );
        assert!(
            matches!(&refused, Err(AdapterError::Sink(SinkError::StreamLimit { resource, .. })) if *resource == expected),
            "{refused:?}"
        );
    }
}

#[test]
fn projected_source_has_an_explicit_rdf_semantic_gap() {
    let source = b"<s:><p:><o:>.\n";
    let native = Arc::new(provider());
    let fixture = Fixture::new(NTRIPLES, source);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(NTRIPLES),
        EncodingId::utf8(),
        vec![IncludedRange::new(
            fixture.source.span(),
            language(NTRIPLES),
        )],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(NTRIPLES.generated);
    let result = analyze(
        &analyzer(&native, NTRIPLES),
        &input,
        &ExtensionSupport::default(),
    );
    assert!(statements(result.document()).is_empty());
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Extensions
            && gap.detail == "ntriples-whole-physical-utf8-source-required"
            && gap.source == fixture.source
    }));
}

#[test]
#[ignore = "requires explicitly supplied pinned original N-Triples corpus"]
fn original_schemaorg_statements_keep_typed_physical_source_evidence() {
    let root =
        std::env::var_os("ROOTLIGHT_NTRIPLES_CORPUS").expect("explicit original corpus parent");
    let bytes =
        fs::read(Path::new(&root).join("182_Turtle_schemaorg/data/releases/3.9/ext-bib.nt"))
            .unwrap();
    assert_eq!(bytes.len(), 19_356);
    let result = output(&bytes);
    let decoded = statements(result.document());
    assert_eq!(decoded.len(), 169);
    for statement in decoded {
        let span = statement.source().span();
        let text = std::str::from_utf8(
            &bytes[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
        )
        .unwrap();
        statement.verify_source_text(text).unwrap();
    }
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("ntriples-")
                || gap.detail == "declaration-name-unavailable"),
        "{:?}",
        result.document().skipped_regions
    );
}
