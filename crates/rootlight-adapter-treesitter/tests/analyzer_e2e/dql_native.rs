//! Original native queries reach source-owned typed IR without software entities.

use super::*;
use rootlight_adapter_sdk::{BoundedIrSink, IrBatchSink, SyntaxFact, testkit::MockParseProvider};
use rootlight_ir::{CanonicalNormalizedFileChunk, DqlQuery};
use rootlight_ir::{DQL_QUERY_NAMESPACE, DqlCallKind, DqlNodeKind, decode_dql_query_envelope};

const DQL: LanguageCase = LanguageCase {
    name: "dql",
    path: "queries/query.json",
    frontend: "rootlight-tree-sitter-dql-0.1.0",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

#[test]
fn original_three_queries_preserve_typed_roles_and_physical_sources() {
    // Apache-2.0 originals and their complete notice are retained with the grammar.
    for (source, roots, filters, counts) in [
        (
            include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite01.dql").as_slice(),
            2,
            0,
            0,
        ),
        (
            include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite02.dql").as_slice(),
            18,
            12,
            6,
        ),
        (
            include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite03.dql").as_slice(),
            4,
            0,
            1,
        ),
    ] {
        let native = Arc::new(provider());
        let fixture = Fixture::new(DQL, source);
        let budget = limits();
        let result = analyze(
            &analyzer(&native, DQL),
            &request(&fixture.snapshot, &fixture.source, DQL, &budget),
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        let envelopes: Vec<_> = document
            .extensions
            .iter()
            .filter(|extension| extension.namespace == DQL_QUERY_NAMESPACE)
            .collect();
        assert_eq!(envelopes.len(), 1, "{:?}", document.skipped_regions);
        let query = decode_dql_query_envelope(envelopes[0]).unwrap();
        assert_eq!(query.source(), &fixture.source);
        assert_eq!(query.text().as_bytes(), source);
        query
            .verify_source_text(std::str::from_utf8(source).unwrap())
            .unwrap();
        assert_eq!(query.source().content_hash(), content_hash(source));
        assert_eq!(query.source().span().start_byte(), 0);
        assert_eq!(
            query.source().span().end_byte(),
            u64::try_from(source.len()).unwrap()
        );
        let count = |role| {
            query
                .nodes()
                .iter()
                .filter(|node| node.role == role)
                .count()
        };
        assert_eq!(count(DqlNodeKind::Operation), 1);
        assert_eq!(count(DqlNodeKind::Root { aggregation: false }), roots);
        for (class, expected) in [
            (DqlCallKind::Generator, roots),
            (DqlCallKind::Filter, filters),
            (DqlCallKind::Nested, filters),
            (DqlCallKind::Count, counts),
        ] {
            assert_eq!(count(DqlNodeKind::Call { class }), expected, "{class:?}");
        }
        assert_eq!(count(DqlNodeKind::Filter), filters);
        assert_eq!(count(DqlNodeKind::Predicate { reverse: true }), filters / 2);
        let mut languages = Vec::new();
        for (index, node) in query.nodes().iter().enumerate() {
            let source_ref = query.node_source(index).unwrap();
            assert_eq!(source_ref.span(), node.span);
            assert_eq!(source_ref.repository(), fixture.source.repository());
            assert_eq!(source_ref.generation(), fixture.source.generation());
            assert_eq!(source_ref.content_hash(), fixture.source.content_hash());
            let text = &query.text()[usize::try_from(node.span.start_byte()).unwrap()
                ..usize::try_from(node.span.end_byte()).unwrap()];
            assert!(!text.is_empty());
            if let Some(parent) = node.parent {
                let parent = &query.nodes()[usize::try_from(parent).unwrap()];
                assert!(parent.span.start_byte() <= node.span.start_byte());
                assert!(parent.span.end_byte() >= node.span.end_byte());
            }
            if matches!(node.role, DqlNodeKind::Predicate { reverse: true }) {
                assert_eq!(text, "~friend");
            }
            if node.role == DqlNodeKind::Language {
                languages.push(text);
                assert!(matches!(
                    query.nodes()[usize::try_from(node.parent.unwrap()).unwrap()].role,
                    DqlNodeKind::Predicate { .. }
                ));
            }
        }
        assert_eq!(languages, if roots == 4 { vec!["@."] } else { Vec::new() });
        assert!(query.references().is_empty());
        assert!(document.entities.is_empty());
        assert!(
            document
                .occurrences
                .iter()
                .all(|occurrence| occurrence.role != OccurrenceRole::Definition)
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail.starts_with("dql-")
                    || gap.detail == "declaration-name-unavailable"),
            "{:?}",
            document.skipped_regions
        );
    }
}

fn output(source: &[u8], budget: &AnalysisLimits) -> AnalysisOutput {
    let native = Arc::new(provider());
    let fixture = Fixture::new(DQL, source);
    analyze(
        &analyzer(&native, DQL),
        &request(&fixture.snapshot, &fixture.source, DQL, budget),
        &ExtensionSupport::default(),
    )
}

fn query(result: &AnalysisOutput) -> DqlQuery {
    let envelopes: Vec<_> = result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == DQL_QUERY_NAMESPACE)
        .collect();
    assert_eq!(
        envelopes.len(),
        1,
        "{:?}",
        result.document().skipped_regions
    );
    decode_dql_query_envelope(envelopes[0]).unwrap()
}

