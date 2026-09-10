//! Nix source bindings through the public analyzer and validated durable IR.
//! Dynamic evaluation must not masquerade as a source-backed definition or relation.

use super::*;

pub(super) const NIX: LanguageCase = LanguageCase {
    name: "nix",
    path: "src/module.nix",
    frontend: "tree-sitter-nix-0.3.0",
    source: include_str!("../../../../tests/fixtures/nix/expressions.nix"),
    generated: false,
    body_before: "first + 1",
    body_after: "first + 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(NIX, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, NIX),
        &request(&fixture.snapshot, &fixture.source, NIX, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn nix_native_retains_authored_bindings_and_parameters_without_string_phantoms() {
    let result = output(NIX.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("choose", EntityKind::Function),
        ("nested", EntityKind::Variable),
        ("first", EntityKind::Variable),
        ("second", EntityKind::Variable),
        ("package.name", EntityKind::Variable),
        ("package.run", EntityKind::Function),
        ("\"quoted.key\"", EntityKind::Variable),
        ("path", EntityKind::Variable),
        ("script", EntityKind::Variable),
        ("lib", EntityKind::Parameter),
        ("system", EntityKind::Parameter),
        ("args", EntityKind::Parameter),
        ("value", EntityKind::Parameter),
        ("name", EntityKind::Parameter),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind),
            "missing {name}: {:?}",
            document.entities
        );
    }
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| ["phantom", "literal", "dynamic"]
                .contains(&entity.canonical_name.as_str()))
    );
    for entity in document
        .entities
        .iter()
        .filter(|entity| !entity.flags.contains(&EntityFlag::Synthetic))
    {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert!(!definitions.is_empty(), "missing definition: {entity:?}");
        for definition in definitions {
            let span = definition.source.span();
            assert_eq!(
                NIX.source.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(entity.canonical_name.as_str())
            );
        }
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "nix-computed-attribute-name-unavailable")
    );
}

#[test]
fn nix_empty_inheritance_never_invents_bindings() {
    for source in [
        "{ inherit; }",
        "{ inherit (builtins); }",
        "let inherit; in 1",
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty());
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.flags.contains(&EntityFlag::Synthetic))
        );
    }
}

#[test]
fn nix_structural_artifact_rebinds_to_the_same_ir_as_a_clean_generation() {
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(NIX, NIX.source.as_bytes());
    let analyzer = analyzer(&provider, NIX);
    let initial = request(&fixture.snapshot, &fixture.source, NIX, &budget);
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(artifact.required_syntax_fact_count(&deadline()).unwrap() > 0);
    let successor = fixture.next_generation();
    let request = request(&successor.snapshot, &successor.source, NIX, &budget);
    let reused = analyzer
        .analyze_from_artifact(
            &request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&analyzer, &request, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
}

#[test]
fn nix_source_owners_survive_body_and_trivia_edits_with_distinct_sibling_bindings() {
    let source =
        "let first = { shared = x: x; }; second = { shared = x: x + 1; }; in [ first second ]";
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| {
                (
                    entity.id,
                    (entity.kind, entity.canonical_name.clone(), entity.container),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let initial = output(source);
    assert_eq!(
        initial.document().entities.len(),
        7,
        "{:?}",
        initial.document().entities
    );
    assert_eq!(identities(&initial).len(), 7);
    for changed in [
        source.to_owned(),
        format!("# shifted λ😀\r\n{source}"),
        source.replace("x + 1", "x + 2"),
    ] {
        assert_eq!(identities(&output(&changed)), identities(&initial));
    }
}

#[test]
fn nix_unresolved_expressions_do_not_invent_lexical_or_attribute_relations() {
    let source = "let outer = 1; inherit (builtins) map; in with environment; { outer = 2; plain = outer; recursive = rec { value = value; }; \"quoted.key\" = outer; quoted.key = outer; selected = object.member; applied = map (x: x) []; }";
    let result = output(source);
    let document = result.document();
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "\"quoted.key\"")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "quoted.key")
    );
    for occurrence in document.occurrences.iter().filter(|occurrence| {
        matches!(
            occurrence.role,
            OccurrenceRole::Reference | OccurrenceRole::CallSite
        )
    }) {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert!(
        document.skipped_regions.iter().any(
            |gap| gap.detail == "nix-lexical-import-and-runtime-binding-resolution-unavailable"
        )
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn nix_static_quoted_keys_and_inherited_names_retain_exact_written_definitions() {
    let source = "{ inherit \"two words\"; inherit (builtins) map foldl'; \"\" = 1; a /* path trivia */ . b = 2; ${key} = value: value; }";
    let result = output(source);
    let computed = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "<computed-key>")
        .unwrap();
    assert_eq!(computed.kind, EntityKind::Function);
    assert!(computed.flags.contains(&EntityFlag::Synthetic));
    for name in [
        "\"two words\"",
        "map",
        "foldl'",
        "\"\"",
        "a /* path trivia */ . b",
        "value",
    ] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}: {:?}",
            result.document().entities
        );
    }
}
