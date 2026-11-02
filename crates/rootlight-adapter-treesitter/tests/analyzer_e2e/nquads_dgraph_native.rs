//! Dgraph statements retain native identifiers and byte values over physical sources.

use super::*;
use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};
use rootlight_ir::{
    RDF_STATEMENT_NAMESPACE, RdfStatement, RdfStatementFormat, decode_rdf_statement_envelope,
};

const DGRAPH: LanguageCase = LanguageCase {
    name: "nquads-dgraph",
    path: "src/written.nq",
    frontend: "rootlight-tree-sitter-nquads-dgraph-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

// Apache-2.0 fixture from dgraph-io/dgraph, revision
// c587cba75f60445125459b46b4d81757a73384aa, dgraph/cmd/alpha/testdata.nq.
// License: https://github.com/dgraph-io/dgraph/blob/c587cba75f60445125459b46b4d81757a73384aa/LICENSE.txt
const ORIGINAL: &str = "<alice> <follows> <bob> .\n\
<bob> <follows> <fred> .\n\
<bob> <status> \"I'm your Uncle Bob!\" .\n\
<charlie> <follows> <bob> .\n\
<charlie> <follows> <dani> .\n\
<dani> <follows> <bob> .\n\
<dani> <follows> <greg> .\n\
<dani> <status> \"Smart Dani\" .\n\
<emily> <follows> <fred> .\n\
<fred> <follows> <greg> .\n\
<greg> <status> \"Mr. Cool greg\" .\n";

fn output(source: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(DGRAPH, source);
    let budget = limits();
    let result = analyze(
        &analyzer(&native, DGRAPH),
        &request(&fixture.snapshot, &fixture.source, DGRAPH, &budget),
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
        .map(|extension| {
            assert_eq!(extension.version, "3");
            decode_rdf_statement_envelope(extension).unwrap()
        })
        .collect();
    statements.sort_by_key(|statement| statement.source().span().start_byte());
    statements
}

fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str().starts_with("nquads-dgraph.")
}

fn mocked_output(source: &[u8], mutate: impl FnOnce(&mut Vec<SyntaxFact>)) -> AnalysisOutput {
    let native = provider();
    let fixture = Fixture::new(DGRAPH, source);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, DGRAPH, &budget);
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
        language(DGRAPH),
        DGRAPH.frontend,
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

fn has_gap(result: &AnalysisOutput, start: usize, end: usize, detail: &str) -> bool {
    result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Extensions
            && gap.detail == detail
            && gap.source.span().start_byte() <= u64::try_from(start).unwrap()
            && gap.source.span().end_byte() >= u64::try_from(end).unwrap()
    })
}