fn with_ir(ir: IrLimits) -> AnalysisLimits {
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

fn mocked_output(source: &[u8], mutate: impl FnOnce(&mut Vec<SyntaxFact>)) -> AnalysisOutput {
    let native = provider();
    let fixture = Fixture::new(DQL, source);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, DQL, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let mut facts = parsed.facts().to_vec();
    mutate(&mut facts);
    let mocked = MockParseProvider::new(
        native.capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(mocked),
        producer_identity(),
        language(DQL),
        DQL.frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    analyze(&frontend, &input, &ExtensionSupport::default())
}

#[test]
fn native_refusals_and_incomplete_role_ownership_never_publish_a_query() {
    for source in [
        b"{ q(func: uid(1)) @cascade { name } }".as_slice(),
        b"{ q(func: uid(1), first: 2) { name } }",
        b"{ q(func: eq(<urn:name>, \"x\")) { name } }",
    ] {
        let result = output(source, &limits());
        assert!(
            !result
                .document()
                .extensions
                .iter()
                .any(|extension| extension.namespace == DQL_QUERY_NAMESPACE)
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Extensions
                    && gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && gap.detail.starts_with("dql-"))
        );
        assert!(result.document().entities.is_empty());
    }
    let source = b"{ q(func: eq(name, \"opaque\")) { name } }";
    for mutation in 0..3 {
        let result = mocked_output(source, |facts| {
            if mutation == 2 {
                facts.retain(|fact| {
                    !fact.syntax_kind().as_str().starts_with("dql.")
                        || fact.kind() != rootlight_adapter_sdk::SyntaxFactKind::Signature
                        || fact.syntax_kind().as_str() == "dql.operation.expression"
                });
                return;
            }
            let witness = facts
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == "dql.root_name.expression")
                .unwrap()
                .clone();
            if mutation == 0 {
                facts.retain(|fact| fact.local_id() != witness.local_id());
            } else {
                facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            }
        });
        assert!(
            !result
                .document()
                .extensions
                .iter()
                .any(|extension| extension.namespace == DQL_QUERY_NAMESPACE),
            "mutation {mutation}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Extensions && gap.detail.starts_with("dql-"))
        );
    }
}

