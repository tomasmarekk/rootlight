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

#[test]
fn astro_server_and_client_imports_keep_distinct_written_definitions() {
    let source = "---\r\nimport {Widget as Component} from './server';\r\n---\r\n<Component /><script>import {Widget as Component} from './client';</script>";
    let result = output(source);
    let document = result.document();
    let imports: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Import && entity.canonical_name == "Component")
        .collect();
    assert_eq!(imports.len(), 2, "{:#?}", document.skipped_regions);
    assert_ne!(imports[0].id, imports[1].id);
    for import in imports {
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: import.id }
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "Component"
        );
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Imports)
    );
}

#[test]
fn original_astro_template_read_targets_its_frontmatter_variable() {
    let raw = include_bytes!("../fixtures/astro_source_fields/Grid.astro");
    assert_eq!(raw.len(), 319);
    assert_eq!(&raw[127..132], b"posts");
    assert_eq!(&raw[161..166], b"posts");
    assert_eq!(&raw[267..272], b"posts");
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ASTRO, raw);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, ASTRO),
        &request(&fixture.snapshot, &fixture.source, ASTRO, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    let definitions: Vec<_> = [
        (116, 121, "Props", EntityKind::Interface, "typescript"),
        (127, 132, "posts", EntityKind::Field, "typescript"),
        (161, 166, "posts", EntityKind::Variable, "typescript"),
        (288, 292, "Item", EntityKind::MarkupElement, "astro"),
    ]
    .into_iter()
    .map(|(start, end, name, kind, language)| {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect();
        assert_eq!(sites.len(), 1, "exact written definition {name}");
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("written definition must resolve: {name}")
        };
        let entity = document
            .entities
            .iter()
            .find(|item| item.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.language, language);
        assert_eq!(entity.canonical_name, name);
        assert_eq!(sites[0].source.repository(), fixture.repository);
        assert_eq!(sites[0].source.generation(), fixture.source.generation());
        assert_eq!(sites[0].source.content_hash(), content_hash(raw));
        symbol
    })
    .collect();
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(definitions[0])
                    && edge.object == RelationEndpoint::Entity(definitions[1])
            })
            .count(),
        1
    );
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == 267
                && site.source.span().end_byte() == 272
        })
        .collect();
    assert_eq!(reads.len(), 1, "exact original template read");
    let read = reads[0];
    assert_eq!(read.source.repository(), fixture.repository);
    assert_eq!(read.source.generation(), fixture.source.generation());
    assert_eq!(read.source.content_hash(), content_hash(raw));
    assert_eq!(read.syntactic_text_hash, content_hash(b"posts"));
    assert_eq!(
        read.target,
        OccurrenceTarget::Resolved {
            symbol: definitions[2]
        }
    );
    assert_ne!(definitions[1], definitions[2]);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
                    && edge.object == RelationEndpoint::Entity(definitions[2])
                    && edge.evidence.source.as_ref() == Some(&read.source)
            })
            .count(),
        1
    );
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 267
            && site.source.span().end_byte() == 272
    }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "astro-template-semantics-unavailable" })
    );
}

#[test]
fn astro_template_certificates_keep_frontmatter_fields_and_client_variables_separate() {
    let source = "---\ninterface Props { posts: string[] }\nconst {posts} = Astro.props;\n---\n{posts.map(post => <b>{post}</b>)}<script>const posts = []; console.log(posts);</script>";
    let document = output(source);
    let document = document.document();
    let declared = source.find("{posts}").unwrap() + 1;
    let read = source.find("{posts.map").unwrap() + 1;
    let variable = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == u64::try_from(declared).unwrap()
        })
        .unwrap();
    let OccurrenceTarget::Resolved { symbol } = variable.target else {
        panic!("exact frontmatter variable definition");
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Variable);
    assert_eq!(entity.language, "typescript");
    let references: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(read).unwrap()
                && site.source.span().end_byte() == u64::try_from(read + 5).unwrap()
        })
        .collect();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(
        references[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    let client_read = source.rfind("posts").unwrap();
    assert!(
        !document
            .occurrences
            .iter()
            .any(
                |site| site.source.span().start_byte() == u64::try_from(client_read).unwrap()
                    && site.target == OccurrenceTarget::Resolved { symbol }
            )
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "astro-template-semantics-unavailable")
    );
}