#[test]
fn original_eleven_statements_preserve_native_edges_values_and_physical_sources() {
    assert_eq!(ORIGINAL.len(), 316);
    let result = output(ORIGINAL.as_bytes());
    let decoded = statements(result.document());
    let expected = [
        ("alice", "follows", "bob"),
        ("bob", "follows", "fred"),
        ("bob", "status", "I'm your Uncle Bob!"),
        ("charlie", "follows", "bob"),
        ("charlie", "follows", "dani"),
        ("dani", "follows", "bob"),
        ("dani", "follows", "greg"),
        ("dani", "status", "Smart Dani"),
        ("emily", "follows", "fred"),
        ("fred", "follows", "greg"),
        ("greg", "status", "Mr. Cool greg"),
    ];
    assert_eq!(decoded.len(), expected.len());
    let mut offset = 0;
    let mut edges = 0;
    let mut values = 0;
    for ((statement, line), (subject, predicate, object)) in decoded
        .iter()
        .zip(ORIGINAL.split_inclusive('\n'))
        .zip(expected)
    {
        let text = line.strip_suffix('\n').unwrap();
        assert_eq!(statement.format(), RdfStatementFormat::NQuadsDgraph);
        assert!(statement.graph().is_none());
        assert_eq!(statement.subject().dgraph_identifier(), Some(subject));
        assert_eq!(statement.predicate().dgraph_identifier(), Some(predicate));
        assert_eq!(statement.source().span().start_byte(), offset);
        assert_eq!(
            statement.source().span().end_byte(),
            offset + u64::try_from(text.len()).unwrap()
        );
        statement.verify_source_text(text).unwrap();
        for term in [
            statement.subject(),
            statement.predicate(),
            statement.object(),
        ] {
            assert_eq!(
                term.source().content_hash(),
                content_hash(ORIGINAL.as_bytes())
            );
            assert!(term.iri().is_none());
            assert!(term.datatype().is_none());
            assert!(term.language().is_none());
            assert!(term.blank_node_label().is_none());
            let raw = &ORIGINAL[usize::try_from(term.source().span().start_byte()).unwrap()
                ..usize::try_from(term.source().span().end_byte()).unwrap()];
            assert!(text.contains(raw));
        }
        if predicate == "follows" {
            edges += 1;
            assert_eq!(statement.object().dgraph_identifier(), Some(object));
        } else {
            values += 1;
            assert_eq!(
                statement.object().dgraph_default_value(),
                Some(object.as_bytes())
            );
            assert!(statement.object().literal_value().is_none());
        }
        offset += u64::try_from(line.len()).unwrap();
    }
    assert_eq!((edges, values), (8, 3));
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
            .any(|gap| gap.detail.starts_with("nquads-dgraph-")
                || gap.detail == "declaration-name-unavailable"),
        "{:?}",
        result.document().skipped_regions
    );
}

#[test]
fn native_identifier_spelling_and_byte_values_never_acquire_rdf_semantics() {
    let source = concat!(
        "<0x01><follows><λ0>.\n",
        r"<a\u0020b><follows><a\UFFFFFFFF>.",
        "\n",
        r#"<alice><status>"\xFF\v\u03BB\U0001F600"."#,
        "\n",
        "<alice><status>\"raw\0value\t\".\n",
    );
    let result = output(source.as_bytes());
    let decoded = statements(result.document());
    assert_eq!(decoded.len(), 4);
    assert_eq!(decoded[0].subject().dgraph_identifier(), Some("0x01"));
    assert_eq!(decoded[0].object().dgraph_identifier(), Some("λ0"));
    assert_eq!(decoded[1].subject().dgraph_identifier(), Some(r"a\u0020b"));
    assert_eq!(
        decoded[1].object().dgraph_identifier(),
        Some(r"a\UFFFFFFFF")
    );
    let mut expected = vec![255, 11];
    expected.extend_from_slice("λ😀".as_bytes());
    assert_eq!(
        decoded[2].object().dgraph_default_value(),
        Some(expected.as_slice())
    );
    assert!(decoded[2].object().dgraph_default_text().is_none());
    assert_eq!(
        decoded[3].object().dgraph_default_value(),
        Some(b"raw\0value\t".as_slice())
    );
    for statement in decoded {
        for term in [
            statement.subject(),
            statement.predicate(),
            statement.object(),
        ] {
            assert!(term.iri().is_none());
            assert!(term.datatype().is_none());
            assert!(term.literal_value().is_none());
        }
    }
}

