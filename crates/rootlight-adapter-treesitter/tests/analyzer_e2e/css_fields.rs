//! CSS declaration tests retain property names, rule ownership and source witnesses.
//! Ordinary and custom declarations share the CSS property kind and identity admission.

use super::*;

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CSS_CASE, source.as_bytes());
    let limits = limits();
    let result = analyze(
        &analyzer(&provider, CSS_CASE),
        &request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    result
}

fn entity_at<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    source: &str,
    name: &str,
    kind: EntityKind,
    start: usize,
) -> &'a rootlight_ir::EntityRecord {
    let definitions = document
        .occurrences
        .iter()
        .filter(|row| {
            row.role == OccurrenceRole::Definition
                && row.source.span().start_byte() == u64::try_from(start).unwrap()
                && text(source, &row.source) == name
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "exact {name} definition at {start}");
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("a written definition must target its own entity");
    };
    let entity = document
        .entities
        .iter()
        .find(|row| row.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    assert_eq!(
        definitions[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
    entity
}

#[test]
fn css_ordinary_properties_retain_exact_rule_ownership() {
    let source = "html {\n  line-height: 1.15; /* 1 */\n}\nbody {\n  margin: 0;\n}\n";
    let result = output(source);
    let document = result.document();
    for (owner, name, declaration) in [
        ("html", "line-height", "line-height: 1.15;"),
        ("body", "margin", "margin: 0;"),
    ] {
        let parent = entity_at(
            document,
            source,
            owner,
            EntityKind::StyleRule,
            source.find(owner).unwrap(),
        );
        let property = entity_at(
            document,
            source,
            name,
            EntityKind::Property,
            source.find(name).unwrap(),
        );
        assert_eq!(
            text(source, property.evidence.source.as_ref().unwrap()),
            declaration
        );
        assert_eq!(
            property.container,
            Some(rootlight_ir::ContainerRef::Entity(parent.id))
        );
        assert!(
            document.relations.iter().any(|row| {
                row.predicate == RelationPredicate::Contains
                    && row.subject == RelationEndpoint::Entity(parent.id)
                    && row.object == RelationEndpoint::Entity(property.id)
                    && row.evidence.source.as_ref().is_some_and(|reference| {
                        reference.content_hash() == content_hash(source.as_bytes())
                            && text(source, reference).contains(name)
                    })
            }),
            "{owner} contains the source-backed {name} property"
        );
    }
}

#[test]
fn css_properties_keep_contexts_duplicates_and_value_boundaries() {
    let source = concat!(
        ".card, .tile { color: red; color: blue; --Accent: green; content: 'fake: value'; width: calc(100% - var(--Accent)); }\n",
        "@media print { .card, .tile { color: black; } }\n",
        "@font-face { font-family: 'First'; src: url(first.woff); }\n",
        "@font-face { font-family: 'Second'; src: url(second.woff); }\n",
        "/* phantom: value; .fake { hidden: red; } */\n",
    );
    let result = output(source);
    let document = result.document();
    let colors = source
        .match_indices("color:")
        .map(|(start, _)| entity_at(document, source, "color", EntityKind::Property, start))
        .collect::<Vec<_>>();
    assert_eq!(colors.len(), 3);
    assert_eq!(colors[0].id, colors[1].id);
    assert_ne!(colors[0].id, colors[2].id);
    for (position, owner_start) in [(0, 0), (2, source.find("@media print { ").unwrap() + 15)] {
        let owner = entity_at(
            document,
            source,
            ".card, .tile",
            EntityKind::StyleRule,
            owner_start,
        );
        assert_eq!(
            colors[position].container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
        assert!(
            document
                .relations
                .iter()
                .any(|row| row.predicate == RelationPredicate::Contains
                    && row.subject == RelationEndpoint::Entity(owner.id)
                    && row.object == RelationEndpoint::Entity(colors[position].id))
        );
    }
    entity_at(
        document,
        source,
        "--Accent",
        EntityKind::Property,
        source.find("--Accent").unwrap(),
    );
    let descriptors = source
        .match_indices("font-family:")
        .map(|(start, _)| entity_at(document, source, "font-family", EntityKind::Property, start))
        .collect::<Vec<_>>();
    assert_eq!(descriptors.len(), 2);
    assert_eq!(
        descriptors[0].id, descriptors[1].id,
        "identical at-rule headers retain every declaration occurrence"
    );
    let file = document
        .entities
        .iter()
        .find(|row| row.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(
        descriptors[0].container,
        Some(rootlight_ir::ContainerRef::Entity(file.id))
    );
    assert!(document.entities.iter().all(|row| !matches!(
        row.canonical_name.as_str(),
        "fake" | ".fake" | "value" | "phantom" | "hidden" | "calc" | "var" | "url" | "@font-face"
    )));
    assert!(
        !document
            .relations
            .iter()
            .any(|row| row.predicate == RelationPredicate::Calls)
    );
    let changed = format!(
        ".unrelated {{ height: 1px; }}\n{}",
        source
            .replace("blue", "cyan")
            .replace("first.woff", "other.woff")
    );
    let changed_output = output(&changed);
    let before = document
        .entities
        .iter()
        .map(|row| row.id)
        .collect::<BTreeSet<_>>();
    let after = changed_output
        .document()
        .entities
        .iter()
        .map(|row| row.id)
        .collect::<BTreeSet<_>>();
    assert!(
        before.is_subset(&after),
        "value edits and unrelated siblings preserve identities"
    );
}

#[test]
fn css_properties_survive_optional_fact_capacity_and_reject_identity_overflow() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let source = ".card { color: red; --Accent: blue; }\n/* optional documentation */\n/* another optional comment */\n";
    let fixture = Fixture::new(CSS_CASE, source.as_bytes());
    let full_limits = limits();
    let full_request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &full_limits);
    let artifact = analyzer
        .prepare_artifact(&full_request, &deadline())
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(&full_request.to_parse_request(), &deadline())
            .unwrap()
    );
    assert!(artifact.syntax_fact_count() > required);
    let bounded_limits = limits_with_syntax_records(required);
    let bounded_request = request(
        &fixture.snapshot,
        &fixture.source,
        CSS_CASE,
        &bounded_limits,
    );
    let bounded = analyze(&analyzer, &bounded_request, &ExtensionSupport::default());
    entity_at(
        bounded.document(),
        source,
        "color",
        EntityKind::Property,
        source.find("color").unwrap(),
    );
    entity_at(
        bounded.document(),
        source,
        "--Accent",
        EntityKind::Property,
        source.find("--Accent").unwrap(),
    );
    assert!(
        bounded
            .document()
            .diagnostics
            .iter()
            .any(|row| row.code == "syntax-extraction-limit")
    );
    let insufficient = limits_with_syntax_records(required - 1);
    assert!(
        analyzer
            .prepare_artifact(
                &request(&fixture.snapshot, &fixture.source, CSS_CASE, &insufficient),
                &deadline()
            )
            .is_err(),
        "required declarations must not be downgraded to optional facts"
    );
}

#[test]
fn css_escaped_properties_preserve_raw_names_and_property_kind() {
    let names = [r"\2d-foo", r"-\2d foo", r"\00002d\00002dfoo", r"\-\-foo"];
    let source = format!(
        ":root {{ {} \\63 olor: red; \\2e-foo: blue; }}\n",
        names.map(|name| format!("{name}: red;")).join(" ")
    );
    let result = output(&source);
    for name in names {
        entity_at(
            result.document(),
            &source,
            name,
            EntityKind::Property,
            source.find(name).unwrap(),
        );
    }
    for name in [r"\63 olor", r"\2e-foo"] {
        entity_at(
            result.document(),
            &source,
            name,
            EntityKind::Property,
            source.find(name).unwrap(),
        );
    }
}