#[test]
fn canonical_payload_and_required_record_limits_are_atomic() {
    let source = include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite02.dql");
    let baseline = output(source, &limits());
    let native_payload = baseline
        .document()
        .extensions
        .iter()
        .find(|extension| extension.namespace == DQL_QUERY_NAMESPACE)
        .unwrap()
        .payload
        .len();
    assert!(native_payload > IrLimits::default().max_string_bytes);
    let total = baseline
        .document()
        .extensions
        .iter()
        .map(|extension| extension.payload.len())
        .sum::<usize>();
    let required = baseline.document().extensions.len();
    assert_eq!(required, 2);
    let mut floor = IrLimits::default();
    floor.max_extensions = required;
    floor.max_extension_payload_bytes = native_payload;
    floor.max_total_extension_bytes = total;
    assert_eq!(
        query(&output(source, &with_ir(floor.clone())))
            .nodes()
            .len(),
        query(&baseline).nodes().len()
    );
    let native = Arc::new(provider());
    let fixture = Fixture::new(DQL, source);
    let frontend = analyzer(&native, DQL);
    for (dimension, resource) in [
        (0, ResourceKind::Records),
        (1, ResourceKind::ExtensionBytes),
        (2, ResourceKind::ExtensionBytes),
    ] {
        let mut short = floor.clone();
        match dimension {
            0 => short.max_extensions -= 1,
            1 => short.max_extension_payload_bytes -= 1,
            _ => short.max_total_extension_bytes -= 1,
        }
        let bounds = with_ir(short);
        let input = request(&fixture.snapshot, &fixture.source, DQL, &bounds);
        let mut sink = BoundedIrSink::new(
            fixture.source.clone(),
            bounds.ir_stream().clone(),
            bounds.ir().clone(),
            ExtensionSupport::default(),
        );
        let result = frontend.analyze(&input, &mut sink, &deadline());
        assert!(
            matches!(result, Err(AdapterError::Sink(SinkError::StreamLimit { resource: actual, .. })) if actual == resource),
            "{result:?}"
        );
        assert_eq!(
            sink.staged_usage().records(),
            0,
            "partial output for dimension {dimension}"
        );
    }
}

#[test]
fn retained_queries_rebind_generations_and_preserve_local_bindings() {
    let source = include_bytes!("../../../../native-grammars/dql/tests/fixtures/suite03.dql");
    let native = Arc::new(provider());
    let fixture = Fixture::new(DQL, source);
    let frontend = analyzer(&native, DQL);
    let wide = limits();
    let input = request(&fixture.snapshot, &fixture.source, DQL, &wide);
    let required = native
        .required_syntax_fact_count(&input.to_parse_request(), &deadline())
        .unwrap();
    let exact = limits_with_syntax_records(required);
    let input = request(&fixture.snapshot, &fixture.source, DQL, &exact);
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
    for current in [&fixture, &fixture.next_generation()] {
        let input = request(&current.snapshot, &current.source, DQL, &exact);
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
        assert_eq!(query(&replay).source(), &current.source);
    }
    let chunk = CanonicalNormalizedFileChunk::new(
        fresh.document(),
        exact.ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    let generation = GenerationId::from_bytes([91; 20]);
    let rebound = chunk
        .rebind(generation, exact.ir(), &ExtensionSupport::default())
        .unwrap();
    let rebound = decode_dql_query_envelope(
        rebound
            .extensions
            .iter()
            .find(|extension| extension.namespace == DQL_QUERY_NAMESPACE)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(rebound.nodes(), query(&fresh).nodes());
    assert!(
        rebound.nodes().iter().enumerate().all(|(index, _)| rebound
            .node_source(index)
            .unwrap()
            .generation()
            == generation)
    );
    let short = limits_with_syntax_records(required - 1);
    assert!(matches!(
        frontend.prepare_artifact(
            &request(&fixture.snapshot, &fixture.source, DQL, &short),
            &deadline()
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            ..
        }))
    ));
    let local = query(&output(br#"query find($term: string) { q(func: eq(name, $term)) { f as friends { c as age } } r(func: uid(f)) { val(c) } }"#, &wide));
    assert_eq!(local.references().len(), 3);
    assert!(
        local
            .references()
            .iter()
            .all(|reference| reference.declaration.is_some())
    );
    assert_eq!(
        local
            .nodes()
            .iter()
            .filter(|node| node.role == DqlNodeKind::Binding)
            .count(),
        2
    );
    let declarations: BTreeSet<_> = local
        .references()
        .iter()
        .map(|reference| {
            let span = local.nodes()[usize::try_from(reference.declaration.unwrap()).unwrap()]
                .name
                .unwrap();
            &local.text()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(declarations, BTreeSet::from(["$term", "f", "c"]));
}