#[test]
fn astro_template_certificates_refuse_shadowed_computed_and_unproved_server_bindings() {
    for source in [
        "---\nlet posts = [];\n---\n{posts}",
        "---\nconst {posts = []} = props;\n---\n{posts}",
        "---\nconst {name: posts} = props;\n---\n{posts}",
        "---\nconst posts = []; const posts = [];\n---\n{posts}",
        "---\nconst posts = eval('[]');\n---\n{posts}",
        "---\nconst posts = [];\n---\n{posts[0]}",
        "---\nconst posts = [];\n---\n{items.map((posts) => <b>{posts}</b>)}",
        "---\nconst posts = [];\n---\n{(() => { const posts = []; return posts; })()}",
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "astro.template_binding_read.reference"),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "astro-template-semantics-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn astro_template_certificates_are_required_in_preflight_and_artifact_replay() {
    let source =
        "---\nconst {posts} = Astro.props;\n---\n<main>{posts.map(post => <b>{post}</b>)}</main>";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, ASTRO);
    let fixture = Fixture::new(ASTRO, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, ASTRO, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(
        fresh
            .document()
            .occurrences
            .iter()
            .any(
                |site| site.syntax_kind == "astro.template_binding_read.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            )
    );
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        demand
    );
    let replay = analyzer
        .analyze_from_artifact(
            &initial,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    let bounded = limits_with_syntax_records(demand - 1);
    let refused = request(&fixture.snapshot, &fixture.source, ASTRO, &bounded);
    assert!(
        analyzer
            .analyze_and_capture(
                &refused,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
}

#[test]
fn astro_template_certificates_keep_utf16_physical_source_and_target_spans() {
    let text = "---\r\n// é雪\r\nconst {posts} = Astro.props;\r\n---\r\n{posts.map(post => <b>{post}</b>)}";
    let mut raw = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        raw.extend_from_slice(&unit.to_le_bytes());
    }
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ASTRO, &raw);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(ASTRO),
        EncodingId::new("utf-16le").unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(ASTRO.generated);
    let output = analyze(
        &analyzer(&provider, ASTRO),
        &request,
        &ExtensionSupport::default(),
    );
    let read = text.find("{posts.map").unwrap() + 1;
    let read = 2 + text[..read].encode_utf16().count() * 2;
    let declaration = text.find("{posts}").unwrap() + 1;
    let declaration = 2 + text[..declaration].encode_utf16().count() * 2;
    let reference = output
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "astro.template_binding_read.reference")
        .unwrap();
    assert_eq!(
        reference.source.span().start_byte(),
        u64::try_from(read).unwrap()
    );
    assert_eq!(
        reference.source.span().end_byte(),
        u64::try_from(read + 10).unwrap()
    );
    assert_eq!(reference.source.content_hash(), content_hash(&raw));
    let OccurrenceTarget::Resolved { symbol } = reference.target else {
        panic!("physical target certificate");
    };
    let definition = output
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol }
        })
        .unwrap();
    assert_eq!(
        definition.source.span().start_byte(),
        u64::try_from(declaration).unwrap()
    );
    assert_eq!(definition.source.content_hash(), content_hash(&raw));
}

