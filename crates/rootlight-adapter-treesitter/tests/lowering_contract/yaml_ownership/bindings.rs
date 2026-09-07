//! Serialization binding contracts with exact source evidence, not value expansion.

use super::*;
use rootlight_ir::{OccurrenceTarget, RelationEndpoint, RelationPredicate};

impl Fixture<'_> {
    fn binding_name(&mut self, parent: u64, token: &str, ordinal: usize) -> u64 {
        let span = span_in(self.text, &self.source, token, ordinal);
        let parent_fact = self
            .facts
            .iter()
            .find(|fact| fact.local_id() == parent)
            .unwrap();
        let local = u64::try_from(self.facts.len()).unwrap() + 1;
        self.facts.push(SyntaxFact::new(
            local,
            Some(parent),
            SyntaxFactKind::Occurrence,
            SourceSpan::new(span.file(), span.start_byte() + 1, span.end_byte()).unwrap(),
            parent_fact.depth() + 1,
            label(if token.starts_with('&') {
                "yaml.anchor.definition"
            } else {
                "yaml.alias.reference"
            }),
        ));
        local
    }

    fn anchor(&mut self, parent: u64, node: &str, token: &str, ordinal: usize) -> u64 {
        let declaration = self.add(parent, "yaml.anchor.declaration", node, ordinal);
        self.binding_name(declaration, token, ordinal);
        declaration
    }
}

fn anchors(document: &NormalizedIrDocument) -> Vec<&rootlight_ir::EntityRecord> {
    let mut result: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .collect();
    result.sort_by_key(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte());
    result
}

fn aliases(document: &NormalizedIrDocument) -> Vec<&rootlight_ir::OccurrenceRecord> {
    let mut result: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "yaml.alias.reference")
        .collect();
    result.sort_by_key(|occurrence| occurrence.source.span().start_byte());
    result
}

fn assert_targets(
    fixture: &Fixture<'_>,
    document: &NormalizedIrDocument,
    targets: &[Option<usize>],
) {
    let anchors = anchors(document);
    let aliases = aliases(document);
    assert_eq!(aliases.len(), targets.len());
    for (alias, target) in aliases.into_iter().zip(targets) {
        assert_eq!(alias.source.generation(), fixture.source.generation());
        assert_eq!(alias.source.content_hash(), fixture.source.content_hash());
        let span = alias.source.span();
        assert_eq!(
            fixture.text.as_bytes()[usize::try_from(span.start_byte()).unwrap() - 1],
            b'*'
        );
        if let Some(target) = target {
            let symbol = anchors[*target].id;
            assert_eq!(alias.target, OccurrenceTarget::Resolved { symbol });
            let relations: Vec<_> = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(alias.id)
                        && relation.predicate == RelationPredicate::RefersTo
                })
                .collect();
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0].object, RelationEndpoint::Entity(symbol));
            assert_eq!(relations[0].evidence.source.as_ref(), Some(&alias.source));
        } else {
            assert!(matches!(alias.target, OccurrenceTarget::Unresolved { .. }));
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|region| region.detail == "yaml-alias-target-unavailable"
                        && region.source.span() == span)
            );
        }
    }
    for coverage in &document.coverage_records {
        assert_eq!(coverage.discovered, coverage.indexed + coverage.skipped);
    }
    assert_exact_sources(fixture, document);
}

#[test]
fn aliases_follow_serialization_order_and_do_not_cross_documents() {
    let text = "---\n[*x, &x first, *x, &x second, *x]\n---\n[*x, &x third, *x]\n";
    let mut fixture = Fixture::new(text);
    let first = fixture.add(
        2,
        "yaml.document.scope",
        "---\n[*x, &x first, *x, &x second, *x]",
        0,
    );
    let second = fixture.add(2, "yaml.document.scope", "---\n[*x, &x third, *x]", 0);
    fixture.anchor(first, "&x first", "&x", 0);
    // The node span is unique, while the raw name has a separate global ordinal.
    let repeated = fixture.add(first, "yaml.anchor.declaration", "&x second", 0);
    fixture.binding_name(repeated, "&x", 1);
    let third = fixture.add(second, "yaml.anchor.declaration", "&x third", 0);
    fixture.binding_name(third, "&x", 2);
    for ordinal in 0..5 {
        fixture.binding_name(if ordinal < 3 { first } else { second }, "*x", ordinal);
    }
    let result = fixture.analyze();
    assert_targets(&fixture, &result, &[None, Some(0), Some(1), None, Some(2)]);
    assert_eq!(
        anchors(&result)
            .iter()
            .map(|anchor| anchor.qualified_name.as_str())
            .collect::<Vec<_>>(),
        [
            "document[0].&x#occurrence[0]",
            "document[0].&x#occurrence[1]",
            "document[1].&x#occurrence[0]"
        ]
    );
    fixture.facts.reverse();
    assert_eq!(fixture.analyze(), result);
}