#[test]
fn missing_duplicate_and_misbound_roles_leave_physical_line_gaps() {
    let first = "<alice><follows><bob>.";
    let source = format!("{first}\n<next><follows><last>.\n");
    assert_eq!(
        statements(mocked_output(source.as_bytes(), |_| {}).document()).len(),
        2
    );
    for role in [
        "nquads-dgraph.statement.expression",
        "nquads-dgraph.subject.expression",
        "nquads-dgraph.predicate.expression",
        "nquads-dgraph.object.expression",
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
            assert!(has_gap(
                &result,
                0,
                first.len(),
                "nquads-dgraph-native-statement-evidence-unavailable"
            ));
        }
    }
    let result = mocked_output(source.as_bytes(), |facts| {
        let object = facts
            .iter_mut()
            .find(|fact| fact.syntax_kind().as_str() == "nquads-dgraph.object.expression")
            .unwrap();
        *object = SyntaxFact::new(
            object.local_id(),
            object.parent(),
            object.kind(),
            SourceSpan::new(object.span().file(), 0, 7).unwrap(),
            object.depth(),
            object.syntax_kind().clone(),
        );
    });
    assert_eq!(statements(result.document()).len(), 1);
    assert!(has_gap(
        &result,
        0,
        first.len(),
        "nquads-dgraph-native-statement-evidence-unavailable"
    ));
}

#[test]
fn forged_substrings_cannot_promote_comments_literals_or_unproven_prefixes() {
    let fragment = "<alice><follows><bob>.";
    for (prefix, suffix) in [
        ("# ", ""),
        ("\"", "\""),
        ("<outer><status>\"", "\" ."),
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
        });
        let decoded = statements(result.document());
        assert_eq!(decoded.len(), 1, "{decoy}");
        assert_eq!(decoded[0].source().span().start_byte(), real_start);
        assert!(has_gap(
            &result,
            0,
            decoy.len(),
            "nquads-dgraph-native-statement-evidence-unavailable"
        ));
    }
}

