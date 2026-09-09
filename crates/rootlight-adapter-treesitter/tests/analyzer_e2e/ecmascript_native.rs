//! Source-backed ECMAScript binding and lexical ownership contracts.
//! Both language identities use native binding fields; defaults, property keys
//! and assignment patterns must not invent declarations.

use super::*;

#[test]
fn ecmascript_pattern_bindings_replay_with_exact_generation_evidence() {
    let source = "const {名: alias = fallback, shorthand = defaultValue, nested: [, [element = initial], ...tail]} = source;\r\nfor (let index = 0; index < limit; index++) { let [value] = items; }\r\ntry { run(); } catch ([caught]) { consume(caught); }\r\n";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let demand = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        let bounded = limits_with_syntax_records(demand);
        let initial = request(&fixture.snapshot, &fixture.source, case, &bounded);
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            artifact.required_syntax_fact_count(&deadline()).unwrap(),
            demand
        );
        assert!(
            first
                .document()
                .skipped_regions
                .iter()
                .all(|gap| gap.domain != FactDomain::Entities),
            "{:#?}",
            first.document().skipped_regions
        );
        let insufficient = limits_with_syntax_records(demand.checked_sub(1).unwrap());
        let insufficient = request(&fixture.snapshot, &fixture.source, case, &insufficient);
        let error = analyzer
            .analyze_and_capture(
                &insufficient,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap_err();
        assert!(
            matches!(
                error,
                AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
                    observed, limit,
                }) if observed == demand && limit == demand - 1
            ),
            "{error:?}"
        );
        let names: BTreeSet<_> = first
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "alias",
                "shorthand",
                "element",
                "tail",
                "index",
                "value",
                "caught"
            ]),
            "{}: {:#?}",
            case.name,
            first.document().skipped_regions
        );
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &bounded);
        let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
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
                .filter(|entity| entity.kind == EntityKind::Variable)
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(identities(first.document()), identities(replay.document()));
        for occurrence in &replay.document().occurrences {
            assert_eq!(occurrence.source.generation(), next.source.generation());
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn ecmascript_variable_patterns_preserve_only_declared_bindings() {
    let source = "const {a, key: renamed = initializer, nested: [element], [lookup()]: computed, ...objectRest} = input; let [head, ...arrayRest] = values; for (const {item} of items) { consume(item); } for (let index in records) { consume(index); } for (assigned of items) consume(assigned); ({outside} = input); try { run(); } catch ({message: reason, ...details}) { consume(reason); } try { run(); } catch (error) { consume(error); } var simple = 0;";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, case, &limits()),
            &ExtensionSupport::default(),
        );
        let document = result.document();
        assert!(
            document
                .skipped_regions
                .iter()
                .all(|gap| gap.domain != FactDomain::Entities),
            "{:#?}",
            document.skipped_regions
        );
        let variables: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .collect();
        let names: BTreeSet<_> = variables
            .iter()
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "a",
                "renamed",
                "element",
                "computed",
                "objectRest",
                "head",
                "arrayRest",
                "item",
                "index",
                "reason",
                "details",
                "error",
                "simple"
            ]),
            "{}: {:#?}",
            case.name,
            document.skipped_regions
        );
        assert_eq!(variables.len(), 13);
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Parameter)
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
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(text, variable.canonical_name);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(text.as_bytes())
            );
        }
    }
}

#[test]
fn ecmascript_block_loop_and_catch_bindings_have_distinct_stable_owners() {
    let source = "{ const value = first; consume(value); } { const value = second; consume(value); } for (const value of items) consume(value); for (const value of items) consume(value); try { run(); } catch (value) { consume(value); } try { run(); } catch (value) { consume(value); }";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let mut identities = Vec::new();
        for source in [
            source.to_owned(),
            source.replace("consume(value)", "consume( value )"),
            format!("{{ const unrelated = 0; }} {source}"),
        ] {
            let fixture = Fixture::new(case, source.as_bytes());
            let result = analyze(
                &analyzer,
                &request(&fixture.snapshot, &fixture.source, case, &limits()),
                &ExtensionSupport::default(),
            );
            let variables: BTreeSet<_> = result
                .document()
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "value"
                })
                .map(|entity| entity.id)
                .collect();
            assert_eq!(
                variables.len(),
                6,
                "{}: {:#?}",
                case.name,
                result.document().skipped_regions
            );
            identities.push(variables);
        }
        assert_eq!(identities[0], identities[1]);
        assert_eq!(identities[0], identities[2]);
    }
}

