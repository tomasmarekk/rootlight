//! Written document ownership through the native block parser and validated IR.
//! Heading text remains authored source, not a renderer-specific fragment ID.

use super::*;

const MARKDOWN: LanguageCase = LanguageCase {
    name: "markdown",
    path: "docs/storage.md",
    frontend: "tree-sitter-md-0.5.3",
    source: "# Storage *rules*\r\n\r\nIntro.\r\n\r\n## Shared cache\r\n\r\n[Guide]: <guide.md> \"Help\"\r\n\r\nBody.\r\n\r\nOther section\r\n=============\r\nTail.\r\n",
    generated: false,
    body_before: "Body.",
    body_after: "Longer replacement body.",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, MARKDOWN),
        &request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn markdown_blocks_preserve_authored_names_and_kinds() {
    let result = output(MARKDOWN.source);
    let document = result.document();
    let names: BTreeSet<_> = document
        .entities
        .iter()
        .map(|entity| (entity.kind, entity.canonical_name.as_str()))
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([
            (EntityKind::Module, MARKDOWN.path),
            (EntityKind::DocumentSection, "Storage *rules*"),
            (EntityKind::DocumentSection, "Shared cache"),
            (EntityKind::DocumentSection, "Other section"),
            (EntityKind::LinkDefinition, "[Guide]"),
        ])
    );
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == "markdown")
    );
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_5);
    for entity in document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let text = &MARKDOWN.source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(text, entity.canonical_name);
        assert_eq!(
            definitions[0].syntactic_text_hash,
            content_hash(text.as_bytes())
        );
        assert_eq!(
            definitions[0].source.content_hash(),
            content_hash(MARKDOWN.source.as_bytes())
        );
    }
}

#[test]
fn markdown_section_ownership_follows_written_levels_including_setext() {
    let result = output(MARKDOWN.source);
    let document = result.document();
    for (name, owner, exact_text) in [
        (
            "Storage *rules*",
            MARKDOWN.path,
            MARKDOWN.source.split("Other section").next().unwrap(),
        ),
        (
            "Shared cache",
            "Storage *rules*",
            "## Shared cache\r\n\r\n[Guide]: <guide.md> \"Help\"\r\n\r\nBody.\r\n\r\n",
        ),
        (
            "[Guide]",
            "Shared cache",
            "[Guide]: <guide.md> \"Help\"\r\n",
        ),
        (
            "Other section",
            MARKDOWN.path,
            "Other section\r\n=============\r\nTail.\r\n",
        ),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(entity.id)),
            "{name} -> {owner}"
        );
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            &MARKDOWN.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            exact_text
        );
    }
}

#[test]
fn markdown_nested_blocks_do_not_extend_sections_into_following_containers() {
    let source =
        "# Outer 雪\n\n> ## Quoted\n> text\n\n- ## Listed\n  item text\n\n## Following\nend\n";
    let result = output(source);
    for (name, expected) in [
        ("Quoted", "## Quoted\n> text\n"),
        ("Listed", "## Listed\n  item text\n\n"),
        ("Following", "## Following\nend\n"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let span = entity.evidence.source.as_ref().unwrap().span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            expected,
            "{name}"
        );
    }
}

#[test]
fn markdown_empty_headings_own_sections_without_inventing_a_written_definition() {
    let result = output("#\n\nBody\n\n#\n\nOther\n");
    let headings: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::DocumentSection)
        .collect();
    assert_eq!(headings.len(), 2);
    assert_ne!(headings[0].id, headings[1].id);
    assert!(
        headings
            .iter()
            .all(|entity| entity.canonical_name == "<untitled>")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition)
    );
}

#[test]
fn markdown_opaque_blocks_do_not_invent_document_headings_or_code_symbols() {
    let source = "# Written\n\n```markdown\n# Fence\n[hidden]: fake.md\n```\n\n    # Indented\n\n<!--\n# Comment\n-->\n\nParagraph with # text.\n";
    let result = output(source);
    let names: BTreeSet<_> = result
        .document()
        .entities
        .iter()
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert_eq!(names, BTreeSet::from([MARKDOWN.path, "Written"]));
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "markdown-embedded-analysis-unavailable")
            .count(),
        3
    );
}

#[test]
fn markdown_excessive_heading_name_remains_a_source_scoped_gap() {
    let source = format!(
        "# {}\n\nBody.\n\n## Visible\nTail.\n",
        "a".repeat(IrLimits::default().max_string_bytes + 1)
    );
    let result = output(&source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Visible")
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "<untitled>")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Entities
                && gap.detail == "declaration-name-unavailable"
                && gap.source.span().start_byte() == 0
                && usize::try_from(gap.source.span().end_byte()).unwrap() == source.len())
    );
}

#[test]
fn markdown_required_replay_preserves_ownership_and_scoped_unavailable_analysis() {
    let source =
        "# Guide\n\nUse [entry][ref].\n\n```rust\nfn embedded() {}\n```\n\n[ref]: entry.rs\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let demand = provider
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
        demand,
        artifact.required_syntax_fact_count(&deadline()).unwrap()
    );
    let bounded = limits_with_syntax_records(demand);
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &bounded);
    let (fresh, retained) = analyzer
        .analyze_and_capture(
            &next,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let gaps = |result: &AnalysisOutput| {
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail.starts_with("markdown-"))
            .map(|gap| (gap.detail.clone(), gap.domain, gap.source.span()))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(gaps(&first), gaps(&replay));
    for (detail, domain, text) in [
        (
            "markdown-link-resolution-unavailable",
            FactDomain::Relations,
            source,
        ),
        (
            "markdown-inline-analysis-unavailable",
            FactDomain::Occurrences,
            "Use [entry][ref].",
        ),
        (
            "markdown-embedded-analysis-unavailable",
            FactDomain::Entities,
            "```rust\nfn embedded() {}\n```\n",
        ),
    ] {
        assert!(
            replay.document().skipped_regions.iter().any(|gap| {
                let span = gap.source.span();
                gap.detail == detail
                    && gap.domain == domain
                    && &source[usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()]
                        == text
            }),
            "{detail}"
        );
    }
    assert!(
        !replay
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "embedded")
    );
    assert!(
        replay
            .document()
            .skipped_regions
            .iter()
            .all(|gap| gap.source.generation() == changed.source.generation())
    );
}

#[test]
fn markdown_repeated_headings_remain_distinct_and_body_edits_preserve_identity() {
    let source = "# Cache\n\nText.\n\n## Entry\n\nFirst.\n\n## Entry\n\nSecond.\n\n[ref]: one.md\n[ref]: two.md\n";
    let before = output(source);
    let after = output(&source.replace("First.", "A longer body with é."));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.id, entity.kind, entity.canonical_name.clone()))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(
        before.document().entities.len(),
        6,
        "{:#?}",
        before.document()
    );
    assert_eq!(identities(&before).len(), 6);
    assert_eq!(identities(&before), identities(&after));
}
