//! Astro host and native child analysis against original source coordinates.
//! Server/client ownership and visible template gaps prevent syntax-only passes
//! from being mistaken for complete framework or template semantics.

use super::*;

#[test]
fn astro_embedded_preflight_and_replay_retain_complete_declarations() {
    let source = "---\r\nfunction first(value: string) { return value; }\r\n---\r\n<style>.item { color: red; }</style><script>function last(value) { return value; }</script><Card {...props} {label} />{(() => { const local = 1; consume(local); return local; })()}";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, ASTRO);
    let fixture = Fixture::new(ASTRO, source.as_bytes());
    let budget = limits();
    let initial_request = request(&fixture.snapshot, &fixture.source, ASTRO, &budget);
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
    let next_request = request(&changed.snapshot, &changed.source, ASTRO, &bounded);
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
    let edited_source = source
        .replace("return value;", "return value + '!';")
        .replace("color: red", "color: blue")
        .replace("consume(local)", "consume( local )");
    let edited = fixture.rewrite(edited_source.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&edited.snapshot, &edited.source, ASTRO, &budget),
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

pub(super) const ASTRO: LanguageCase = LanguageCase {
    name: "astro",
    path: "src/view.astro",
    frontend: "tree-sitter-astro-next-0.1.1",
    source: "<main>text</main>",
    generated: false,
    body_before: "text",
    body_after: "changed text",
};

#[test]
fn astro_child_limits_preserve_host_and_exact_skipped_ranges() {
    let source = "---\nfunction server() {}\n---\n<script>function client() {}</script><style>.card { color: red; }</style>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, ASTRO);
    let fixture = Fixture::new(ASTRO, source.as_bytes());
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
            &request(&fixture.snapshot, &fixture.source, ASTRO, &budget),
            &ExtensionSupport::default(),
        );
        let gaps: Vec<_> = result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "astro-embedded-analysis-limit")
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
            assert_eq!(gap.reason, SkippedRegionReason::ResourceLimit);
            let span = gap.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert!(!text.contains("<script>") && !text.contains("---"));
        }
    }
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ASTRO, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, ASTRO),
        &request(&fixture.snapshot, &fixture.source, ASTRO, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn astro_server_client_and_css_keep_original_identity() {
    let source = "---\r\nfunction greet(name: string) { return name; }\r\n---\r\n<main title='é'><Card value={greet('a')} /><script>function greet(name: string) { return name + '!'; }</script><style>.card { color: red; }</style></main>";
    let result = output(source);
    let document = result.document();
    let functions: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "greet")
        .collect();
    assert_eq!(functions.len(), 2, "{:#?}", document.entities);
    assert_ne!(functions[0].id, functions[1].id);
    assert_ne!(functions[0].container, functions[1].container);
    assert!(
        functions
            .iter()
            .all(|entity| entity.language == "typescript")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.language == "css" && entity.canonical_name == ".card")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.language == "astro"
                && entity.kind == EntityKind::MarkupElement
                && entity.canonical_name == "Card")
    );
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "astro");
    for entity in functions {
        let reference = entity.evidence.source.as_ref().unwrap();
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let span = reference.span();
        assert!(
            source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
                .starts_with("function greet(")
        );
    }
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "astro-expression-analysis-unavailable")
    );
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "astro-embedded-analysis-unavailable")
    );
}

