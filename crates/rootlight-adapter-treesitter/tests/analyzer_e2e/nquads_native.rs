//! Native N-Quads lowering preserves named/default graph roles over physical lines.
//! RDF resources remain source-backed data without invented software declarations.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
use rootlight_ir::{
    RDF_STATEMENT_NAMESPACE, RdfStatement, RdfStatementFormat, RdfTermKind,
    decode_rdf_statement_envelope,
};

const NQUADS: LanguageCase = LanguageCase {
    name: "nquads",
    path: "src/written.nq",
    frontend: "rootlight-tree-sitter-nquads-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(source: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(NQUADS, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, NQUADS),
        &request(&fixture.snapshot, &fixture.source, NQUADS, &budget),
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

fn metadata(fact: &SyntaxFact) -> bool {
    fact.syntax_kind().as_str().starts_with("nquads.") && fact.kind() == SyntaxFactKind::Signature
}

fn mocked_output(source: &[u8], mutate: impl FnOnce(&mut Vec<SyntaxFact>)) -> AnalysisOutput {
    let native = provider();
    let fixture = Fixture::new(NQUADS, source);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, NQUADS, &bounds);
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
        language(NQUADS),
        NQUADS.frontend,
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
}

fn has_gap(result: &AnalysisOutput, start: usize, end: usize) -> bool {
    result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Extensions
            && gap.detail == "nquads-native-statement-evidence-unavailable"
            && gap.source.span().start_byte() <= u64::try_from(start).unwrap()
            && gap.source.span().end_byte() >= u64::try_from(end).unwrap()
    })
}

#[test]
fn native_named_and_default_graphs_preserve_physical_roles_and_blank_node_identity() {
    let source = "_:same <p:> \"λ😀\"@EN <urn:graph#fragment> .\n_:same <p:> _:same _:same .\n<s:> <p:> \"typed\"^^<urn:datatype> .\n";
    let result = output(source.as_bytes());
    let decoded = statements(result.document());
    assert_eq!(decoded.len(), 3);
    assert!(
        decoded
            .iter()
            .all(|statement| statement.format() == RdfStatementFormat::NQuads)
    );
    assert_eq!(
        decoded[0].graph().unwrap().iri(),
        Some("urn:graph#fragment")
    );
    assert_eq!(decoded[0].object().literal_value(), Some("λ😀"));
    assert_eq!(decoded[0].object().language(), Some("en"));
    assert_eq!(decoded[1].graph().unwrap().kind(), RdfTermKind::BlankNode);
    assert!(decoded[0].subject().same_blank_node(decoded[1].subject()));
    assert!(decoded[1].subject().same_blank_node(decoded[1].object()));
    assert!(
        decoded[1]
            .subject()
            .same_blank_node(decoded[1].graph().unwrap())
    );
    assert!(decoded[2].graph().is_none());
    assert_eq!(decoded[2].object().datatype(), Some("urn:datatype"));
    for statement in decoded {
        let span = statement.source().span();
        statement
            .verify_source_text(
                source
                    .get(
                        usize::try_from(span.start_byte()).unwrap()
                            ..usize::try_from(span.end_byte()).unwrap(),
                    )
                    .unwrap(),
            )
            .unwrap();
    }
    assert!(result.document().entities.is_empty());
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|record| record.role != OccurrenceRole::Definition)
    );
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("nquads-")
                || gap.detail == "declaration-name-unavailable"),
        "{:?}",
        result.document().skipped_regions
    );
    let other_case = LanguageCase {
        path: "other/written.nq",
        ..NQUADS
    };
    let other = Fixture::new(other_case, source.as_bytes());
    let native = Arc::new(provider());
    let bounds = limits();
    let different_document = analyze(
        &analyzer(&native, NQUADS),
        &request(&other.snapshot, &other.source, NQUADS, &bounds),
        &ExtensionSupport::default(),
    );
    assert!(
        !statements(result.document())[0]
            .subject()
            .same_blank_node(statements(different_document.document())[0].subject())
    );
}

#[test]
fn missing_duplicate_and_nested_graph_roles_leave_exact_line_gaps() {
    let first = "<s:><p:>\"v\"^^<urn:type><urn:graph>.";
    let source = format!("{first}\n<next:s><next:p>\"next\".\n");
    assert_eq!(
        statements(mocked_output(source.as_bytes(), |_| {}).document()).len(),
        2
    );
    for role in [
        "nquads.statement.expression",
        "nquads.subject.expression",
        "nquads.predicate.expression",
        "nquads.object.expression",
        "nquads.graph.expression",
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
            });
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
    for replacement in ["<urn:type>", "<s:>"] {
        let result = mocked_output(source.as_bytes(), |facts| {
            let offset = u64::try_from(source.find(replacement).unwrap()).unwrap();
            let graph = facts
                .iter_mut()
                .find(|fact| fact.syntax_kind().as_str() == "nquads.graph.expression")
                .unwrap();
            *graph = SyntaxFact::new(
                graph.local_id(),
                graph.parent(),
                graph.kind(),
                SourceSpan::new(
                    graph.span().file(),
                    offset,
                    offset + u64::try_from(replacement.len()).unwrap(),
                )
                .unwrap(),
                graph.depth(),
                graph.syntax_kind().clone(),
            );
        });
        assert_eq!(statements(result.document()).len(), 1);
        assert!(has_gap(&result, 0, first.len()));
    }
    let omitted = mocked_output(source.as_bytes(), |facts| {
        facts.retain(|fact| {
            !metadata(fact) || fact.span().start_byte() >= u64::try_from(first.len()).unwrap()
        })
    });
    assert_eq!(statements(omitted.document()).len(), 1);
    assert!(has_gap(&omitted, 0, first.len()));
}

