//! Native collection-key equality, source and graph-sharing contracts.
//! Fixtures compare semantic identities across real source generations rather
//! than relying on raw-text or parser-local-ID equality.

use super::*;
use rootlight_ir::NormalizedIrDocument;

fn collection(document: &NormalizedIrDocument) -> &rootlight_ir::EntityRecord {
    document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Property
                && (entity.canonical_name.starts_with("seq:")
                    || entity.canonical_name.starts_with("map:"))
        })
        .expect("collection property")
}

#[test]
fn collection_key_identity_uses_typed_content_and_representation_order() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    for (before, after, equal) in [
        ("[one, 0xB]", "['one', 11]", true),
        ("[one, two]", "[two, one]", false),
        ("[11]", "['11']", false),
        ("{a: 1, b: 2}", "{b: 2, a: 1}", true),
        ("{a: 1, b: 2}", "{b: 3, a: 1}", false),
        ("[]", "{}", false),
        ("[]", "!!seq []", true),
        ("{}", "! {}", true),
        ("[one, two]", "\n  - one\n  - two", true),
        ("[null, one]", "\n  -\n  - one", true),
        ("{a: 1, b: 2}", "\n  b: 2\n  a: 1", true),
        ("[a: 1, b: 2]", "[{a: 1}, {b: 2}]", true),
        ("{a, b:}", "{a: null, b: null}", true),
        ("{? : value}", "{null: value}", true),
        ("[{a: [0xB, null]}]", "[{a: [11, ~]}]", true),
        ("{[one, two]}", "{? ['one', two]: null}", true),
        (
            "{&key [one], copy: *key}",
            "{? [one]: null, copy: [one]}",
            true,
        ),
        ("[{a: \"first\\n\"}]", "\n  - a: |\n      first", true),
        ("[!!str true]", "['true']", true),
        ("[a: [null]]", "[{a: [~]}]", true),
        ("[-\u{a0}]", "['-\u{a0}']", true),
        ("[-\u{85}]", "['-\u{85}']", true),
    ] {
        let fixture = Fixture::new(YAML, format!("? {before}\n: first\n").as_bytes());
        let changed = fixture.rewrite(format!("? {after}\n: second\n").as_bytes());
        let first = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, YAML, &limits()),
            &ExtensionSupport::default(),
        );
        let second = analyze(
            &analyzer,
            &request(&changed.snapshot, &changed.source, YAML, &limits()),
            &ExtensionSupport::default(),
        );
        for result in [&first, &second] {
            assert!(
                result.document().diagnostics.is_empty(),
                "{before:?} -> {after:?}: {:?}",
                result.document().diagnostics
            );
            assert!(
                result.document().skipped_regions.is_empty(),
                "{before:?} -> {after:?}: {:?}",
                result.document().skipped_regions
            );
        }
        assert_eq!(
            collection(first.document()).id == collection(second.document()).id,
            equal,
            "{before:?} -> {after:?}"
        );
    }
}

#[test]
fn collection_key_duplicates_remain_distinct_source_occurrences() {
    let source = "? {a: 1, b: 2}\n: first\n? {b: 2, a: 1}\n: second\n";
    let result = output(source);
    let properties: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name.starts_with("map:"))
        .collect();
    assert_eq!(properties.len(), 2);
    assert_eq!(properties[0].canonical_name, properties[1].canonical_name);
    assert_ne!(properties[0].id, properties[1].id);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "yaml-duplicate-mapping-key")
    );
    for property in properties {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: property.id
                        }))
        );
    }
}

#[test]
fn collection_key_artifact_replay_preserves_semantics_and_rebinds_generation() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let fixture = Fixture::new(YAML, b"base: &key [one, {a: 1}]\n? *key\n: value\n");
    let initial_limits = limits();
    let memory = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, YAML, &initial_limits),
            ExtensionSupport::default(),
            memory,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.next_generation();
    let request = request(&changed.snapshot, &changed.source, YAML, &initial_limits);
    let fresh = analyze(&analyzer, &request, &ExtensionSupport::default());
    let replay = analyzer
        .analyze_from_artifact(
            &request,
            &artifact,
            ExtensionSupport::default(),
            memory,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    assert_eq!(symbol_ids(first.document()), symbol_ids(replay.document()));
    assert!(replay.document().skipped_regions.is_empty());
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.source.generation() == changed.source.generation())
    );
}

#[test]
fn collection_alias_keys_share_identity_without_expanding_shared_graphs() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let mut declarations = "seed: &n0 [0]\n".to_owned();
    for level in 1..48 {
        let previous = level - 1;
        declarations.push_str(&format!(
            "node{level}: &n{level} [*n{previous}, *n{previous}]\n"
        ));
    }
    let fixture = Fixture::new(YAML, format!("{declarations}? *n47\n: value\n").as_bytes());
    let changed = fixture.rewrite(format!("{declarations}? [*n46, *n46]\n: edited\n").as_bytes());
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, YAML, &limits()),
        &ExtensionSupport::default(),
    );
    let second = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, YAML, &limits()),
        &ExtensionSupport::default(),
    );
    for result in [&first, &second] {
        assert!(
            result.document().diagnostics.is_empty(),
            "{:?}",
            result.document().diagnostics
        );
        assert!(
            result.document().skipped_regions.is_empty(),
            "{:?}",
            result.document().skipped_regions
        );
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Property)
                .count(),
            49
        );
    }
    assert_eq!(
        collection(first.document()).id,
        collection(second.document()).id
    );
    let definition = first
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target
                    == (OccurrenceTarget::Resolved {
                        symbol: collection(first.document()).id,
                    })
        })
        .unwrap();
    let span = definition.source.span();
    let full = format!("{declarations}? *n47\n: value\n");
    assert_eq!(
        &full[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "*n47"
    );
}

#[test]
fn collection_key_ambiguity_and_unknown_tags_stay_scoped() {
    for source in [
        "? {a: 1, a: 2}\n: value\nsafe: value\n",
        "base: &x [*x]\n? *x\n: value\nsafe: value\n",
        "? [*missing]\n: value\nsafe: value\n",
        "? !!map [one]\n: value\nsafe: value\n",
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty(), "{source:?}");
        assert!(!result.document().skipped_regions.is_empty(), "{source:?}");
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "str:\"safe\"")
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name.starts_with("seq:")
                    || entity.canonical_name.starts_with("map:"))
        );
    }
    for key in ["!custom [one]", "[!custom one]", "{a: !custom [one]}"] {
        let result = output(&format!("? {key}\n: value\n"));
        assert!(result.document().diagnostics.is_empty());
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "yaml-key-tag-semantics-unknown")
        );
        collection(result.document());
    }
}
