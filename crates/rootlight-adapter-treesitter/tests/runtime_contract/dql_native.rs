//! Native DQL captures preserve physical query roles without software targets.

use super::*;

const MEMORY: MemoryAdmissionPolicy = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

fn spellings<'a>(
    facts: &[rootlight_adapter_sdk::SyntaxFact],
    source: &'a [u8],
    role: &str,
) -> Vec<&'a str> {
    let syntax = format!("dql.{role}.expression");
    facts
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == syntax)
        .map(|fact| {
            assert_eq!(fact.kind(), SyntaxFactKind::Signature);
            let span = fact.span();
            std::str::from_utf8(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
            )
            .unwrap()
        })
        .collect()
}

#[test]
fn dql_sdk_preserves_original_roots_filters_reverse_predicates_and_language() {
    // The pinned Apache-2.0 originals and notice are retained with the grammar.
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
        let fixture = Fixture::new("query.json", source);
        let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
        let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
        let input = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "dql",
            Vec::new(),
        );
        let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(output.facts().iter().any(
            |fact| fact.kind() == SyntaxFactKind::Root && fact.span() == fixture.source.span()
        ));
        assert_eq!(spellings(output.facts(), source, "root").len(), roots);
        assert_eq!(spellings(output.facts(), source, "generator").len(), roots);
        assert_eq!(spellings(output.facts(), source, "filter").len(), filters);
        assert_eq!(spellings(output.facts(), source, "count").len(), counts);
        assert_eq!(
            spellings(output.facts(), source, "filter_call").len(),
            filters
        );
        assert_eq!(
            spellings(output.facts(), source, "nested_call").len(),
            filters
        );
        assert_eq!(
            spellings(output.facts(), source, "predicate")
                .iter()
                .filter(|text| **text == "~friend")
                .count(),
            filters / 2
        );
        assert_eq!(
            spellings(output.facts(), source, "language"),
            if roots == 4 { vec!["@."] } else { Vec::new() }
        );
        assert!(output.facts().iter().all(|fact| !matches!(
            fact.kind(),
            SyntaxFactKind::Declaration | SyntaxFactKind::Occurrence
        )));
    }
}

#[test]
fn dql_sdk_owns_bindings_languages_and_opaque_tokens_without_forged_variables() {
    let source = br#"query find($term: string = "name@filter $fake") {
      q(func: eq(name, $term)) @filter(eq(name, "f as uid(fake)")) {
        f as friends
        name@filter
        name@en @filter(eq(name, "x"))
      }
      r(func: uid(f)) { alias: val(f) }
      # forged as uid(comment)
    }"#;
    let fixture = Fixture::new("query.dql", source);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "dql",
        Vec::new(),
    );
    let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
    assert_eq!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    for (role, expected) in [
        ("binding_name", vec!["f"]),
        ("parameter_name", vec!["$term"]),
        ("parameter_use", vec!["$term"]),
        ("variable_use", vec!["f", "f"]),
        ("alias_name", vec!["alias"]),
        ("language", vec!["@filter", "@en"]),
    ] {
        assert_eq!(spellings(output.facts(), source, role), expected, "{role}");
    }
    assert_eq!(spellings(output.facts(), source, "filter").len(), 2);
    for source in [
        b"{ q(func: uid(~friend)) { name } }".as_slice(),
        b"{ q(func: uid(name@en)) { name } }",
        b"{ q(func: uid(<urn:value>)) { name } }",
    ] {
        let fixture = Fixture::new("unproven.dql", source);
        let input = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "dql",
            Vec::new(),
        );
        let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
        assert!(spellings(output.facts(), source, "variable_use").is_empty());
    }
}

#[test]
fn dql_sdk_noop_and_predicate_edit_equal_fresh_physical_facts() {
    let original = b"{ q(func: has(name)) { name } }\r\n";
    let fixture = Fixture::new("incremental.dql", original);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let settings = ParserSettings::new(1024).unwrap();
    let cancellation = Cancellation::new();
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "dql",
        Vec::new(),
    );
    let first = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancellation)
        .unwrap();
    assert_eq!(
        first.reuse_key().native_family(),
        rootlight_adapter_treesitter::GrammarFamily::Dql
    );
    let previous = first.previous().unwrap();
    let noop = provider
        .execute_with_previous(&input, Some(previous), &[], settings, MEMORY, &cancellation)
        .unwrap();
    assert_eq!(
        noop.reuse_status(),
        ReuseStatus::Reused { changed_ranges: 0 }
    );
    assert_eq!(noop.output().facts(), first.output().facts());
    assert_eq!(noop.output().report(), first.output().report());
    let start = original
        .windows(4)
        .position(|bytes| bytes == b"name")
        .unwrap();
    let mut changed = original.to_vec();
    changed[start..start + 4].copy_from_slice(b"kind");
    let current = fixture.rewrite(&changed);
    let input = request(
        &current.snapshot,
        &current.source,
        &limits,
        "dql",
        Vec::new(),
    );
    let edited = provider
        .execute_with_previous(
            &input,
            Some(previous),
            &[SourceEdit::new(start, start + 4, "kind").unwrap()],
            settings,
            MEMORY,
            &cancellation,
        )
        .unwrap();
    assert!(matches!(edited.reuse_status(), ReuseStatus::Reused { .. }));
    let fresh = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancellation)
        .unwrap();
    assert_eq!(edited.output().facts(), fresh.output().facts());
    assert_eq!(edited.output().report(), fresh.output().report());
    assert_eq!(
        fresh.output().report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        spellings(edited.output().facts(), &changed, "predicate"),
        ["kind", "name"]
    );
    assert_eq!(
        edited.reuse_key().current_content_hash(),
        content_hash(&changed)
    );
    assert_eq!(fixture.snapshot.content(), original);
}

#[test]
fn dql_sdk_refuses_json_graphql_schema_and_malformed_native_roots() {
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    for source in [
        br#"{"query":"{ q(func: uid(1)) { name } }"}"#.as_slice(),
        b"{ user(id: 1) { name } }",
        b"friend: [uid] @count @reverse .",
        b"{ q(func: uid(1)) @filter { name } }",
    ] {
        let fixture = Fixture::new("unproven.dql", source);
        let input = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "dql",
            Vec::new(),
        );
        let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(spellings(output.facts(), source, "root").is_empty());
    }
}
