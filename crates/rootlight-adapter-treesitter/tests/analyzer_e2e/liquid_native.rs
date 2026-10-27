//! Liquid loop contracts through VFS, actual native parsing and validated IR.
//! Runtime object/property evaluation remains source-bound unavailable evidence.

use super::*;

const LIQUID: LanguageCase = LanguageCase {
    name: "liquid",
    path: "written.liquid",
    frontend: "liquid-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const ORIGINAL: &[u8] = include_bytes!("../fixtures/liquid/blog-sidebar.liquid");

fn output(raw: &[u8], encoding: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(LIQUID, raw);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(LIQUID),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, LIQUID),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.content_hash() == content_hash(raw))
    );
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    name: &str,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact definition {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition must own a Variable")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, EntityKind::Variable);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.language, "liquid");
    symbol
}

fn read(document: &rootlight_ir::NormalizedIrDocument, start: u64, end: u64, expected: SymbolId) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact ordinary Read@{start}..{end}");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: expected }
    );
}

#[test]
fn liquid_original_full_template_retains_both_variable_definitions_and_exact_read_targets() {
    assert_eq!(ORIGINAL.len(), 888);
    let result = output(ORIGINAL, "utf-8");
    let document = result.document();
    let article = definition(document, 191, 198, "article");
    let tag = definition(document, 673, 676, "tag");
    assert_ne!(article, tag);
    read(document, 270, 277, article);
    read(document, 753, 756, tag);
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 753)
    );
    assert!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind != EntityKind::Module)
            .all(|entity| entity.id == article || entity.id == tag)
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail
            .contains("runtime-template-evaluation-unavailable")
    }));
}

#[test]
fn liquid_nested_same_name_variables_keep_distinct_symbols_and_scope_boundaries() {
    let source = "{{ item }}{% for item in outer %}{{ item }}{% for item in inner %}{{ item }}{% else %}{{ item }}{% endfor %}{{ item }}{% endfor %}{{ item }}";
    let result = output(source.as_bytes(), "utf-8");
    let document = result.document();
    let items: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .collect();
    assert_eq!(items.len(), 2);
    assert_ne!(items[0].id, items[1].id);
    let resolved: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. })
        })
        .collect();
    assert_eq!(resolved.len(), 4);
    for position in [
        source.find("item }}").unwrap(),
        source.rfind("item }}").unwrap(),
    ] {
        assert!(
            !resolved
                .iter()
                .any(|site| site.source.span().start_byte() == u64::try_from(position).unwrap())
        );
    }
}

#[test]
fn liquid_assign_capture_and_tablerow_do_not_inherit_an_unproven_loop_value() {
    for middle in [
        "{% assign item = other %}{{ item }}",
        "{% capture item %}{{ item }}{% endcapture %}{{ item }}",
        "{% tablerow item in source %}{{ item }}{% endtablerow %}",
    ] {
        let source = format!("{{% for item in source %}}{middle}{{% endfor %}}");
        let result = output(source.as_bytes(), "utf-8");
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{middle}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail
                .contains("runtime-construct-semantics-unavailable")
        }));
    }
}

#[test]
fn liquid_raw_comments_strings_properties_and_external_values_do_not_become_definitions() {
    let source = "{% raw %}{% for fake in source %}{{ fake }}{% endfor %}{% endraw %}{% comment %}{{ fake }}{% endcomment %}{% for item in source %}{{ external.item }}{{ item.name | unknown: 'item' }}{% endfor %}";
    let result = output(source.as_bytes(), "utf-8");
    assert_eq!(
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .count(),
        1
    );
    assert!(!result.document().entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "fake" | "external" | "name" | "unknown"
    )));
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
            .count(),
        1
    );
}

#[test]
fn liquid_utf8_bom_and_crlf_preserve_physical_definition_and_read_bytes() {
    let source = "\u{feff}é🧭\r\n{% for item in source %}{{ item.name }}{% endfor %}";
    let result = output(source.as_bytes(), "utf-8");
    let written = u64::try_from(source.find("item in").unwrap()).unwrap();
    let used = u64::try_from(source.find("item.name").unwrap()).unwrap();
    let symbol = definition(result.document(), written, written + 4, "item");
    read(result.document(), used, used + 4, symbol);
}