#[test]
fn astro_template_native_proof_survives_exact_required_capacity_and_refuses_one_less() {
    let original = include_bytes!("../fixtures/astro_source_fields/Grid.astro");
    let tail = format!("\n{{posts.map(post => ({}post))}}", "post + ".repeat(128));
    let raw = [original.as_slice(), tail.as_bytes()].concat();
    assert_eq!(&raw[..319], original);
    // Isolate record admission from the independently tested native depth bound.
    const NATIVE_DEPTH: usize = 512;
    let settings = ParserSettings::new(4096).unwrap();
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                MAX_SOURCE_BYTES,
                MAX_SYNTAX_NODES,
                NATIVE_DEPTH,
                32,
                64,
                1,
                16 * 1024 * 1024,
                settings,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let scoped_limits = |records| {
        let selected = limits_with_syntax_records(records);
        AnalysisLimits::new(
            selected.max_source_bytes(),
            selected.max_syntax_nodes(),
            NATIVE_DEPTH,
            selected.max_embedded_ranges(),
            selected.max_reported_memory_bytes(),
            selected.syntax_stream().clone(),
            selected.ir_stream().clone(),
            selected.ir().clone(),
        )
        .unwrap()
    };
    let analyzer = analyzer(&provider, ASTRO);
    let fixture = Fixture::new(ASTRO, &raw);
    let defaults = scoped_limits(limits().syntax_stream().max_records());
    let initial = request(&fixture.snapshot, &fixture.source, ASTRO, &defaults);
    let native = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &initial.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        native.report().coverage().status(),
        CoverageStatus::Complete,
        "record-quota calibration requires a complete native host: {:?}",
        native.report()
    );
    assert!(native.report().resources().max_syntax_depth() > MAX_SYNTAX_DEPTH);
    assert!(native.report().resources().max_syntax_depth() <= NATIVE_DEPTH);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let (wide, initial_artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(
        wide.document()
            .occurrences
            .iter()
            .any(
                |site| site.syntax_kind == "astro.template_binding_read.reference"
                    && site.source.span().start_byte() == 267
                    && site.source.span().end_byte() == 272
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            )
    );
    assert_eq!(
        initial_artifact
            .required_syntax_fact_count(&deadline())
            .unwrap(),
        demand
    );
    let exact_limits = scoped_limits(demand);
    let exact = request(&fixture.snapshot, &fixture.source, ASTRO, &exact_limits);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        demand
    );
    let replayed = analyzer
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replayed.document(), fresh.document());
    let document = replayed.document();
    let variable = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 161
                && site.source.span().end_byte() == 166
        })
        .unwrap();
    let field = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 127
                && site.source.span().end_byte() == 132
        })
        .unwrap();
    let read = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "astro.template_binding_read.reference"
                && site.source.span().start_byte() == 267
                && site.source.span().end_byte() == 272
        })
        .unwrap();
    assert!(matches!(variable.target, OccurrenceTarget::Resolved { .. }));
    assert_eq!(read.target, variable.target);
    assert_ne!(read.target, field.target);
    assert_eq!(read.source.content_hash(), content_hash(&raw));
    let small_limits = scoped_limits(demand - 1);
    let small = request(&fixture.snapshot, &fixture.source, ASTRO, &small_limits);
    assert_eq!(
        analyzer.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
            observed: demand,
            limit: demand - 1,
        })
    );
}

#[test]
fn astro_template_native_depth_limit_withholds_full_host_certificates() {
    let original = include_bytes!("../fixtures/astro_source_fields/Grid.astro");
    let tail = format!("\n{{posts.map(post => ({}post))}}", "post + ".repeat(128));
    let raw = [original.as_slice(), tail.as_bytes()].concat();
    assert_eq!(&raw[..319], original);
    let provider = provider();
    let fixture = Fixture::new(ASTRO, &raw);
    let defaults = limits();
    assert_eq!(defaults.max_syntax_depth(), 128);
    let request = request(&fixture.snapshot, &fixture.source, ASTRO, &defaults);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(parsed.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(parsed.report().resources().max_syntax_depth(), 128);
    for code in ["syntax-context-limit", "syntax-depth-limit"] {
        assert!(
            parsed.diagnostics().iter().any(|diagnostic| {
                diagnostic.code().as_str() == code && diagnostic.source() == Some(&fixture.source)
            }),
            "explicit whole-source native depth refusal: {code}"
        );
    }
    assert!(
        !parsed
            .facts()
            .iter()
            .any(|fact| { fact.syntax_kind().as_str().starts_with("astro.template_") }),
        "an incomplete native host cannot certify even the earlier clean read"
    );
    assert!(parsed.facts().iter().any(|fact| {
        fact.syntax_kind().as_str() == "typescript.identifier.reference"
            && fact.span().start_byte() == 267
            && fact.span().end_byte() == 272
    }));
    assert_eq!(fixture.source.content_hash(), content_hash(&raw));
}

fn assert_astro_bom_file_root_and_binding(encoding: &str) {
    let text = "---\r\n// é雪\r\nconst {posts} = Astro.props;\r\n---\r\n{posts.map(post => <b>{post}</b>)}";
    let mut raw = match encoding {
        "utf-8" => vec![0xef, 0xbb, 0xbf],
        "utf-16le" => vec![0xff, 0xfe],
        "utf-16be" => vec![0xfe, 0xff],
        _ => unreachable!(),
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
    let physical = |offset: usize| {
        u64::try_from(if encoding == "utf-8" {
            3 + offset
        } else {
            2 + 2 * text[..offset].encode_utf16().count()
        })
        .unwrap()
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ASTRO, &raw);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(ASTRO),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(ASTRO.generated);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(parsed.diagnostics().is_empty());
    let roots: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "astro.file.root")
        .collect();
    let modules: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "astro.file.module")
        .collect();
    assert_eq!(roots.len(), 1);
    assert_eq!(modules.len(), 1);
    for fact in [roots[0], modules[0]] {
        assert_eq!(
            fact.span(),
            fixture.source.span(),
            "complete physical file includes its known BOM: {encoding}"
        );
    }
    assert_eq!(roots[0].parent(), None);
    assert_eq!(modules[0].parent(), Some(roots[0].local_id()));
    let result = analyze(
        &analyzer(&provider, ASTRO),
        &request,
        &ExtensionSupport::default(),
    );
    let declaration = text.find("{posts}").unwrap() + 1;
    let read = text.find("{posts.map").unwrap() + 1;
    let definitions: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == physical(declaration)
                && site.source.span().end_byte() == physical(declaration + 5)
        })
        .collect();
    let reads: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "astro.template_binding_read.reference"
                && site.source.span().start_byte() == physical(read)
                && site.source.span().end_byte() == physical(read + 5)
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    assert_eq!(reads.len(), 1);
    assert!(matches!(
        definitions[0].target,
        OccurrenceTarget::Resolved { .. }
    ));
    assert_eq!(reads[0].target, definitions[0].target);
    assert_eq!(reads[0].source.content_hash(), content_hash(&raw));
    assert_eq!(reads[0].source.repository(), fixture.source.repository());
    assert_eq!(reads[0].source.generation(), fixture.source.generation());
    assert_eq!(reads[0].source.span().file(), fixture.source.span().file());
    assert_eq!(reads[0].provenance, definitions[0].provenance);
}

