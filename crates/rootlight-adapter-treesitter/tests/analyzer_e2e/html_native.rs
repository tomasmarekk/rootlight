//! Source markup contracts through the real parser, lowering and validated IR.
//! Repeated occurrences must retain ownership without claiming browser DOM or
//! browser semantics from successful host or embedded syntax extraction.

use super::*;

pub(super) const HTML: LanguageCase = LanguageCase {
    name: "html",
    path: "src/view.html",
    frontend: "tree-sitter-html-0.23.2",
    source: "<main><item key='one' key='two'><item disabled /></item><item key='three'>text</item></main>",
    generated: false,
    body_before: "text",
    body_after: "changed text",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(HTML, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, HTML),
        &request(&fixture.snapshot, &fixture.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn html_embedded_languages_keep_host_source_and_distinct_owners() {
    let source = "<main>é\r\n<script>function greet(name) { return name; } greet('a');</script><style>.card { color: red }</style><script>function greet(name) { return name + '!'; }</script></main>";
    let result = output(source);
    let document = result.document();
    let functions: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "greet")
        .collect();
    assert_eq!(functions.len(), 2, "{:#?}", document.entities);
    assert_ne!(functions[0].id, functions[1].id);
    assert!(
        functions
            .iter()
            .all(|entity| entity.language == "javascript")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.language == "css" && entity.canonical_name == ".card")
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "html-embedded-analysis-unavailable")
    );
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "html");
    for entity in functions {
        let span = entity.evidence.source.as_ref().unwrap().span();
        let text = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert!(text.starts_with("function greet("), "{text}");
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().content_hash(),
            content_hash(source.as_bytes())
        );
    }
}

#[test]
fn html_embedded_routing_uses_host_attributes_not_body_heuristics() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let budget = limits();
    for (attributes, supported) in [
        ("", true),
        (" type", true),
        (" TYPE='MoDuLe'", true),
        (" type=text/javascript", true),
        (" type='APPLICATION/ECMASCRIPT'", true),
        (" language=JavaScript", true),
        (" type='' language=vbscript", true),
        (" language=vbscript", false),
        (" src=external.js", false),
        (" type=application/json", false),
        (" type=importmap", false),
        (" type=text/typescript", false),
        (" type='text/javascript; charset=utf-8'", false),
        (" type='module ' ", true),
        (" type='\ttext/javascript\r\n'", true),
        (" type=' ' ", false),
        (" type='\u{000b}module'", false),
        (" language=javascript1.3", true),
        (" type='text/java&#115;cript'", false),
        (" type='text/javascript' type='application/json'", false),
    ] {
        let source = format!(
            "<!-- <script>function hidden() {{}}</script> --><script{attributes}>function visible() {{}}</script>"
        );
        let fixture = Fixture::new(HTML, source.as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, HTML, &budget),
            &ExtensionSupport::default(),
        );
        assert_eq!(result.document().entities.iter().any(|entity| entity.language == "javascript" && entity.canonical_name == "visible"), supported, "{attributes}");
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "hidden")
        );
        assert_eq!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "html-embedded-analysis-unavailable"),
            !supported,
            "{attributes}"
        );
    }
    for source in [
        "<svg><script>function hidden() {}</script></svg>",
        "<noscript><script>function hidden() {}</script></noscript>",
        "<style type='text/scss'>.hidden { color: red; }</style>",
    ] {
        let fixture = Fixture::new(HTML, source.as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, HTML, &budget),
            &ExtensionSupport::default(),
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.language == "html")
        );
    }
}

#[test]
fn html_embedded_preflight_and_replay_retain_complete_declarations() {
    let source = "<script>function first(value) { return value; }</script><style>.item { color: red; }</style><script>function last(value) { return value; }</script>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let fixture = Fixture::new(HTML, source.as_bytes());
    let budget = limits();
    let initial_request = request(&fixture.snapshot, &fixture.source, HTML, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial_request.to_parse_request(), &deadline())
        .unwrap();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        demand,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    let bounded = limits_with_syntax_records(demand);
    let changed = fixture.next_generation();
    let next_request = request(&changed.snapshot, &changed.source, HTML, &bounded);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &next_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &next_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let identities = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(first.document()), identities(replay.document()));
    assert!(
        replay
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "last")
    );
    for entity in &replay.document().entities {
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().generation(),
            changed.source.generation()
        );
    }
    let edited_source = format!(
        "é\r\n{}",
        source
            .replace("return value;", "return value + '!';")
            .replace("color: red", "color: blue")
    );
    let edited = fixture.rewrite(edited_source.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&edited.snapshot, &edited.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(identities(first.document()), identities(next.document()));
    for entity in &next.document().entities {
        let reference = entity.evidence.source.as_ref().unwrap();
        assert_eq!(reference.generation(), edited.source.generation());
        assert_eq!(
            reference.content_hash(),
            content_hash(edited_source.as_bytes())
        );
    }
}