#[test]
fn self_cycles_are_finite_source_bound_edges_and_names_are_literal() {
    let text = "&true [*true, &null [*true, *null]]";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    let outer = fixture.anchor(document, text, "&true", 0);
    let inner = fixture.anchor(outer, "&null [*true, *null]", "&null", 0);
    fixture.binding_name(outer, "*true", 0);
    fixture.binding_name(inner, "*true", 1);
    fixture.binding_name(inner, "*null", 0);
    let result = fixture.analyze();
    assert_targets(&fixture, &result, &[Some(0), Some(0), Some(1)]);
    assert_eq!(
        anchors(&result)
            .iter()
            .map(|anchor| anchor.canonical_name.as_str())
            .collect::<Vec<_>>(),
        ["true", "null"]
    );
    assert_eq!(
        anchors(&result)[0].evidence.source.as_ref().unwrap().span(),
        fixture.source.span()
    );
    assert!(
        result.skipped_regions.is_empty(),
        "{:?}",
        result.skipped_regions
    );
    assert_eq!(result.entities.len(), 3);
}

#[test]
fn anchor_annotations_do_not_change_data_identity_or_contains_ownership() {
    let analyze = |name: Option<&str>| {
        let text = name.map_or_else(
            || "{key: value}".to_owned(),
            |name| format!("&{name} {{key: value}}"),
        );
        let mut fixture = Fixture::new(&text);
        let document = fixture.add(2, "yaml.document.scope", &text, 0);
        let parent = name.map_or(document, |name| {
            fixture.anchor(document, &text, &format!("&{name}"), 0)
        });
        let mapping = fixture.add(parent, "yaml.mapping.scope", "{key: value}", 0);
        fixture.property(mapping, "key: value", "key", 0);
        fixture.analyze()
    };
    let plain = analyze(None);
    for name in ["first", "renamed", "null"] {
        let actual = analyze(Some(name));
        assert_eq!(properties(&actual)[0].id, properties(&plain)[0].id);
        assert_eq!(
            properties(&actual)[0].container,
            properties(&plain)[0].container
        );
        assert_eq!(
            properties(&actual)[0].qualified_name,
            properties(&plain)[0].qualified_name
        );
    }
}

#[test]
fn anchors_inside_unidentified_complex_keys_still_bind_in_the_document() {
    let text = "{? [&x value]: hidden, use: *x}";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    let complex = fixture.add(
        document,
        "yaml.property.declaration",
        "? [&x value]: hidden",
        0,
    );
    fixture.anchor(complex, "&x value", "&x", 0);
    let usage = fixture.property(document, "use: *x", "use", 0);
    fixture.binding_name(usage, "*x", 0);
    let result = fixture.analyze();
    assert_targets(&fixture, &result, &[Some(0)]);
    assert_eq!(properties(&result).len(), 1);
    assert_eq!(anchors(&result).len(), 1);
}

#[test]
fn ambiguous_later_anchor_shadows_an_earlier_valid_binding() {
    let text = "[&x first, &x second, *x]";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    fixture.anchor(document, "&x first", "&x", 0);
    let second = fixture.add(document, "yaml.anchor.declaration", "&x second", 0);
    fixture.binding_name(second, "&x", 1);
    fixture.add(second, "yaml.anchor.definition", "second", 0);
    fixture.binding_name(document, "*x", 0);
    // Conflicting definition capture lacks its required indicator: reject the
    // provider contract instead of silently recovering to the earlier binding.
    assert!(matches!(
        fixture.analyze_with(fixture.facts.clone(), IrLimits::default()),
        Err(AdapterError::ProviderFailed { .. })
    ));
    // Two valid, distinct definition tokens owned by one declaration are also
    // ambiguous and must shadow each spelling without inventing a target.
    let text = "[&x first, &x &y second, *x]";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    fixture.anchor(document, "&x first", "&x", 0);
    let second = fixture.add(document, "yaml.anchor.declaration", "&x &y second", 0);
    fixture.binding_name(second, "&x", 1);
    fixture.binding_name(second, "&y", 0);
    fixture.binding_name(document, "*x", 0);
    let result = fixture.analyze();
    assert_targets(&fixture, &result, &[None]);
    assert_eq!(anchors(&result).len(), 1);
}

#[test]
fn missing_document_scope_keeps_alias_unresolved() {
    let text = "[&x value, *x]";
    let mut fixture = Fixture::new(text);
    fixture.anchor(2, "&x value", "&x", 0);
    fixture.binding_name(2, "*x", 0);
    let result = fixture.analyze();
    assert_targets(&fixture, &result, &[None]);
    assert!(anchors(&result).is_empty());
}

#[test]
fn missing_anchor_name_blocks_stale_bindings_until_a_new_definition() {
    let text = "[&x first, &x missing, *x, &x third, *x]";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    fixture.anchor(document, "&x first", "&x", 0);
    fixture.add(document, "yaml.anchor.declaration", "&x missing", 0);
    let third = fixture.add(document, "yaml.anchor.declaration", "&x third", 0);
    fixture.binding_name(third, "&x", 2);
    fixture.binding_name(document, "*x", 0);
    fixture.binding_name(document, "*x", 1);
    let result = fixture.analyze();
    assert_targets(&fixture, &result, &[None, Some(1)]);
}

