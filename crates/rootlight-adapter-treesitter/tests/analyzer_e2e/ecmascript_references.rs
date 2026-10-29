//! Native reference roles distinguish type names from value expressions.
//! Shorthand object reads and assignment targets retain their authored leaves;
//! a type query still refers to a value, not a same-named type declaration.

use super::*;
use rootlight_adapter_sdk::testkit::MockParseProvider;

#[test]
fn javascript_plain_formal_receivers_resolve_only_their_own_lexical_binding() {
    let case = *CASES.iter().find(|case| case.name == "javascript").unwrap();
    for (source, name, owner, receiver) in [
        (
            "function capitalize(text){\n    return text.charAt(0).toUpperCase() + text.slice(1);\n}\n\nmodule.exports = {capitalize};\n",
            "text",
            "capitalize",
            "text.charAt",
        ),
        (
            "function sendJson(response, status, value, contentType) {\n  response.writeHead(status, {\n    \"content-type\": `${contentType ?? \"application/json\"}; charset=utf-8`,\n    \"cache-control\": \"no-store\",\n  });\n  response.end(JSON.stringify(value));\n}\n",
            "response",
            "sendJson",
            "response.writeHead",
        ),
        (
            "const text = 0; function first(text) { return text.first(\"eval(code); with(other){}\"); } function second(text) { return text.second(); }",
            "text",
            "first",
            "text.first",
        ),
        (
            "const text = 0; function first(text) { return text.first(\"eval(code); with(other){}\"); } function second(text) { return text.second(); }",
            "text",
            "second",
            "text.second",
        ),
    ] {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let input = request(&fixture.snapshot, &fixture.source, case, &budget);
        let (result, artifact) = analyzer
            .analyze_and_capture(
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let document = result.document();
        let function = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(function.id))
            })
            .unwrap();
        assert_eq!(
            parameter.container,
            Some(rootlight_ir::ContainerRef::Entity(function.id))
        );
        let start = u64::try_from(source.find(receiver).unwrap()).unwrap();
        let end = start + u64::try_from(name.len()).unwrap();
        let references = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .collect::<Vec<_>>();
        assert_eq!(references.len(), 1);
        let reference = references[0];
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{source}"
        );
        assert_eq!(reference.enclosing, Some(function.id));
        assert_eq!(
            reference.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(reference.id)
                && edge.object == RelationEndpoint::Entity(parameter.id)
                && edge.evidence.source.as_ref() == Some(&reference.source)
        }));
        assert!(
            document
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::CallSite
                    || site.syntax_kind == "javascript.member_name.reference")
                .all(|site| site.target
                    != (OccurrenceTarget::Resolved {
                        symbol: parameter.id
                    }))
        );
        assert!(
            document
                .relations
                .iter()
                .all(|edge| edge.predicate != RelationPredicate::Calls)
        );
        let next = fixture.next_generation();
        let next_input = request(&next.snapshot, &next.source, case, &budget);
        let replay = analyzer
            .analyze_from_artifact(
                &next_input,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let fresh = analyze(&analyzer, &next_input, &ExtensionSupport::default());
        assert_eq!(replay.document(), fresh.document());
        assert_eq!(replay.report(), fresh.report());
    }
}

#[test]
fn javascript_plain_formal_receivers_refuse_shadow_write_dynamic_and_nonordinary_bodies() {
    let case = *CASES.iter().find(|case| case.name == "javascript").unwrap();
    for source in [
        "function read(text, text) { return text.charAt(0); }",
        "function read(text = other) { return text.charAt(0); }",
        "function read(...text) { return text.charAt(0); }",
        "function read({text}) { return text.charAt(0); }",
        "function read(text: string) { return text.charAt(0); }",
        "function read(text) { var text = other; return text.charAt(0); }",
        "function read(text) { const {text} = other; return text.charAt(0); }",
        "function read(text) { text = other; return text.charAt(0); }",
        "function read(text) { text += other; return text.charAt(0); }",
        "function read(text) { text++; return text.charAt(0); }",
        "function read(text) { ({text} = other); return text.charAt(0); }",
        "function read(text) { { let text = other; text.charAt(0); } return text.charAt(0); }",
        "function read(text) { try {} catch (text) { text.charAt(0); } return text.charAt(0); }",
        "function read(text) { function nested(text) { return text.charAt(0); } return text.charAt(0); }",
        "function read(text) { const nested = (text) => text.charAt(0); return text.charAt(0); }",
        "function read(text) { eval(code); return text.charAt(0); }",
        "function read(text) { \\u0065val(code); return text.charAt(0); }",
        "function read(text) { with (other) {} return text.charAt(0); }",
        "function read(text) { type text = string; return text.charAt(0); }",
        "function read(text) { return text.charAt(0) + ; }",
        "async function read(text) { return text.charAt(0); }",
        "const read = function named(text) { return text.charAt(0); };",
        "function same(text) { return text.charAt(0); } function same(text) { return text.slice(0); }",
    ] {
        let native = Arc::new(provider());
        let fixture = Fixture::new(case, source.as_bytes());
        let result = analyze(
            &analyzer(&native, case),
            &request(&fixture.snapshot, &fixture.source, case, &limits()),
            &ExtensionSupport::default(),
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::Reference)
                .all(|site| { !matches!(site.target, OccurrenceTarget::Resolved { .. }) }),
            "{source}: {:#?}",
            result.document().occurrences
        );
    }
}