#[test]
fn html_embedded_limits_are_shared_and_leave_source_scoped_gaps() {
    let source = "<script>function first() {}</script><script>function last() {}</script><style>.item { color:red; }</style>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let fixture = Fixture::new(HTML, source.as_bytes());
    let base = limits();
    for ranges in [0, 1, 2] {
        let budget = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            ranges,
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            base.ir().clone(),
        )
        .unwrap();
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, HTML, &budget),
            &ExtensionSupport::default(),
        );
        let gaps: Vec<_> = result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "html-embedded-resource-limit")
            .collect();
        assert_eq!(gaps.len(), 3 - ranges);
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Function)
                .count(),
            ranges
        );
        assert!(result.report().resources().syntax_nodes() <= budget.max_syntax_nodes());
        for gap in gaps {
            let span = gap.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert!(!text.contains("<script>"));
        }
    }
    let host_only = AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        0,
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap();
    let host = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, HTML, &host_only),
        &ExtensionSupport::default(),
    );
    let budget = AnalysisLimits::new(
        base.max_source_bytes(),
        host.report().resources().syntax_nodes(),
        base.max_syntax_depth(),
        32,
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap();
    let exhausted = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(
        exhausted.report().resources().syntax_nodes(),
        budget.max_syntax_nodes()
    );
    assert_eq!(
        exhausted
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "html-embedded-resource-limit")
            .count(),
        3
    );
}

#[test]
fn html_embedded_parse_errors_do_not_join_bodies_or_hide_later_valid_code() {
    let source = "<script>function split(</script><script>) {}</script><script>function complete() {}</script>";
    let result = output(source);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "split")
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "complete")
    );
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "html-embedded-parse-error")
            .count(),
        2
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn html_native_preserves_every_source_element_attribute_and_owner() {
    let result = output(HTML.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_3);
    assert_eq!(document.entities.len(), 9, "{:?}", document.entities);
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        9
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::MarkupElement)
            .count(),
        4
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::MarkupAttribute)
            .count(),
        4
    );
    for entity in document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let name = &HTML.source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(name, entity.canonical_name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(name.as_bytes())
        );
        let owner = document
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id)
            })
            .unwrap();
        let RelationEndpoint::Entity(symbol) = owner.subject else {
            panic!("markup has an entity owner")
        };
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let parent_span = parent.evidence.source.as_ref().unwrap().span();
        let child_span = entity.evidence.source.as_ref().unwrap().span();
        assert!(parent_span.start_byte() <= child_span.start_byte());
        assert!(parent_span.end_byte() >= child_span.end_byte());
        if entity.kind == EntityKind::MarkupAttribute {
            assert_eq!(parent.kind, EntityKind::MarkupElement);
        }
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_eq!(document.skipped_regions.len(), 1);
    assert_eq!(
        document.skipped_regions[0].detail,
        "html-dom-semantics-unavailable"
    );
    assert_eq!(document.skipped_regions[0].domain, FactDomain::Relations);
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn html_native_body_value_and_trivia_edits_keep_occurrence_identities() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let fixture = Fixture::new(HTML, HTML.source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    let edited = HTML
        .source
        .replace("text", "changed text")
        .replace("'one'", "\"updated\"")
        .replace("<item ", "<item  ");
    let changed = fixture.rewrite(edited.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(markup_ids(first.document()), markup_ids(next.document()));
    for entity in &next.document().entities {
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().generation(),
            changed.source.generation()
        );
    }
}

