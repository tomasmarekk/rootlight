//! Exercise native N-Quads role capture through the source-bound SDK parser.
//! These facts certify physical syntax, independently of typed semantic lowering.

use super::*;
use rootlight_adapter_treesitter::GrammarFamily;

const MEMORY: MemoryAdmissionPolicy = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

#[test]
fn nquads_sdk_captures_exact_direct_roles_without_a_default_graph_term() {
    let source = "#\0comment\r\n<s:><p:>\"λ\0\".\r\n<s:><p:>\"typed\"^^<urn:type><urn:g#fragment>.\r\n_:same<p:>_:same _:same.\r\n".as_bytes();
    let fixture = Fixture::new("roles.nq", source);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "nquads",
        Vec::new(),
    );
    let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
    assert_eq!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(output.facts().iter().any(|fact| fact.kind() == SyntaxFactKind::Root && fact.span() == fixture.source.span()));
    for (syntax, count) in [
        ("nquads.statement.expression", 3),
        ("nquads.subject.expression", 3),
        ("nquads.predicate.expression", 3),
        ("nquads.object.expression", 3),
        ("nquads.graph.expression", 2),
    ] {
        assert_eq!(
            output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == syntax)
                .count(),
            count,
            "{syntax}"
        );
    }
    let graphs: Vec<_> = output
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "nquads.graph.expression")
        .collect();
    let physical: Vec<_> = graphs
        .iter()
        .map(|fact| {
            &source[usize::try_from(fact.span().start_byte()).unwrap()
                ..usize::try_from(fact.span().end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(
        physical,
        [b"<urn:g#fragment>".as_slice(), b"_:same".as_slice()]
    );
    assert!(
        graphs
            .iter()
            .all(|fact| fact.kind() == SyntaxFactKind::Signature)
    );
    assert!(output.facts().iter().all(|fact| !matches!(
        fact.kind(),
        SyntaxFactKind::Declaration | SyntaxFactKind::Occurrence
    )));
}

#[test]
fn nquads_sdk_noop_and_graph_removal_match_fresh_physical_facts() {
    let original = b"<s:><p:>\"v\"<g:>.\n";
    let fixture = Fixture::new("incremental.nq", original);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let settings = ParserSettings::new(1024).unwrap();
    let cancellation = Cancellation::new();
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "nquads",
        Vec::new(),
    );
    let first = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancellation)
        .unwrap();
    assert_eq!(first.reuse_key().native_family(), GrammarFamily::NQuads);
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

    let changed = b"<s:><p:>\"v\".\n";
    let current = fixture.rewrite(changed);
    let input = request(
        &current.snapshot,
        &current.source,
        &limits,
        "nquads",
        Vec::new(),
    );
    let edited = provider
        .execute_with_previous(
            &input,
            Some(previous),
            &[SourceEdit::new(11, 15, "").unwrap()],
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
        edited.output().report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !edited
            .output()
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "nquads.graph.expression")
    );
    assert_eq!(
        edited.reuse_key().current_content_hash(),
        content_hash(changed)
    );
    assert_eq!(fixture.snapshot.content(), original);
}

#[test]
fn nquads_sdk_recovery_and_ntriples_route_do_not_certify_quad_roles() {
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    for source in [
        b"<s:><p:><o:>\"g\".".as_slice(),
        b"<s:><p:><o:><g:><extra:>.",
        b"<alice><follows><bob>.",
        b"<s:><p:><o:><urn:\0g>.",
    ] {
        let fixture = Fixture::new("invalid.nq", source);
        let input = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "nquads",
            Vec::new(),
        );
        let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(
            !output
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "nquads.statement.expression"),
            "{source:?}"
        );
    }
    let fixture = Fixture::new("quad.nt", b"<s:><p:><o:><g:>.");
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "ntriples",
        Vec::new(),
    );
    let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str().starts_with("nquads."))
    );
}
