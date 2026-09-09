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
fn markdown_named_links_and_images_bind_the_first_document_definition() {
    let source = "[text][Guide] [GUIDE][] [guide] ![alt][Guide] ![guide][] ![GUIDE]\n\n> [Guide]: first.md\n\n[guide]: second.md\n";
    let result = output(source);
    let target = result
        .document()
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::LinkDefinition && entity.canonical_name == "[Guide]"
        })
        .unwrap();
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 6, "{references:#?}");
    for reference in references {
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        let span = reference.source.span();
        assert_eq!(
            reference.syntactic_text_hash,
            content_hash(
                &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]
            )
        );
        assert!(result.document().relations.iter().any(|relation| {
            relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                && relation.subject == rootlight_ir::RelationEndpoint::Occurrence(reference.id)
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
        }));
    }
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::LinkDefinition)
            .count(),
        2
    );
}

#[test]
fn markdown_reference_comparison_preserves_unicode_and_container_source() {
    let source = "> [text][Straße\r\n> label] [É] [e\u{301}] [A\u{a0}B] [a b]\r\n\r\n[STRASSE label]: one.md\r\n[é]: two.md\r\n[a b]: three.md\r\n";
    let result = output(source);
    for (written, expected) in [
        ("[Straße\r\n> label]", Some("[STRASSE label]")),
        ("[É]", Some("[é]")),
        ("[e\u{301}]", None),
        ("[A\u{a0}B]", None),
        ("[a b]", Some("[a b]")),
    ] {
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.syntactic_text_hash == content_hash(written.as_bytes())
            })
            .unwrap_or_else(|| panic!("missing {written:?}"));
        let target = expected.map(|name| {
            result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap()
                .id
        });
        if let Some(symbol) = target {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol },
                "{written}"
            );
        } else {
            assert!(
                matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                "{written}"
            );
        }
    }
}

#[test]
fn markdown_bounded_plans_do_not_guess_reference_targets() {
    let source = format!("{}\n[ref]: target.md\n", "[ref]\n\n".repeat(34));
    let result = output(&source);
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    let references: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect();
    assert_eq!(references.len(), 32);
    for occurrence in references {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail == "markdown-reference-target-unavailable"
                && gap.source.span() == occurrence.source.span()
        }));
    }
}

#[test]
fn markdown_resolved_reference_artifacts_rebind_without_changing_targets() {
    let source = "> [text][first\n> label]\n\n> [FIRST\n> LABEL]: target.md\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, MARKDOWN);
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, MARKDOWN, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next, &ExtensionSupport::default());
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(fresh.report(), replay.report());
    let original = first
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert!(matches!(original.target, OccurrenceTarget::Resolved { .. }));
    let current = replay
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .unwrap();
    assert_eq!(current.target, original.target);
    assert_eq!(current.source.generation(), changed.source.generation());
    assert_eq!(
        current.syntactic_text_hash,
        content_hash(b"[first\n> label]")
    );
}

#[test]
fn markdown_inline_evidence_preserves_written_spans_and_hashes() {
    let source = "# Links\r\n\r\n> é [full][Guide] [Guide][] [Guide] [direct](guide.md)\r\n> <https://example.test> and `code [opaque](hidden.md)`\r\n\r\n[Guide]: target.md\r\n";
    let result = output(source);
    let actual: BTreeSet<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
        .map(|occurrence| {
            let span = occurrence.source.span();
            let text = &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(text.as_bytes())
            );
            assert_eq!(
                occurrence.source.content_hash(),
                content_hash(source.as_bytes())
            );
            (occurrence.syntax_kind.as_str(), text)
        })
        .collect();
    for expected in [
        ("markdown.reference_label.reference", "[Guide]"),
        ("markdown.collapsed_link.reference", "[Guide][]"),
        ("markdown.shortcut_link.reference", "[Guide]"),
        ("markdown.link_destination.reference", "guide.md"),
        (
            "markdown.link_destination.reference",
            "<https://example.test>",
        ),
        ("markdown.code_span.string", "`code [opaque](hidden.md)`"),
    ] {
        assert!(
            actual.contains(&expected),
            "missing {expected:?}: {actual:#?}"
        );
    }
    assert!(
        !actual
            .iter()
            .any(|(kind, text)| kind.ends_with(".reference") && text.contains("hidden.md"))
    );
}

#[test]
fn markdown_inline_delimiters_do_not_cross_paragraphs() {
    let source = "[opening\n\nclosing](hidden.md)\n\n`[literal](also-hidden.md)`\n\n```text\n[opaque](fenced.md)\n```\n";
    let result = output(source);
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|occurrence| { occurrence.syntax_kind != "markdown.link_destination.reference" })
    );
}

#[test]
fn markdown_inline_embedded_syntax_remains_an_exact_scoped_gap() {
    let source = "Written <i title='x'>word</i> and $x+y$.\n";
    let result = output(source);
    let gaps: BTreeSet<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-embedded-unavailable")
        .map(|gap| {
            assert_eq!(gap.domain, FactDomain::Entities);
            let span = gap.source.span();
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(gaps, BTreeSet::from(["<i title='x'>", "</i>", "$x+y$"]));
}

#[test]
fn markdown_inline_range_budget_reports_each_unparsed_block() {
    let source = "[direct](guide.md)\n\n".repeat(34);
    let result = output(&source);
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.syntax_kind == "markdown.link_destination.reference"
            })
            .count(),
        32
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-inline-budget-unavailable")
        .collect();
    assert_eq!(gaps.len(), 2);
    for gap in gaps {
        let span = gap.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            "[direct](guide.md)"
        );
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
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
    assert!(first.document().occurrences.iter().any(|occurrence| {
        occurrence.syntax_kind == "markdown.reference_label.reference"
            && occurrence.syntactic_text_hash == content_hash(b"[ref]")
    }));
    assert!(!replay.document().skipped_regions.iter().any(|gap| {
        gap.detail == "markdown-inline-analysis-unavailable"
            || gap.detail == "markdown-inline-parse-unavailable"
    }));
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