#[test]
fn html_native_source_spelling_void_and_implicit_elements_are_not_dom_bindings() {
    let source = "<MAIN><p title='a'>one<p title='b'>two<img src=x /><x-é data-[x]='y' @click='go'/><svg viewBox='0 0 1 1'><linearGradient /></svg></MAIN>";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, count) in [
        ("MAIN", 1),
        ("p", 2),
        ("title", 2),
        ("img", 1),
        ("x-é", 1),
        ("data-[x]", 1),
        ("@click", 1),
        ("viewBox", 1),
        ("linearGradient", 1),
    ] {
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name)
                .count(),
            count,
            "{name}: {:?}",
            document.entities
        );
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "html-dom-semantics-unavailable")
    );
}

#[test]
fn html_native_unsupported_embedded_gaps_survive_required_capture_and_replay() {
    let source = "<main><script type='application/x-template'>const value = '<item>'; </script><style type='text/scss'>.item { color: red }</style><item /></main>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let fixture = Fixture::new(HTML, source.as_bytes());
    let budget = limits();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, HTML, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let gaps: Vec<_> = first
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "html-embedded-analysis-unavailable")
        .collect();
    assert_eq!(gaps.len(), 2);
    let bodies: BTreeSet<_> = gaps
        .iter()
        .map(|gap| {
            let span = gap.source.span();
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(
        bodies,
        BTreeSet::from(["const value = '<item>'; ", ".item { color: red }"])
    );
    assert!(
        !first
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "value")
    );
    let changed = fixture.next_generation();
    let bounded =
        limits_with_syntax_records(artifact.required_syntax_fact_count(&deadline()).unwrap());
    let (fresh, retained) = analyzer
        .analyze_and_capture(
            &request(&changed.snapshot, &changed.source, HTML, &bounded),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &request(&changed.snapshot, &changed.source, HTML, &bounded),
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    assert_eq!(markup_ids(first.document()), markup_ids(replay.document()));
    for gap in gaps {
        assert!(
            replay
                .document()
                .skipped_regions
                .iter()
                .any(|next| next.detail == gap.detail
                    && next.source.span() == gap.source.span()
                    && next.source.generation() == changed.source.generation())
        );
    }
}

fn markup_ids(document: &rootlight_ir::NormalizedIrDocument) -> BTreeSet<SymbolId> {
    document
        .entities
        .iter()
        .filter(|entity| entity.language == "html")
        .map(|entity| entity.id)
        .collect()
}

#[test]
fn html_native_script_escapes_keep_exact_embedded_coverage() {
    let body = "<!--<ScRiPt>one\0</script><fake key='literal'>two</fake>-->";
    let source = format!("<main><script>{body}</script><p>safe</p></main>");
    let result = output(&source);
    let document = result.document();
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    assert_eq!(markup_ids(document).len(), 4);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::MarkupElement)
            .map(|entity| entity.canonical_name.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["main", "script", "p"])
    );
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "html-embedded-parse-error")
        .collect();
    assert_eq!(gaps.len(), 1);
    let span = gaps[0].source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        body
    );
    assert_eq!(gaps[0].domain, FactDomain::Entities);
    let altered = source.replace("one", "changed source body");
    assert_eq!(
        markup_ids(document),
        markup_ids(output(&altered).document())
    );
}

#[test]
fn html_native_text_modes_do_not_define_literal_markup() {
    for tag in ["title", "textarea", "xmp", "iframe", "noembed", "noframes"] {
        let body =
            format!("\n<fake id='not-an-attribute'>literal</fake><!-- text -->&amp;</{tag}X>");
        let source = format!("<{tag} id='owner'>{body}</{tag}><p>outside</p>");
        let result = output(&source);
        let document = result.document();
        assert!(
            document.diagnostics.is_empty(),
            "{tag}: {:?}",
            document.diagnostics
        );
        assert_eq!(document.entities.len(), 4, "{tag}: {:?}", document.entities);
        for name in [tag, "id", "p"] {
            assert_eq!(
                document
                    .entities
                    .iter()
                    .filter(|entity| entity.canonical_name == name)
                    .count(),
                1,
                "{tag}: {name}"
            );
        }
        assert!(
            document.occurrences.iter().any(|occurrence| {
                let span = occurrence.source.span();
                occurrence.role == OccurrenceRole::StringEvidence
                    && occurrence.syntactic_text_hash == content_hash(body.as_bytes())
                    && source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                        == body
            }),
            "{tag}: literal source must remain retrievable"
        );
        assert_eq!(
            document.skipped_regions.len(),
            1,
            "{tag}: {:?}",
            document.skipped_regions
        );
        assert_eq!(
            document.skipped_regions[0].detail,
            "html-dom-semantics-unavailable"
        );
    }
}