#[test]
fn astro_unprocessed_data_and_preprocessors_are_not_code_successes() {
    for (tag, language, name) in [
        (
            "<script>function typed(value: string) { return value; }</script>",
            Some("typescript"),
            "typed",
        ),
        (
            "<script is:inline>function inline() {}</script>",
            Some("javascript"),
            "inline",
        ),
        (
            "<script type='application/json'>function hidden() {}</script>",
            None,
            "hidden",
        ),
        (
            "<script src='client.js'>function hidden() {}</script>",
            None,
            "hidden",
        ),
        (
            "<script type={kind}>function hidden() {}</script>",
            None,
            "hidden",
        ),
        (
            "<script {...props}>function hidden() {}</script>",
            None,
            "hidden",
        ),
        (
            "<style {...props}>.hidden { color: red; }</style>",
            None,
            ".hidden",
        ),
        (
            "<style lang='scss'>.hidden { color: red; }</style>",
            None,
            ".hidden",
        ),
        (
            "<style is:global>.visible { color: red; }</style>",
            Some("css"),
            ".visible",
        ),
    ] {
        let result = output(tag);
        let document = result.document();
        if language.is_none() {
            assert!(
                document
                    .entities
                    .iter()
                    .all(|entity| entity.canonical_name != name),
                "{tag}: {:#?}",
                document.entities
            );
        }
        assert_eq!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name
                    && Some(entity.language.as_str()) == language),
            language.is_some(),
            "{tag}: {:#?}",
            document.entities
        );
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "astro-embedded-analysis-unavailable"),
            language.is_none(),
            "{tag}: {:#?}",
            document.skipped_regions
        );
    }
}

#[test]
fn astro_child_errors_do_not_hide_healthy_neighbors_or_template_calls() {
    let source = "---\nconst broken: = ;\n---\n<main title={format(name)}>{render(name)}<script>function healthy() {}</script></main>";
    let result = output(source);
    let document = result.document();
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "healthy" && entity.language == "typescript")
    );
    let parse_gap = document
        .skipped_regions
        .iter()
        .find(|gap| gap.detail == "astro-embedded-parse-error")
        .expect("invalid TypeScript is not a successful child parse");
    assert_eq!(parse_gap.reason, SkippedRegionReason::ParseError);
    let mut expressions: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "astro-expression-analysis-unavailable")
        .map(|gap| {
            let span = gap.source.span();
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    expressions.sort_unstable();
    assert!(expressions.is_empty());
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count(),
        2
    );
}

#[test]
fn astro_expression_calls_keep_exact_bytes_without_duplicate_nested_extraction() {
    for expression in [
        "format(label)",
        "({ label: format('é'), value: render(/}/) })",
        "items.map((item: string) => <Card title={format(item)}>{render(item)}</Card>)",
        "`value: ${format(label)}`",
    ] {
        let source = format!("<main>雪\r\n{{{expression}}}</main>");
        let result = output(&source);
        let document = result.document();
        assert!(
            !document.skipped_regions.iter().any(|gap| matches!(
                gap.detail.as_str(),
                "astro-expression-analysis-unavailable" | "astro-embedded-parse-error"
            )),
            "{expression}: {:#?}",
            document.skipped_regions
        );
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .collect();
        let expected = expression.matches("format(").count()
            + expression.matches("render(").count()
            + expression.matches(".map(").count();
        assert_eq!(calls.len(), expected, "{expression}: {calls:#?}");
        let mut ranges = BTreeSet::new();
        for call in calls {
            let span = call.source.span();
            assert!(ranges.insert((span.start_byte(), span.end_byte())));
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(call.syntactic_text_hash, content_hash(text.as_bytes()));
            assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
            assert!(
                text.contains("format") || text.contains("render") || text.contains("map"),
                "{text}"
            );
        }
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "astro-template-semantics-unavailable")
        );
    }
}

#[test]
fn astro_expression_bodies_retain_written_definitions_and_reject_invalid_syntax() {
    let source = "<main>{(() => { const local = 1; consume(local); return local; })()}</main>";
    let result = output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "local" && entity.language == "typescript")
    );
    for source in [
        "<main title={const value = 1}></main>",
        "<main>{function broken(}</main>",
        "<main>{/* comment */ const value = 1}</main>",
        "<main title={/* no attribute value */}></main>",
        "<main>{/* comment */ , ,}</main>",
        "<Card {...props,} />",
        "<Card {...props, ...other} />",
        "<Card {...} />",
        "<Card value={...props} />",
        "<Card {/* lead */ ...props} />",
    ] {
        let result = output(source);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.reason == SkippedRegionReason::ParseError),
            "{source}: {:#?}",
            result.document().skipped_regions
        );
    }
}

