//! Real YAML parsing contracts through the public analyzer and normalized IR.
//! Document context and exact source evidence are checked together, not inferred
//! from standalone scalar decoding or successful grammar construction.

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