#[test]
fn html_native_plaintext_keeps_the_complete_remainder_as_source_text() {
    let body = "one\0<fake key='text'>two</fake></plaintext><p>still text</p>";
    let source = format!("<plaintext>{body}");
    let result = output(&source);
    let document = result.document();
    assert!(document.diagnostics.is_empty());
    assert_eq!(document.entities.len(), 2);
    let element = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::MarkupElement)
        .unwrap();
    assert_eq!(element.canonical_name, "plaintext");
    assert_eq!(
        element.evidence.source.as_ref().unwrap().span().end_byte(),
        u64::try_from(source.len()).unwrap()
    );
    assert!(document.occurrences.iter().any(|occurrence| occurrence.role
        == OccurrenceRole::StringEvidence
        && occurrence.syntactic_text_hash == content_hash(body.as_bytes())));
}

#[test]
fn html_native_context_uncertainty_survives_required_capture_replay() {
    let source = "<main><svg><title><b>markup</b></title></svg><math><mi>x</mi></math><noscript><b>conditional</b></noscript><p>safe</p></main>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let fixture = Fixture::new(HTML, source.as_bytes());
    let budget = limits();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, HTML, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let expected = [
        (
            "html-foreign-context-unavailable",
            "<svg><title><b>markup</b></title></svg>",
        ),
        (
            "html-foreign-context-unavailable",
            "<math><mi>x</mi></math>",
        ),
        (
            "html-scripting-mode-unavailable",
            "<noscript><b>conditional</b></noscript>",
        ),
    ];
    for (detail, text) in expected {
        assert!(
            first.document().skipped_regions.iter().any(|gap| {
                let span = gap.source.span();
                gap.domain == FactDomain::Entities
                    && gap.detail == detail
                    && &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                        == text
            }),
            "{detail}: {:?}",
            first.document().skipped_regions
        );
    }
    let changed = fixture.next_generation();
    let bounded =
        limits_with_syntax_records(artifact.required_syntax_fact_count(&deadline()).unwrap());
    let (fresh, retained) = analyzer
        .analyze_and_capture(
            &request(&changed.snapshot, &changed.source, HTML, &bounded),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &request(&changed.snapshot, &changed.source, HTML, &bounded),
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(markup_ids(first.document()), markup_ids(replay.document()));
    for gap in &first.document().skipped_regions {
        assert!(
            replay
                .document()
                .skipped_regions
                .iter()
                .any(|next| next.detail == gap.detail
                    && next.source.span() == gap.source.span()
                    && next.source.generation() == changed.source.generation())
        );
    }
}

#[test]
fn html_native_unrelated_siblings_do_not_renumber_same_name_occurrences() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, HTML);
    let fixture = Fixture::new(HTML, HTML.source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    let edited = HTML
        .source
        .replace("<main>", "<main><aside />")
        .replace("key='three'", "title='other' key='three'");
    let changed = fixture.rewrite(edited.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, HTML, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(
        next.document().entities.len(),
        first.document().entities.len() + 2
    );
    assert!(markup_ids(first.document()).is_subset(&markup_ids(next.document())));
}

#[test]
fn html_native_unmatched_end_tags_remain_explicit_source_gaps() {
    let source = "<main>text</missing><item /></main>";
    let result = output(source);
    let gap = result
        .document()
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "html-unmatched-end-tag")
        .unwrap();
    let span = gap.source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "</missing>"
    );
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::MarkupElement)
            .count(),
        2
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "missing")
    );
}

#[test]
fn html_native_oversized_native_names_do_not_claim_complete_structure() {
    let name = format!("x-{}", "a".repeat(1100));
    let source = format!("<{name}>text</{name}>");
    let result = output(&source);
    assert!(!result.document().diagnostics.is_empty());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == name)
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}