#[test]
fn astro_utf8_bom_file_root_preserves_full_source_and_exact_template_binding() {
    assert_astro_bom_file_root_and_binding("utf-8");
}

#[test]
fn astro_utf16le_file_root_preserves_full_source_and_exact_template_binding() {
    assert_astro_bom_file_root_and_binding("utf-16le");
}

#[test]
fn astro_utf16be_file_root_preserves_full_source_and_exact_template_binding() {
    assert_astro_bom_file_root_and_binding("utf-16be");
}

#[test]
fn original_astro_template_value_use_keeps_typed_read_and_existing_reference() {
    let raw = include_bytes!("../fixtures/astro_source_fields/Grid.astro");
    assert_eq!(raw.len(), 319);
    assert_eq!(&raw[161..166], b"posts");
    assert_eq!(&raw[267..272], b"posts");
    let original = std::str::from_utf8(raw).unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let bytes = if encoding == "utf-8" {
            raw.to_vec()
        } else {
            let little = encoding == "utf-16le";
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in original.encode_utf16() {
                bytes.extend_from_slice(&if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            bytes
        };
        let physical = |offset: usize| {
            if encoding == "utf-8" {
                u64::try_from(offset).unwrap()
            } else {
                2 + u64::try_from(original[..offset].encode_utf16().count() * 2).unwrap()
            }
        };
        let provider = Arc::new(provider());
        let fixture = Fixture::new(ASTRO, &bytes);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(ASTRO),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, ASTRO),
            &request,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        let target = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == physical(161)
                    && site.source.span().end_byte() == physical(166)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            target.len(),
            1,
            "actual own Variable Definition prerequisite"
        );
        let OccurrenceTarget::Resolved { symbol } = target[0].target else {
            panic!("resolved Definition prerequisite")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Variable);
        assert_eq!(entity.language, "typescript");
        assert_eq!(entity.canonical_name, "posts");
        let field = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == physical(127)
                    && site.source.span().end_byte() == physical(132)
            })
            .unwrap();
        assert_ne!(field.target, target[0].target);
        let reference = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == physical(267)
                    && site.source.span().end_byte() == physical(272)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            reference.len(),
            1,
            "preserved exact ordinary Reference prerequisite"
        );
        assert_eq!(reference[0].target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(
            reference[0].syntax_kind,
            "astro.template_binding_read.reference"
        );
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == physical(267)
                    && site.source.span().end_byte() == physical(272)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            reads.len(),
            1,
            "original value use must retain its independently requested Read role"
        );
        let read = reads[0];
        assert_eq!(read.syntax_kind, "astro.template_value_read.read");
        assert_ne!(read.syntax_kind, reference[0].syntax_kind);
        assert_eq!(read.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(read.source, reference[0].source);
        assert_eq!(
            read.source,
            SourceRef::new(
                document.repository,
                document.generation,
                SourceSpan::new(document.files[0].id, physical(267), physical(272)).unwrap(),
                content_hash(&bytes),
                None
            )
        );
        assert_eq!(read.provenance, reference[0].provenance);
        assert_eq!(read.syntactic_text_hash, content_hash(b"posts"));
        assert_ne!(read.id, reference[0].id);
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.syntax_kind == "astro.template_binding_read.reference"
                        && site.source.span() == read.source.span()
                })
                .count(),
            1,
            "old Reference label keeps its exact cardinality"
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(read.id)
                    && edge.object == RelationEndpoint::Entity(symbol)
                    && edge.evidence.source.as_ref() == Some(&read.source))
                .count(),
            1
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span() == read.source.span())
        );
    }
}

