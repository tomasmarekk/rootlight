//! Real YAML parsing contracts through the public analyzer and normalized IR.
//! Document context and exact source evidence are checked together, not inferred
//! from standalone scalar decoding or successful grammar construction.

#[path = "yaml_collections.rs"]
mod collections;

#[test]
fn yaml_native_wide_integer_spellings_share_identity_but_keep_exact_sources() {
    // 2^1024 - 1, independently constructed as a decimal golden.
    let decimal = concat!(
        "179769313486231590772930519078902473361797697894230657273430081157732675805500",
        "963132708477322407536021120113879871393357658789768814416622492847430639474",
        "124377767893424865485276302219601246094119453082952085005768838150682342462",
        "881473913110540827237163350510684586298239947245938479716304835356329624224",
        "137215"
    );
    let hex = format!("0x{}", "f".repeat(256));
    let octal = format!("0o1{}", "7".repeat(341));
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let mut fixture = Fixture::new(YAML, b"0: value\n");
    let mut previous = None;
    for key in [
        hex.clone(),
        octal,
        decimal.to_owned(),
        format!("!!int '{hex}'"),
    ] {
        fixture = fixture.rewrite(format!("{key}: value\n").as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, YAML, &limits()),
            &ExtensionSupport::default(),
        );
        let document = result.document();
        assert!(
            document.diagnostics.is_empty(),
            "{:?}",
            document.diagnostics
        );
        assert!(
            document.skipped_regions.is_empty(),
            "{:?}",
            document.skipped_regions
        );
        let properties: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .collect();
        assert_eq!(properties.len(), 1);
        let entity = properties[0];
        assert_eq!(entity.canonical_name, format!("int:{decimal}"));
        if let Some(previous) = previous {
            assert_eq!(entity.id, previous);
        }
        previous = Some(entity.id);
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .unwrap();
        assert_eq!(definition.source.generation(), fixture.source.generation());
        assert_eq!(definition.source.span().start_byte(), 0);
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(key.len()).unwrap()
        );
    }
}

#[test]
fn yaml_native_addresses_preserve_layout_and_value_edits_but_count_every_slot() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    for (before, after, stable) in [
        ("key: one\n", "'key': two\n", true),
        ("key: one\n", "# note\nkey: &name two\n", true),
        ("- first\n- key: one\n", "- false\n- key: two\n", true),
        ("[null, {key: one}]", "[false, {key: two}]", true),
        ("[null, {key: one}]", "[null, # note\n {key: two}]", true),
        ("[null, {key: one}]", "[null, null, {key: one}]", false),
        ("- first\n- key: one\n", "- first\n-\n- key: one\n", false),
        ("---\nkey: one\n", "---\n{}\n---\nkey: one\n", false),
        ("[first: one, key: two]", "[other: one, key: three]", true),
    ] {
        let fixture = Fixture::new(YAML, before.as_bytes());
        let changed = fixture.rewrite(after.as_bytes());
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
            symbol_id_named(first.document(), r#"str:"key""#)
                == symbol_id_named(second.document(), r#"str:"key""#),
            stable,
            "{before:?} -> {after:?}"
        );
    }
}

#[test]
fn yaml_native_artifacts_retain_document_and_slot_context_at_the_required_boundary() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let fixture = Fixture::new(YAML, b"%TAG !e! tag:yaml.org,2002:\n---\n&base [null, {!e!str true: value}, *base, {? : last}]\n---\nkey: next\n");
    let initial_limits = limits();
    let initial = request(&fixture.snapshot, &fixture.source, YAML, &initial_limits);
    let (full, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("complete YAML artifact");
    assert!(full.document().diagnostics.is_empty());
    assert!(
        full.document().skipped_regions.is_empty(),
        "{:?}",
        full.document().skipped_regions
    );
    let required = artifact
        .required_syntax_fact_count(&deadline())
        .expect("required facts");
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .expect("native preflight")
    );
    let changed = fixture.next_generation();
    let bounded_limits = limits_with_syntax_records(required);
    let bounded = request(&changed.snapshot, &changed.source, YAML, &bounded_limits);
    let (fresh, bounded_artifact) = analyzer
        .analyze_and_capture(
            &bounded,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("bounded identity capture");
    assert!(!artifact.is_compatible_with_limits(&bounded_limits));
    for (candidate, replay_request, expected) in [
        (&bounded_artifact, &bounded, &fresh),
        (
            &artifact,
            &request(&changed.snapshot, &changed.source, YAML, &initial_limits),
            &analyze(
                &analyzer,
                &request(&changed.snapshot, &changed.source, YAML, &initial_limits),
                &ExtensionSupport::default(),
            ),
        ),
    ] {
        let reused = analyzer
            .analyze_from_artifact(
                replay_request,
                candidate,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .expect("bounded identity replay");
        assert_eq!(reused.document(), expected.document());
        assert_eq!(reused.report(), expected.report());
        assert_eq!(symbol_ids(full.document()), symbol_ids(reused.document()));
    }
    let too_small = limits_with_syntax_records(required - 1);
    assert!(
        analyzer
            .analyze_and_capture(
                &request(&changed.snapshot, &changed.source, YAML, &too_small),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err(),
        "never drop a document, sequence slot or binding to meet a budget"
    );
}

#[test]
fn yaml_native_document_schema_and_unknown_key_tags_leave_scoped_gaps() {
    for (source, expected_gap) in [
        (
            "%YAML 1.1\n---\nyes: value\n",
            "yaml-document-schema-uncertain",
        ),
        ("!custom key: value\n", "yaml-key-tag-semantics-unknown"),
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty(), "{source:?}");
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == expected_gap),
            "{source:?}: {:?}",
            result.document().skipped_regions
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Property)
        );
    }
}

