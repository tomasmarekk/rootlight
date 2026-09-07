//! Source markup contracts through the real parser, lowering and validated IR.
//! Repeated occurrences must retain ownership without claiming browser DOM or
//! embedded-language semantics from successful HTML syntax extraction.

use super::*;

const HTML: LanguageCase = LanguageCase {
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
fn html_native_embedded_gaps_survive_required_only_capture_and_generation_replay() {
    let source = "<main><script>const value = '<item>'; </script><style>.item { color: red }</style><item /></main>";
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
    document.entities.iter().map(|entity| entity.id).collect()
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
