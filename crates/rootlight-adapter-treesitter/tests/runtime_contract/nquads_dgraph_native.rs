//! Exercise native Dgraph physical roles through the source-bound SDK parser.
//! Syntax coverage does not certify unsupported Dgraph semantic constructs.

use super::*;
use rootlight_adapter_treesitter::GrammarFamily;

const MEMORY: MemoryAdmissionPolicy = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

// Apache-2.0 fixture from dgraph-io/dgraph at the pinned source revision.
// https://github.com/dgraph-io/dgraph/blob/c587cba75f60445125459b46b4d81757a73384aa/dgraph/cmd/alpha/testdata.nq
// The complete notice is retained in native-grammars/nquads-dgraph/LICENSE-DGRAPH.
const ORIGINAL: &[u8] = b"<alice> <follows> <bob> .\n<bob> <follows> <fred> .\n<bob> <status> \"I'm your Uncle Bob!\" .\n<charlie> <follows> <bob> .\n<charlie> <follows> <dani> .\n<dani> <follows> <bob> .\n<dani> <follows> <greg> .\n<dani> <status> \"Smart Dani\" .\n<emily> <follows> <fred> .\n<fred> <follows> <greg> .\n<greg> <status> \"Mr. Cool greg\" .\n";

#[test]
fn dgraph_sdk_preserves_all_original_direct_term_spans_without_rdf_graphs() {
    assert_eq!(ORIGINAL.len(), 316);
    let fixture = Fixture::new("original.nq", ORIGINAL);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "nquads-dgraph",
        Vec::new(),
    );
    let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
    assert_eq!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(output.facts().iter().any(|fact| fact.kind() == SyntaxFactKind::Root && fact.span() == fixture.source.span()));
    let expected = [
        ["<alice>", "<follows>", "<bob>"],
        ["<bob>", "<follows>", "<fred>"],
        ["<bob>", "<status>", "\"I'm your Uncle Bob!\""],
        ["<charlie>", "<follows>", "<bob>"],
        ["<charlie>", "<follows>", "<dani>"],
        ["<dani>", "<follows>", "<bob>"],
        ["<dani>", "<follows>", "<greg>"],
        ["<dani>", "<status>", "\"Smart Dani\""],
        ["<emily>", "<follows>", "<fred>"],
        ["<fred>", "<follows>", "<greg>"],
        ["<greg>", "<status>", "\"Mr. Cool greg\""],
    ];
    for (role_index, role) in ["statement", "subject", "predicate", "object"]
        .into_iter()
        .enumerate()
    {
        let syntax = format!("nquads-dgraph.{role}.expression");
        let facts: Vec<_> = output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == syntax)
            .collect();
        assert_eq!(facts.len(), 11, "{syntax}");
        assert!(
            facts
                .iter()
                .all(|fact| fact.kind() == SyntaxFactKind::Signature)
        );
        let mut offset = 0;
        for ((fact, line), terms) in facts
            .iter()
            .zip(ORIGINAL.split_inclusive(|byte| *byte == b'\n'))
            .zip(expected)
        {
            let line_end = offset + line.len() - 1;
            let range = usize::try_from(fact.span().start_byte()).unwrap()
                ..usize::try_from(fact.span().end_byte()).unwrap();
            if role_index == 0 {
                assert_eq!(range, offset..line_end);
            } else {
                let term_offset = terms[..role_index - 1]
                    .iter()
                    .map(|term| term.len() + 1)
                    .sum::<usize>();
                let term = terms[role_index - 1].as_bytes();
                assert_eq!(
                    range,
                    offset + term_offset..offset + term_offset + term.len()
                );
                assert_eq!(&ORIGINAL[range], term);
            }
            offset += line.len();
        }
    }
    assert_eq!(
        output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "nquads-dgraph.default-value.string")
            .count(),
        3
    );
    assert!(
        output
            .facts()
            .iter()
            .all(|fact| !fact.syntax_kind().as_str().contains(".graph."))
    );
    assert!(output.facts().iter().all(|fact| !matches!(
        fact.kind(),
        SyntaxFactKind::Declaration | SyntaxFactKind::Occurrence
    )));
}