use super::*;

const YAML: LanguageCase = LanguageCase {
    name: "yaml",
    path: "src/settings.yaml",
    frontend: "tree-sitter-yaml-0.7.2",
    source: "name: value\n",
    generated: false,
    body_before: "value",
    body_after: "changed",
};

#[test]
fn yaml_native_collection_keys_keep_all_nested_properties_and_sources() {
    for key in ["[one, {a: 1, b: [false, null]}]", "{a, b:}"] {
        let source = format!("? {key}\n: value\n");
        let result = output(&source);
        let document = result.document();
        assert!(document.diagnostics.is_empty());
        assert!(
            document.skipped_regions.is_empty(),
            "{:?}",
            document.skipped_regions
        );
        assert_eq!(
            document
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Property)
                .count(),
            3
        );
        let root = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name.starts_with("seq:")
                    || entity.canonical_name.starts_with("map:")
            })
            .unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: root.id })
            })
            .unwrap();
        let span = definition.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            key
        );
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
        let encoded = serde_json::to_vec(document).unwrap();
        assert_eq!(
            decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default())
                .unwrap(),
            IrDocument::NormalizedV1_1(document.clone())
        );
    }
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let fixture = Fixture::new(YAML, source.as_bytes());
    analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, YAML, &limits()),
        &ExtensionSupport::default(),
    )
}

#[test]
fn yaml_native_scalar_keys_use_document_tags_and_complete_block_values() {
    for (source, mut expected) in [
        ("name: value\n", vec!["str:\"name\""]),
        ("!!str true: value\n", vec!["str:\"true\""]),
        ("!!int '0xB': value\n", vec!["int:11"]),
        (
            "%TAG !e! tag:yaml.org,2002:\n---\n!e!str true: value\n",
            vec!["str:\"true\""],
        ),
        (
            "? |+\n  first\n\n: value\n",
            vec!["str:\"first\\u000a\\u000a\""],
        ),
        (": value\n", vec!["null:null"]),
        ("{? : value}", vec!["null:null"]),
        ("{one, two}", vec!["str:\"one\"", "str:\"two\""]),
        (
            "{&key name, copy: *key}",
            vec!["str:\"name\"", "str:\"copy\""],
        ),
        (
            "[first: one, null, second: two]",
            vec!["str:\"first\"", "str:\"second\""],
        ),
        ("[? : value]", vec!["null:null"]),
    ] {
        let result = output(source);
        let document = result.document();
        assert!(
            document.diagnostics.is_empty(),
            "{source:?}: {:?}",
            document.diagnostics
        );
        assert!(
            document.skipped_regions.is_empty(),
            "{source:?}: {:?}",
            document.skipped_regions
        );
        let mut names: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .map(|entity| entity.canonical_name.as_str())
            .collect();
        names.sort_unstable();
        expected.sort_unstable();
        assert_eq!(names, expected, "{source:?}");
        let encoded = serde_json::to_vec(document).unwrap();
        assert_eq!(
            decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default())
                .unwrap(),
            IrDocument::NormalizedV1_1(document.clone())
        );
    }
}