#[test]
fn unsupported_constructs_and_parser_recovery_preserve_clean_neighbors() {
    for unsupported in [
        "_:alice<follows><bob>.",
        "<alice><follows>_:bob.",
        "<alice><status@en>\"value\".",
        "<alice><status>\"value\"@en.",
        "<alice><status>\"value\"^^<xs:string>.",
        "<alice><follows><bob><0x10>.",
        "<alice><follows><bob>_:namespace.",
        "<alice><follows><bob>(friend=true).",
        "uid(a)<follows><bob>.",
        "<alice><follows>val(a).",
        "0x01<follows><bob>.",
        "<alice><follows><bob>. trailing",
        "<alice><status>\"unterminated",
    ] {
        let source = format!("{unsupported}\r\n<next><follows><last>.\r\n");
        let result = output(source.as_bytes());
        let decoded = statements(result.document());
        assert_eq!(decoded.len(), 1, "{unsupported}");
        assert_eq!(decoded[0].subject().dgraph_identifier(), Some("next"));
        assert!(result.document().entities.is_empty(), "{unsupported}");
        assert!(
            has_gap(
                &result,
                0,
                unsupported.len(),
                "nquads-dgraph-native-statement-evidence-unavailable"
            ),
            "{unsupported}"
        );
    }
    for identifier in ["λ", "一", "\u{0301}"] {
        let line = format!("<{identifier}><follows><bob>.");
        let source = format!("{line}\n<next><follows><last>.\n");
        let result = output(source.as_bytes());
        assert_eq!(statements(result.document()).len(), 1);
        assert!(
            has_gap(
                &result,
                0,
                line.len(),
                "nquads-dgraph-identifier-category-evidence-unavailable"
            ),
            "{:?}",
            result.document().skipped_regions
        );
    }
    let source = "\u{feff} \t<alice><status>\"raw\0value\". \t# comment\0\r\n# comment\r\n\r\n <next><follows><last>.\r";
    let result = output(source.as_bytes());
    assert_eq!(statements(result.document()).len(), 2);
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.starts_with("nquads-dgraph-")),
        "{:?}",
        result.document().skipped_regions
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
fn actual_native_byte_payload_sizes_control_atomic_record_and_byte_admission() {
    let source = format!(
        "<alice><status>\"{}\".\n<next><follows><last>.\n",
        "\\xFF".repeat(128)
    );
    let native = Arc::new(provider());
    let frontend = analyzer(&native, DGRAPH);
    let fixture = Fixture::new(DGRAPH, source.as_bytes());
    let baseline = output(source.as_bytes());
    assert_eq!(statements(baseline.document()).len(), 2);
    let required: Vec<_> = baseline
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .collect();
    assert_eq!(required.len(), 3);
    let payloads: Vec<_> = required
        .iter()
        .filter(|extension| extension.namespace == RDF_STATEMENT_NAMESPACE)
        .map(|extension| extension.payload.len())
        .collect();
    let largest = *payloads.iter().max().unwrap();
    assert!(
        largest > source.len(),
        "canonical payload includes expanded native bytes and source evidence"
    );
    let mut floor = IrLimits::default();
    floor.max_extensions = required.len();
    floor.max_extension_payload_bytes = largest;
    let floor = with_ir_limits(floor);
    let exact = analyze(
        &frontend,
        &request(&fixture.snapshot, &fixture.source, DGRAPH, &floor),
        &ExtensionSupport::default(),
    );
    assert_eq!(statements(exact.document()).len(), 2);
    assert_eq!(exact.document().extensions.len(), required.len());
    let mut short_count = IrLimits::default();
    short_count.max_extensions = required.len() - 1;
    let mut short_payload = IrLimits::default();
    short_payload.max_extension_payload_bytes = largest - 1;
    let mut short_total = IrLimits::default();
    short_total.max_extension_payload_bytes = largest;
    short_total.max_total_extension_bytes = payloads.iter().sum::<usize>() - 1;
    for (bounds, expected) in [
        (short_count, ResourceKind::Records),
        (short_payload, ResourceKind::ExtensionBytes),
        (short_total, ResourceKind::ExtensionBytes),
    ] {
        let bounded = with_ir_limits(bounds);
        let refused = execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, DGRAPH, &bounded),
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
fn retained_statements_rebind_but_reject_changed_source_and_short_required_streams() {
    let source = b"<alice><status>\"\\xFF\".\n<alice><follows><bob>.\n<alice><follows><bob>.\n";
    let native = Arc::new(provider());
    let frontend = analyzer(&native, DGRAPH);
    let fixture = Fixture::new(DGRAPH, source);
    let wide = limits();
    let required = native
        .required_syntax_fact_count(
            &request(&fixture.snapshot, &fixture.source, DGRAPH, &wide).to_parse_request(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(required, 14);
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, DGRAPH, &exact);
    let (fresh, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(statements(fresh.document()).len(), 3);
    assert_eq!(artifact.syntax_fact_count(), required);
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    for current in [&fixture, &fixture.next_generation()] {
        let input = request(&current.snapshot, &current.source, DGRAPH, &exact);
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
        assert_eq!(decoded.len(), 3);
        assert_ne!(decoded[1].source().span(), decoded[2].source().span());
        assert!(
            decoded
                .iter()
                .all(|statement| statement.source().generation() == current.source.generation())
        );
    }
    let low = limits_with_syntax_records(required - 1);
    assert!(matches!(frontend.prepare_artifact(
        &request(&fixture.snapshot, &fixture.source, DGRAPH, &low), &deadline(),
    ), Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit }))
        if observed == required && limit == required - 1));
    let changed = fixture.rewrite(b"<alice><status>\"changed\".\n");
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, DGRAPH, &exact),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err()
    );
}

#[test]
fn projected_native_source_has_an_explicit_semantic_gap() {
    let source = b"<alice><follows><bob>.\n";
    let native = Arc::new(provider());
    let fixture = Fixture::new(DGRAPH, source);
    let budget = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(DGRAPH),
        EncodingId::utf8(),
        vec![IncludedRange::new(fixture.source.span(), language(DGRAPH))],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(DGRAPH.generated);
    let result = analyze(
        &analyzer(&native, DGRAPH),
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
                && gap.detail == "nquads-dgraph-whole-physical-utf8-source-required"
                && gap.source == fixture.source)
    );
}