#[test]
fn javascript_plain_formal_receivers_require_complete_current_source() {
    let case = *CASES.iter().find(|case| case.name == "javascript").unwrap();
    let original = b"function read(text) { harmless(); return text.charAt(0); }";
    let native = Arc::new(provider());
    let fixture = Fixture::new(case, original);
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        native.as_ref(),
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let frontend = analyzer(&native, case);
    let (_, artifact) = frontend
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.rewrite(b"function read(text) { eval(code); return text.charAt(0); }");
    let stale_parser = MockParseProvider::new(
        native.capabilities().clone(),
        parsed.facts().to_vec(),
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let stale = TreeSitterAnalyzer::new(
        Arc::new(stale_parser),
        producer_identity(),
        language(case),
        case.frontend,
        content_hash(BINARY_SEED),
    )
    .unwrap();
    let current = request(&changed.snapshot, &changed.source, case, &budget);
    let result = execute_analysis(
        &stale,
        &current,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        matches!(frontend.analyze_from_artifact(&current, &artifact, ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
    let included = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        EncodingId::utf8(),
        vec![IncludedRange::new(fixture.source.span(), language(case))],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(&frontend, &included, &ExtensionSupport::default());
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn ecmascript_escaped_definitions_preserve_authored_spans_and_replay() {
    let source = "function \\u004cocal(value) { return value; }\r\n\\u004cocal(1);";
    for language in ["javascript", "typescript"] {
        let case = CASES
            .iter()
            .copied()
            .find(|case| case.name == language)
            .unwrap();
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let entity = first
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Local")
            .unwrap();
        let definition = first
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        assert_eq!(definition.source.span().start_byte(), 9);
        assert_eq!(definition.source.span().end_byte(), 19);
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &budget);
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
        assert!(
            replay
                .document()
                .occurrences
                .iter()
                .all(|occurrence| occurrence.source.generation() == next.source.generation())
        );
    }
}

#[test]
fn typescript_type_parameter_definitions_keep_exact_identity_and_replay() {
    for source in [
        "function first<名>(value: 名) {}\r\ntype Box<名> = 名;\r\nclass Holder<名> { value!: 名; }",
        "type Box = { [名 in 'a']: 名 } |\r\n{ [名 in 'b']: { [名 in 'c']: 名 } };",
        "type Box = string extends { method(value: infer /* bind */ 名): infer 名 } ? 名 : never;\r\ntype Other = string extends infer 名 ? 名 : never;\r\ntype Third = string extends infer 名 extends string ? 名 : never;",
    ] {
        assert_type_parameter_identity_and_replay(source);
    }
}

fn assert_type_parameter_identity_and_replay(source: &str) {
    let case = CASES
        .iter()
        .copied()
        .find(|case| case.name == "typescript")
        .unwrap();
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let parameters: Vec<_> = first
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::TypeParameter)
        .collect();
    assert_eq!(parameters.len(), 3);
    let identities: BTreeSet<_> = parameters.iter().map(|entity| entity.id).collect();
    assert_eq!(identities.len(), 3);
    for (offset, _) in source.match_indices('名').filter(|(offset, _)| {
        let prefix = &source[..*offset];
        prefix.ends_with('<')
            || prefix.ends_with('[')
            || prefix.ends_with("infer ")
            || prefix.ends_with("/* bind */ ")
    }) {
        let start = u64::try_from(offset).unwrap();
        let definition = first
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("missing generic binder")
        };
        assert!(identities.contains(&symbol));
        assert_eq!(definition.source.span().end_byte(), start + 3);
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, case, &budget);
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
    for occurrence in &replay.document().occurrences {
        assert_eq!(occurrence.source.generation(), next.source.generation());
    }
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let required_budget = limits_with_syntax_records(required);
    let required_request = request(&fixture.snapshot, &fixture.source, case, &required_budget);
    let (bounded, bounded_artifact) = analyzer
        .analyze_and_capture(
            &required_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        bounded
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::TypeParameter)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        identities
    );
    let next_bounded_request = request(&next.snapshot, &next.source, case, &required_budget);
    let fresh_bounded = analyze(
        &analyzer,
        &next_bounded_request,
        &ExtensionSupport::default(),
    );
    let replay_bounded = analyzer
        .analyze_from_artifact(
            &next_bounded_request,
            &bounded_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh_bounded.document(), replay_bounded.document());
    assert_eq!(fresh_bounded.report(), replay_bounded.report());
}

#[test]
fn qualified_reference_fields_keep_leaf_roles_and_generation_bound_replay() {
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let source = if case.name == "typescript" {
            "import type * as Space from './provider'; type Value = typeof Space /* café */ .Public; let typed: Space.Nested.Contract;"
        } else {
            "import * as Space from './provider'; const value = Space /* café */ ?.Public; const nested = Space.Nested.Item;"
        };
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &initial.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let expected = if case.name == "typescript" {
            [
                ("Public", "type_query_member_name.reference"),
                ("Nested", "type_namespace_member.reference"),
                ("Contract", "type_member_name.reference"),
            ]
        } else {
            [
                ("Public", "member_name.reference"),
                ("Nested", "member_name.reference"),
                ("Item", "member_name.reference"),
            ]
        };
        for (name, suffix) in expected {
            let start = u64::try_from(source.find(name).unwrap()).unwrap();
            assert!(
                parsed
                    .facts()
                    .iter()
                    .any(|fact| fact.syntax_kind().as_str().ends_with(suffix)
                        && fact.span().start_byte() == start
                        && fact.span().end_byte() == start + u64::try_from(name.len()).unwrap()),
                "{source}: {name}/{suffix}"
            );
        }
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert!(
            !first
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.syntax_kind.ends_with(".member_path.reference"))
        );
        let next = fixture.next_generation();
        let next_request = request(&next.snapshot, &next.source, case, &budget);
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
fn typescript_reference_roles_replay_under_reduced_and_required_budgets() {
    let source = "type 名 = string; const 名 = 'value'; let typed: 名 = 名; const object = {名}; export type {名 as Public};\r\n";
    let case = *CASES.iter().find(|case| case.name == "typescript").unwrap();
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
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
        required
    );
    assert_eq!(
        first
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
            .count(),
        2
    );
    let bounded = limits_with_syntax_records(artifact.syntax_fact_count().checked_add(1).unwrap());
    assert!(bounded.syntax_stream().max_records() < budget.syntax_stream().max_records());
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
    assert_eq!(
        replay
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
            .count(),
        2
    );
    for occurrence in &replay.document().occurrences {
        assert_eq!(occurrence.source.generation(), next.source.generation());
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
    let required_budget = limits_with_syntax_records(required);
    assert!(!artifact.is_compatible_with_limits(&required_budget));
    let (_, required_artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, case, &required_budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required_request = request(&next.snapshot, &next.source, case, &required_budget);
    let required_fresh = analyze(&analyzer, &required_request, &ExtensionSupport::default());
    let required_replay = analyzer
        .analyze_from_artifact(
            &required_request,
            &required_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(required_fresh.document(), required_replay.document());
    assert_eq!(required_fresh.report(), required_replay.report());
    assert!(
        required_fresh
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-extraction-limit")
    );
    assert!(
        required_fresh
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::TypeUse)
            .count()
            < 2
    );
    let insufficient = limits_with_syntax_records(required.checked_sub(1).unwrap());
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, case, &insufficient),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(rootlight_adapter_sdk::SinkError::StreamLimit { resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts, observed, limit }) if observed == required && limit == required - 1),
        "{error:?}"
    );
}