#[test]
fn yaml_native_anchor_keys_and_cycles_keep_separate_definition_owners() {
    let source = "&root {&key name: *root, copy: *key}";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    assert!(
        document.skipped_regions.is_empty(),
        "{:?}",
        document.skipped_regions
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .count(),
        2
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .count(),
        2
    );
    let aliases: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "yaml.alias.reference")
        .collect();
    assert_eq!(aliases.len(), 2);
    assert!(
        aliases
            .iter()
            .all(|alias| matches!(alias.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn yaml_native_scalar_alias_keys_construct_the_anchored_value_and_keep_alias_sources() {
    for (source, expected) in [
        ("base: &key name\n*key : value\n", "str:\"name\""),
        ("base: &key !!int '0xB'\n*key : value\n", "int:11"),
        ("base: &key\n*key : value\n", "null:null"),
        ("base: &key !!str\n*key : value\n", "str:\"\""),
        (
            "base: &key |+\n  first\n\n*key : value\n",
            "str:\"first\\u000a\\u000a\"",
        ),
        (
            "base:\n  - &key >-\n    first\n    second\n*key : value\n",
            "str:\"first second\"",
        ),
        (
            "%TAG !e! tag:yaml.org,2002:\n---\nbase: &key !e!str true\n*key : value\n",
            "str:\"true\"",
        ),
        (
            "first: &key old\nsecond: &key new\n*key : value\n",
            "str:\"new\"",
        ),
        ("{base: &key name, *key : value}", "str:\"name\""),
        ("{base: &key name, *key}", "str:\"name\""),
    ] {
        let result = output(source);
        let document = result.document();
        assert!(
            document.diagnostics.is_empty(),
            "{source:?}: {:?}",
            document.diagnostics
        );
        assert!(
            document.skipped_regions.is_empty(),
            "{source:?}: {:?}",
            document.skipped_regions
        );
        let key = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Property && entity.canonical_name == expected)
            .unwrap_or_else(|| panic!("missing {expected:?} in {source:?}"));
        let definition = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: key.id })
            })
            .unwrap();
        let start = usize::try_from(definition.source.span().start_byte()).unwrap();
        let end = usize::try_from(definition.source.span().end_byte()).unwrap();
        assert_eq!(&source[start..end], "*key");
        assert!(
            document
                .occurrences
                .iter()
                .any(
                    |occurrence| occurrence.syntax_kind == "yaml.alias.reference"
                        && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
                )
        );
    }
}

#[test]
fn yaml_native_scalar_alias_key_identity_matches_literal_and_changes_with_target() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let mut fixture = Fixture::new(YAML, b"base: &key name\nname: one\n");
    let mut ids = Vec::new();
    for source in [
        "base: &key name\nname: one\n",
        "base: &key name\n*key : two\n",
        "base: &key 'name'\n*key : three\n",
        "base: &key other\n*key : three\n",
    ] {
        fixture = fixture.rewrite(source.as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, YAML, &limits()),
            &ExtensionSupport::default(),
        );
        assert!(result.document().diagnostics.is_empty());
        assert!(
            result.document().skipped_regions.is_empty(),
            "{:?}",
            result.document().skipped_regions
        );
        let key = result
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Property && entity.canonical_name != "str:\"base\""
            })
            .unwrap();
        ids.push(key.id);
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|occurrence| occurrence.source.generation() == fixture.source.generation())
        );
    }
    assert_eq!(ids[0], ids[1]);
    assert_eq!(ids[1], ids[2]);
    assert_ne!(ids[2], ids[3]);
}

#[test]
fn yaml_native_unavailable_or_cyclic_alias_keys_leave_local_gaps() {
    for source in [
        "base: &key name\n---\n*key : value\nsafe: value\n",
        "*key : value\nbase: &key name\nsafe: value\n",
        "base: &key {*key : value}\nsafe: value\n",
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
                .any(|entity| entity.kind == EntityKind::Property
                    && entity.canonical_name == "str:\"name\"")
        );
    }
}

#[test]
fn yaml_native_alias_key_duplicates_and_unknown_tags_remain_source_scoped() {
    for (source, detail) in [
        (
            "&key name: first\n*key : second\n",
            "yaml-duplicate-mapping-key",
        ),
        (
            "base: &key !custom name\n*key : value\n",
            "yaml-key-tag-semantics-unknown",
        ),
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty());
        let gap = result
            .document()
            .skipped_regions
            .iter()
            .find(|gap| gap.detail == detail)
            .unwrap();
        let span = gap.source.span();
        let start = usize::try_from(span.start_byte()).unwrap();
        let end = usize::try_from(span.end_byte()).unwrap();
        assert!(source[start..end].starts_with("*key"));
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Property)
                .count(),
            2
        );
    }
}

#[test]
fn yaml_native_alias_key_artifacts_rebind_source_generation_without_reparsing() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, YAML);
    let fixture = Fixture::new(YAML, b"base: &key !!int '0xB'\n*key : value\n");
    let initial_limits = limits();
    let memory = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
    let (initial, artifact) = analyzer
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
    assert_eq!(
        symbol_ids(initial.document()),
        symbol_ids(replay.document())
    );
    assert!(replay.document().skipped_regions.is_empty());
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.source.generation() == changed.source.generation())
    );
}