#[test]
fn cached_alias_edges_rebind_generation_and_incomplete_parses_do_not_resolve() {
    let text = "[&x first, *x]";
    let mut fixture = Fixture::new(text);
    let document = fixture.add(2, "yaml.document.scope", text, 0);
    fixture.anchor(document, "&x first", "&x", 0);
    fixture.binding_name(document, "*x", 0);
    let language = LanguageId::new("yaml").unwrap();
    let limits = limits(IrLimits::default());
    let request_for = |source| {
        AnalysisRequest::new(
            GenerationBoundSnapshot::new(&fixture.snapshot, source).unwrap(),
            language.clone(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"build-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false)
    };
    let memory = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
    let analyzer = custom_analyzer(&fixture.snapshot, language.clone(), fixture.facts.clone());
    let (initial, artifact) = analyzer
        .analyze_and_capture(
            &request_for(&fixture.source),
            ExtensionSupport::default(),
            memory,
            &deadline(),
        )
        .unwrap();
    assert_targets(&fixture, initial.document(), &[Some(0)]);
    let successor = SourceRef::new(
        fixture.source.repository(),
        GenerationId::from_bytes([9; 20]),
        fixture.source.span(),
        fixture.source.content_hash(),
        None,
    );
    let request = request_for(&successor);
    let reused = analyzer
        .analyze_from_artifact(
            &request,
            &artifact,
            ExtensionSupport::default(),
            memory,
            &deadline(),
        )
        .unwrap();
    let fresh = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        memory,
        &deadline(),
    )
    .unwrap();
    assert_eq!(reused.document(), fresh.document());
    assert_eq!(reused.report(), fresh.report());
    assert_eq!(
        anchors(reused.document())[0].id,
        anchors(initial.document())[0].id
    );
    assert_ne!(
        aliases(reused.document())[0].id,
        aliases(initial.document())[0].id
    );
    assert!(
        reused
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.source.generation() == successor.generation())
    );
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        fixture.facts.len() - 1
    );
    for status in [CoverageStatus::Bounded, CoverageStatus::Unknown] {
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            status,
            text.len(),
            text.len() - 1,
            1,
            Vec::new(),
        )
        .unwrap();
        let analyzer =
            custom_analyzer_with_coverage(language.clone(), fixture.facts.clone(), coverage);
        let partial = execute_analysis(
            &analyzer,
            &request_for(&fixture.source),
            ExtensionSupport::default(),
            memory,
            &deadline(),
        )
        .unwrap();
        assert_targets(&fixture, partial.document(), &[None]);
    }
}

#[test]
fn literal_names_and_binding_evidence_obey_existing_output_quotas() {
    for name in [r#"str:"literal""#, "🌍", "a\u{85}b", r"a\u0062"] {
        let text = format!("[&{name} value, *{name}]");
        let mut fixture = Fixture::new(&text);
        let document = fixture.add(2, "yaml.document.scope", &text, 0);
        fixture.anchor(document, &format!("&{name} value"), &format!("&{name}"), 0);
        fixture.binding_name(document, &format!("*{name}"), 0);
        let result = fixture.analyze();
        assert_targets(&fixture, &result, &[Some(0)]);
        assert_eq!(anchors(&result)[0].canonical_name, name);
        assert_eq!(anchors(&result)[0].display_name, name);
        let mut ir = IrLimits::default();
        ir.max_string_bytes = result
            .entities
            .iter()
            .map(|entity| entity.qualified_name.len())
            .max()
            .unwrap()
            .max(64);
        assert_eq!(
            fixture
                .analyze_with(fixture.facts.clone(), ir.clone())
                .unwrap()
                .document(),
            &result
        );
        ir.max_relations = 0;
        assert!(matches!(
            fixture.analyze_with(fixture.facts.clone(), ir),
            Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
        ));
    }
}

#[test]
fn duplicate_definition_captures_and_value_edits_preserve_anchor_identity() {
    let mut identity = None;
    for value in ["first", "different body"] {
        let text = format!("[&x {value}, *x]");
        let mut fixture = Fixture::new(&text);
        let document = fixture.add(2, "yaml.document.scope", &text, 0);
        let anchor = fixture.anchor(document, &format!("&x {value}"), "&x", 0);
        fixture.binding_name(document, "*x", 0);
        let expected = fixture.analyze();
        assert_targets(&fixture, &expected, &[Some(0)]);
        let symbol = anchors(&expected)[0].id;
        assert_eq!(*identity.get_or_insert(symbol), symbol);
        for _ in 0..256 {
            fixture.binding_name(anchor, "&x", 0);
        }
        assert_eq!(fixture.analyze(), expected);
    }
}