#[test]
fn typescript_type_and_value_occurrences_keep_distinct_roles() {
    let source = "type Token = string; const Token = 'value'; let typed: Token = Token; type Query = typeof Token;\r\n";
    let case = *CASES.iter().find(|case| case.name == "typescript").unwrap();
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    for (needle, offset, role) in [
        ("typed: Token", "typed: ".len(), OccurrenceRole::TypeUse),
        ("= Token;", "= ".len(), OccurrenceRole::Reference),
        ("typeof Token", "typeof ".len(), OccurrenceRole::Reference),
    ] {
        let start = u64::try_from(source.find(needle).unwrap() + offset).unwrap();
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte() == start + 5
            })
            .unwrap();
        assert_eq!(occurrence.role, role, "{needle}: {occurrence:?}");
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
}

#[test]
fn ecmascript_shorthand_reads_and_writes_retain_exact_references() {
    let source = "let value = 1; const object = {value}; ({value} = source); ({value = fallback} = source); const {declared} = source;\r\n";
    for case in CASES
        .iter()
        .copied()
        .filter(|case| matches!(case.name, "javascript" | "typescript"))
    {
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        for needle in ["{value}", "{value} =", "{value ="] {
            let start = u64::try_from(source.find(needle).unwrap() + 1).unwrap();
            let occurrence = result
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.source.span().start_byte() == start
                        && occurrence.source.span().end_byte() == start + 5
                })
                .unwrap_or_else(|| panic!("{}: missing {needle}", case.name));
            assert_eq!(occurrence.role, OccurrenceRole::Reference);
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
        let declared_start = u64::try_from(source.find("{declared}").unwrap() + 1).unwrap();
        let declared: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.source.span().start_byte() == declared_start)
            .collect();
        assert_eq!(declared.len(), 1);
        assert_eq!(declared[0].role, OccurrenceRole::Definition);
    }
}