#[test]
fn astro_template_write_and_assignment_positions_cannot_acquire_read_facets() {
    for expression in [
        "posts = other",
        "posts += other",
        "posts ??= other",
        "posts++",
        "++posts",
        "posts.value = other",
        "posts.value++",
        "delete posts.value",
        "({posts} = other)",
    ] {
        let text = format!("---\nconst {{posts}} = Astro.props;\n---\n{{{expression}}}");
        let provider = provider();
        let fixture = Fixture::new(ASTRO, text.as_bytes());
        let budget = limits();
        let request = request(&fixture.snapshot, &fixture.source, ASTRO, &budget);
        let native = rootlight_adapter_sdk::execute_parse(
            &provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_ne!(
            native.report().coverage().status(),
            CoverageStatus::Unknown,
            "native source prerequisite: {expression}"
        );
        assert!(
            !native
                .facts()
                .iter()
                .any(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::ErrorRecovery),
            "clean native write-position prerequisite: {expression}"
        );
        assert!(
            !native
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "astro.template_binding_read.reference"),
            "no native value-read certificate: {expression}"
        );
        let result = output(&text);
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read),
            "no derived Read from a write position: {expression}"
        );
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert_eq!(
            result.document().files[0].content_hash,
            content_hash(text.as_bytes())
        );
    }
}

#[test]
fn astro_template_dual_facets_obey_exact_occurrence_relation_and_total_record_limits() {
    let raw = include_bytes!("../fixtures/astro_source_fields/Grid.astro");
    let provider = Arc::new(provider());
    let frontend = analyzer(&provider, ASTRO);
    let fixture = Fixture::new(ASTRO, raw);
    let base = limits();
    let run = |ir| {
        let budget = AnalysisLimits::new(
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
        execute_analysis(
            &frontend,
            &request(&fixture.snapshot, &fixture.source, ASTRO, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
    };
    let baseline = run(IrLimits::default()).unwrap();
    for role in [OccurrenceRole::Reference, OccurrenceRole::Read] {
        assert_eq!(
            baseline
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == role
                    && site.source.span().start_byte() == 267
                    && site.source.span().end_byte() == 272
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
                .count(),
            1
        );
    }
    for dimension in 0..3 {
        let make = |bound| {
            let mut ir = IrLimits::default();
            match dimension {
                0 => ir.max_occurrences = bound,
                1 => ir.max_relations = bound,
                _ => ir.max_total_records = bound,
            };
            ir
        };
        let mut low = 0;
        let mut high = 65_536;
        assert_eq!(run(make(high)).unwrap().document(), baseline.document());
        while low < high {
            let middle = low + (high - low) / 2;
            match run(make(middle)) {
                Ok(result) => {
                    assert_eq!(result.document(), baseline.document());
                    high = middle;
                }
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::Records,
                    ..
                })) => low = middle + 1,
                Err(error) => panic!("unexpected capacity prerequisite: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(make(low)).unwrap().document(), baseline.document());
        assert!(
            matches!(run(make(low - 1)), Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records, observed, limit
        })) if observed > limit && limit == low - 1)
        );
    }
}
