//! Native reference roles distinguish type names from value expressions.
//! Shorthand object reads and assignment targets retain their authored leaves;
//! a type query still refers to a value, not a same-named type declaration.

use super::*;

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