#[test]
fn liquid_utf16_both_orders_retain_exact_physical_byte_targets() {
    let text = "é🧭\r\n{% for item in source %}{{ item.name }}{% endfor %}";
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let result = output(&raw, encoding);
        let written =
            2 + u64::try_from(text[..text.find("item in").unwrap()].encode_utf16().count() * 2)
                .unwrap();
        let used = 2 + u64::try_from(
            text[..text.find("item.name").unwrap()]
                .encode_utf16()
                .count()
                * 2,
        )
        .unwrap();
        let symbol = definition(result.document(), written, written + 8, "item");
        read(result.document(), used, used + 8, symbol);
    }
}

#[test]
fn liquid_malformed_or_unclosed_loops_keep_source_without_resolved_item_values() {
    for source in [
        "{% for item in source %}{{ item }}",
        "{% for item in source[broken %}{{ item }}{% endfor %}",
    ] {
        let result = output(source.as_bytes(), "utf-8");
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail
                .contains("complete-clean-native-source-proof-unavailable")
        }));
    }
}

#[test]
fn liquid_repeat_same_source_and_incremental_rewrite_do_not_reuse_old_value_symbols() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, LIQUID);
    let budget = limits();
    let fixture = Fixture::new(LIQUID, b"{% for item in source %}{{ item }}{% endfor %}");
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, LIQUID, &budget),
        &ExtensionSupport::default(),
    );
    let repeated = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, LIQUID, &budget),
        &ExtensionSupport::default(),
    );
    assert_eq!(first.document(), repeated.document());
    let changed = fixture.rewrite(b"{% for other in source %}{{ item }}{% endfor %}");
    let after = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, LIQUID, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        !after
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        after
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.content_hash() == changed.source.content_hash())
    );
}

#[test]
fn liquid_split_included_ranges_cannot_hide_a_same_item_assignment_to_certify_an_owner() {
    let source =
        b"{% for item in source %}{{ item }}{% assign item = other %}{{ item }}{% endfor %}";
    let hidden = b"{% assign item = other %}";
    let start = source
        .windows(hidden.len())
        .position(|part| part == hidden)
        .unwrap();
    let end = start + hidden.len();
    let provider = Arc::new(provider());
    let fixture = Fixture::new(LIQUID, source);
    let budget = limits();
    let ranged = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(LIQUID),
        EncodingId::utf8(),
        vec![
            IncludedRange::new(
                SourceSpan::new(
                    fixture.source.span().file(),
                    0,
                    u64::try_from(start).unwrap(),
                )
                .unwrap(),
                language(LIQUID),
            ),
            IncludedRange::new(
                SourceSpan::new(
                    fixture.source.span().file(),
                    u64::try_from(end).unwrap(),
                    u64::try_from(source.len()).unwrap(),
                )
                .unwrap(),
                language(LIQUID),
            ),
        ],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, LIQUID),
        &ranged,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Variable)
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.detail
            .contains("complete-clean-native-source-proof-unavailable")
    }));
}

fn opaque_markdown_liquid(raw: &[u8], encoding: &str) {
    let case = LanguageCase {
        name: "markdown",
        path: "written.md",
        frontend: "tree-sitter-md-0.5.3",
        source: "",
        generated: false,
        body_before: "",
        body_after: "",
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, raw);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "liquid")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind.starts_with("liquid."))
    );
    let gaps: Vec<_> = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "markdown-embedded-analysis-unavailable")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].source.content_hash(), content_hash(raw));
    assert!(gaps[0].source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
}

#[test]
fn liquid_markdown_fragments_remain_explicit_opaque_source_with_utf8_bom_and_crlf() {
    opaque_markdown_liquid("\u{feff}# Example\r\n\r\n```liquid\r\n{% for item in source %}{{ item }}{% endfor %}\r\n```\r\n".as_bytes(),"utf-8");
}

#[test]
fn liquid_markdown_fragments_keep_utf16_physical_sources_without_binding_credit() {
    let text =
        "# Example\r\n\r\n```liquid\r\n{% for item in source %}{{ item }}{% endfor %}\r\n```\r\n";
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        opaque_markdown_liquid(&raw, encoding);
    }
}