#[test]
fn forged_comment_literal_and_substring_quads_cannot_be_promoted() {
    let fragment = "<s:><p:><o:><g:>.";
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
            assert_eq!(templates.len(), 5);
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
        });
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
fn physical_line_census_rejects_nonstandard_graphs_and_preserves_clean_neighbors() {
    for invalid in [
        "<s:><p:><o:>\"graph\".",
        "<s:><p:><o:><g:>. trailing",
        "<s:><p:><o:><g:><extra:>.",
        "<s:><p:><o:><relative>.",
        "<0x1> <p:> <0x2> .",
        "0x1 <p:> 0x2 .",
        "@prefix ex: <https://example.test/> .",
    ] {
        let source = format!("{invalid}\r\n<next:s><next:p>\"valid\"<next:g>.\r\n");
        let result = output(source.as_bytes());
        let decoded = statements(result.document());
        assert_eq!(decoded.len(), 1, "{invalid}");
        assert_eq!(decoded[0].subject().iri(), Some("next:s"));
        assert_eq!(decoded[0].graph().unwrap().iri(), Some("next:g"));
        assert!(has_gap(&result, 0, invalid.len()), "{invalid}");
    }
    let source = "\u{feff} \t<s:><p:>\"λ\0\"<g:>. \t# comment\0\r\n\t# comment\r\n\r\n  <next:s><next:p><next:o>.\r";
    let result = output(source.as_bytes());
    assert_eq!(statements(result.document()).len(), 2);
    assert_eq!(
        statements(result.document())[0].object().literal_value(),
        Some("λ\0")
    );
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("nquads-")
                || gap.detail == "declaration-name-unavailable"),
        "{:?}",
        result.document().skipped_regions
    );
}

#[test]
fn retained_quad_artifacts_rebind_and_require_every_written_graph_role() {
    for (source, required) in [("_:same <p:> _:same _:same.\n", 7), ("<s:><p:>\"v\".\n", 6)] {
        let native = Arc::new(provider());
        let frontend = analyzer(&native, NQUADS);
        let fixture = Fixture::new(NQUADS, source.as_bytes());
        let wide = limits();
        assert_eq!(
            native
                .required_syntax_fact_count(
                    &request(&fixture.snapshot, &fixture.source, NQUADS, &wide).to_parse_request(),
                    &deadline(),
                )
                .unwrap(),
            required
        );
        let exact = limits_with_syntax_records(required);
        let input = request(&fixture.snapshot, &fixture.source, NQUADS, &exact);
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
            let input = request(&current.snapshot, &current.source, NQUADS, &exact);
            let replay = frontend
                .analyze_from_artifact(
                    &input,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(
                replay.document(),
                analyze(&frontend, &input, &ExtensionSupport::default()).document()
            );
            let decoded = statements(replay.document());
            assert_eq!(decoded.len(), 1);
            assert_eq!(decoded[0].format(), RdfStatementFormat::NQuads);
            if let Some(graph) = decoded[0].graph() {
                assert!(graph.same_blank_node(decoded[0].subject()));
            }
        }
        let low = limits_with_syntax_records(required - 1);
        assert!(matches!(frontend.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, NQUADS, &low), &deadline(),
        ), Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
        })) if observed == required && limit == required - 1));
        let changed = fixture.rewrite(
            source
                .replace("same", "other")
                .replace("v", "changed")
                .as_bytes(),
        );
        assert!(
            frontend
                .analyze_from_artifact(
                    &request(&changed.snapshot, &changed.source, NQUADS, &exact),
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .is_err()
        );
    }
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
fn typed_quad_extension_limits_refuse_atomically() {
    let source = b"<s:><p:>\"written\"<g:>.\n<next:s><next:p><next:o>.\n";
    let native = Arc::new(provider());
    let frontend = analyzer(&native, NQUADS);
    let fixture = Fixture::new(NQUADS, source);
    let baseline = output(source);
    let required: Vec<_> = baseline
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .collect();
    assert_eq!(statements(baseline.document()).len(), 2);
    assert_eq!(required.len(), 3);
    let mut floor = IrLimits::default();
    floor.max_extensions = required.len();
    let floor = with_ir_limits(floor);
    let result = analyze(
        &frontend,
        &request(&fixture.snapshot, &fixture.source, NQUADS, &floor),
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
    let mut short_total = IrLimits::default();
    short_total.max_total_extension_bytes = required
        .iter()
        .filter(|extension| extension.namespace == RDF_STATEMENT_NAMESPACE)
        .map(|extension| extension.payload.len())
        .sum::<usize>()
        - 1;
    short_total.max_extension_payload_bytes = largest;
    for (bounds, expected) in [
        (short_count, ResourceKind::Records),
        (short_payload, ResourceKind::ExtensionBytes),
        (short_total, ResourceKind::ExtensionBytes),
    ] {
        let bounded = with_ir_limits(bounds);
        let refused = execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, NQUADS, &bounded),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        );
        assert!(
            matches!(&refused, Err(AdapterError::Sink(SinkError::StreamLimit { resource, .. }))
            if *resource == expected),
            "{refused:?}"
        );
    }
}

#[test]
fn projected_quad_source_has_explicit_semantic_gap() {
    let source = b"<s:><p:><o:><g:>.\n";
    let native = Arc::new(provider());
    let fixture = Fixture::new(NQUADS, source);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(NQUADS),
        EncodingId::utf8(),
        vec![IncludedRange::new(fixture.source.span(), language(NQUADS))],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(NQUADS.generated);
    let result = analyze(
        &analyzer(&native, NQUADS),
        &input,
        &ExtensionSupport::default(),
    );
    assert!(statements(result.document()).is_empty());
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Extensions
                && gap.detail == "nquads-whole-physical-utf8-source-required"
                && gap.source == fixture.source)
    );
}