#[test]
fn ecmascript_parameter_patterns_keep_only_written_binding_names() {
    let source = "function read({a, key: renamed = initializer, nested: [element], [lookup()]: computed, ...objectRest}, [head, ...arrayRest], plain = defaultValue, ...args) { const [localOnly] = values; return plain; } const {outside} = values;";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, case, &limits()),
            &ExtensionSupport::default(),
        );
        let document = result.document();
        let parameters: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .collect();
        let names: BTreeSet<_> = parameters
            .iter()
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        assert_eq!(
            names,
            BTreeSet::from([
                "a",
                "renamed",
                "element",
                "computed",
                "objectRest",
                "head",
                "arrayRest",
                "plain",
                "args"
            ]),
            "{}: {:#?}",
            case.name,
            document.skipped_regions
        );
        assert_eq!(parameters.len(), 9);
        for parameter in parameters {
            assert_eq!(parameter.language, case.name);
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
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(text, parameter.canonical_name);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(text.as_bytes())
            );
        }
    }
}

#[test]
fn typescript_callable_parameters_replay_with_exact_generation_evidence() {
    let source = "function read(value: string, optional?: number, ...rest: string[]) { return value; } class Store { constructor(public field: string) {} method([entry]: string[]) { return entry; } } const callback = (arrow: string) => arrow; type Callback = (typed: string) => string; function* generate(yielded: number) { yield yielded; } const nested = function named(inner: number) { return inner; }; const generator = function* stream(step: number) { yield step; };";
    let case = *CASES.iter().find(|case| case.name == "typescript").unwrap();
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let bounded = limits_with_syntax_records(demand);
    let initial = request(&fixture.snapshot, &fixture.source, case, &bounded);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        demand,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    let expected = BTreeSet::from([
        "value", "optional", "rest", "field", "entry", "arrow", "typed", "yielded", "inner", "step",
    ]);
    let names: BTreeSet<_> = first
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Parameter)
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, expected, "{:#?}", first.document().skipped_regions);
    for name in ["generate", "named", "stream"] {
        assert!(
            first
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name),
            "{name}"
        );
    }
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, case, &bounded);
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
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
    let ids = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(first.document()), ids(replay.document()));
    for occurrence in &replay.document().occurrences {
        assert_eq!(occurrence.source.generation(), next.source.generation());
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
    let edited = source.replace("return value;", "return  value;");
    let edited_fixture = Fixture::new(case, edited.as_bytes());
    let edited = analyze(
        &analyzer,
        &request(
            &edited_fixture.snapshot,
            &edited_fixture.source,
            case,
            &budget,
        ),
        &ExtensionSupport::default(),
    );
    assert_eq!(ids(first.document()), ids(edited.document()));
}

#[test]
fn ecmascript_sibling_callback_parameters_remain_distinct_and_body_stable() {
    let source = "use(value => value, value => value);";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let mut identities = Vec::new();
        for source in [
            source.to_owned(),
            source.replace("=> value", "=>  value + 1"),
        ] {
            let fixture = Fixture::new(case, source.as_bytes());
            let result = analyze(
                &analyzer,
                &request(&fixture.snapshot, &fixture.source, case, &limits()),
                &ExtensionSupport::default(),
            );
            let parameters: BTreeSet<_> = result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Parameter)
                .map(|entity| entity.id)
                .collect();
            assert_eq!(
                parameters.len(),
                2,
                "{}: {:#?}",
                case.name,
                result.document().skipped_regions
            );
            identities.push(parameters);
        }
        assert_eq!(identities[0], identities[1]);
    }
}