#[test]
fn dgraph_sdk_noop_and_value_edit_equal_fresh_physical_facts() {
    let original = b"<alice><status>\"first\".\n";
    let fixture = Fixture::new("incremental.nq", original);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let settings = ParserSettings::new(1024).unwrap();
    let cancellation = Cancellation::new();
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "nquads-dgraph",
        Vec::new(),
    );
    let first = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancellation)
        .unwrap();
    assert_eq!(
        first.reuse_key().native_family(),
        GrammarFamily::NQuadsDgraph
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
    let changed = b"<alice><status>\"\\xFF\\v\".\n";
    let current = fixture.rewrite(changed);
    let input = request(
        &current.snapshot,
        &current.source,
        &limits,
        "nquads-dgraph",
        Vec::new(),
    );
    let edited = provider
        .execute_with_previous(
            &input,
            Some(previous),
            &[SourceEdit::new(16, 21, "\\xFF\\v").unwrap()],
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
    assert_eq!(
        edited.reuse_key().current_content_hash(),
        content_hash(changed)
    );
    assert_eq!(fixture.snapshot.content(), original);
}

#[test]
fn dgraph_sdk_recovery_and_unsupported_constructs_grant_no_term_metadata() {
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    for source in [
        b"<alice><follows><bob> <0x01>.".as_slice(),
        b"<alice><follows><bob> 0x01.",
        b"<alice><follows><bob> 1.",
        b"<alice><follows>uid(other).",
        b"<alice><follows>*.",
        b"<alice><follows>0x01.",
        b"_:alice <follows><bob>.",
        b"<alice><follows><bob> (since=2020).",
        b"<alice><status>\"value\"^^<xs:string>.",
        b"<alice><status>\"value\"@en.",
        b"<alice><status@en>\"value\".",
        b"<alice><status>\"\\q\".",
        b"prefix <alice><follows><bob>.",
    ] {
        let fixture = Fixture::new("unsupported.nq", source);
        let input = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "nquads-dgraph",
            Vec::new(),
        );
        let output = execute_parse(&provider, &input, MEMORY, &Cancellation::new()).unwrap();
        assert!(
            !output
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str().ends_with(".expression")),
            "{source:?}"
        );
    }
}

#[test]
fn dgraph_and_standard_nquads_keep_distinct_grammar_and_reuse_identities() {
    let fixture = Fixture::new("identity.nq", b"<s:><p:><o:>.\n");
    let limits = limits(MAX_SOURCE_BYTES, 4096, 128);
    let provider = provider(MAX_SOURCE_BYTES, 4096, 128, 2 * 1024 * 1024);
    let settings = ParserSettings::new(1024).unwrap();
    let cancellation = Cancellation::new();
    let input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "nquads-dgraph",
        Vec::new(),
    );
    let native = provider
        .execute_with_previous(&input, None, &[], settings, MEMORY, &cancellation)
        .unwrap();
    let standard_input = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "nquads",
        Vec::new(),
    );
    let standard = provider
        .execute_with_previous(
            &standard_input,
            native.previous(),
            &[],
            settings,
            MEMORY,
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        standard.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::Language)
    );
    assert_eq!(standard.reuse_key().native_family(), GrammarFamily::NQuads);
    assert!(
        standard
            .output()
            .facts()
            .iter()
            .all(|fact| !fact.syntax_kind().as_str().starts_with("nquads-dgraph."))
    );
    let relative = Fixture::new("relative.nq", b"<alice><follows><bob>.\n");
    let input = request(
        &relative.snapshot,
        &relative.source,
        &limits,
        "nquads",
        Vec::new(),
    );
    let output = execute_parse(&provider, &input, MEMORY, &cancellation).unwrap();
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        !output
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "nquads.statement.expression")
    );
}