#[test]
fn astro_comment_interpolations_do_not_invent_code_or_parse_errors() {
    for comment in [
        "/* visible source, no runtime value */",
        " /* é雪 { fakeCall() } */\r\n /* second */ ",
        "// ignoredCall()\r\n",
        "/* first */ // second\n /* third */",
    ] {
        let source = format!("<main>é\r\n{{{comment}}}{{realCall()}}</main>");
        let result = output(&source);
        let document = result.document();
        assert!(
            !document.skipped_regions.iter().any(|gap| matches!(
                gap.detail.as_str(),
                "astro-expression-analysis-unavailable" | "astro-embedded-parse-error"
            )),
            "{source}: {:#?}",
            document.skipped_regions
        );
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .collect();
        assert_eq!(calls.len(), 1, "{source}: {calls:#?}");
        let span = calls[0].source.span();
        let text = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert!(text.contains("realCall"), "{text}");
        assert_eq!(calls[0].syntactic_text_hash, content_hash(text.as_bytes()));
        assert_eq!(
            calls[0].source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
}

#[test]
fn astro_spread_and_shorthand_attributes_keep_native_source_references() {
    for attribute in [
        "{...props}",
        "{ ...props}",
        "{\u{a0}...props}",
        "{... /* lead */ compose(props) /* tail */ }",
        "{...{title: format(label), value: /}/}}",
        "{...items.map(item => <Card {...item} />)}",
        "{label}",
        "{props.value}",
        "{...props, other}",
    ] {
        let source = format!("<main>é\r\n<Card {attribute} /></main>");
        let result = output(&source);
        let document = result.document();
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.reason == SkippedRegionReason::ParseError
                    || gap.detail == "astro-expression-analysis-unavailable"),
            "{source}: {:#?}",
            document.skipped_regions
        );
        let expected = attribute.matches("compose(").count()
            + attribute.matches("format(").count()
            + attribute.matches(".map(").count();
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
                .count(),
            expected,
            "{source}"
        );
        let needle = if attribute.contains("props") {
            "props"
        } else if attribute.contains("items") {
            "items"
        } else {
            "label"
        };
        assert!(
            document.occurrences.iter().any(|occurrence| {
                let span = occurrence.source.span();
                let text = &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()];
                text == needle
                    && occurrence.role != OccurrenceRole::Definition
                    && occurrence.syntactic_text_hash == content_hash(text.as_bytes())
                    && occurrence.source.content_hash() == content_hash(source.as_bytes())
            }),
            "{source}: {:#?}",
            document.occurrences
        );
    }
}

#[test]
fn astro_template_parameters_keep_distinct_written_owners() {
    let source = "---\r\nconst label = 'server';\r\n---\r\n{items.map((label: string) => <span>{label}</span>)}<script>function client(label: string) { return label; }</script>";
    let result = output(source);
    let document = result.document();
    let parameters: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "label")
        .collect();
    assert_eq!(parameters.len(), 2, "{:#?}", document.entities);
    assert_ne!(parameters[0].id, parameters[1].id);
    for parameter in parameters {
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "label"
        );
    }
}

#[test]
fn astro_destructured_variables_keep_server_template_and_client_owners() {
    let source = "---\r\nconst {label} = props;\r\n---\r\n{items.map(item => { const {label} = item; return <span>{label}</span>; })}<script>const {label} = clientProps;</script>";
    let result = output(source);
    let document = result.document();
    let variables: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "label")
        .collect();
    assert_eq!(variables.len(), 3, "{:#?}", document.skipped_regions);
    assert_eq!(
        variables
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    for variable in variables {
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "label"
        );
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
}

#[test]
fn astro_expressions_share_the_existing_host_range_budget() {
    let source = "<main>{first()}<span {...second()}></span>{third()}</main>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, ASTRO);
    let fixture = Fixture::new(ASTRO, source.as_bytes());
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
            &request(&fixture.snapshot, &fixture.source, ASTRO, &budget),
            &ExtensionSupport::default(),
        );
        let document = result.document();
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
                .count(),
            ranges
        );
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "astro-embedded-analysis-limit")
            .collect();
        assert_eq!(gaps.len(), 3 - ranges);
        for gap in gaps {
            let span = gap.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert!(matches!(text, "{first()}" | "{...second()}" | "{third()}"));
            assert_eq!(gap.reason, SkippedRegionReason::ResourceLimit);
        }
    }
}
