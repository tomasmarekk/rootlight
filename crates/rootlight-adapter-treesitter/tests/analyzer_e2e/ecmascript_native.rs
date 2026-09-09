//! Source-backed ECMAScript parameter and callable ownership contracts.
//! Both declared language identities use the audited native pattern classifier;
//! defaults, property keys and unrelated destructuring must not become parameters.

use super::*;

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
