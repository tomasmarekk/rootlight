//! JSONC analysis preserves authored comments, member occurrences and coordinates.
//! Native parser input may mask trailing commas; these public analyzer contracts
//! verify definitions, containment and retained artifacts against physical bytes.

use super::*;

/// Written JSONC data with comments, duplicate keys and trailing commas.
pub(super) const JSONC: LanguageCase = LanguageCase {
    name: "jsonc",
    path: "src/settings.JSONC",
    frontend: "tree-sitter-json-0.24.8",
    source: "// leading λ😀\r\n{\r\n  \"name\":1,\r\n  \"name\":2,\r\n  \"items\":[{\"id\":3,},null,{\"id\":4,},],\r\n  \"nested\":{\"id\":5,},\r\n  \"\":6,\r\n  \"\\u0061\":7, /* closing ,]} */\r\n}\r\n// tail\r\n",
    generated: false,
    body_before: ":3",
    body_after: ":30",
};

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn assert_original_evidence(output: &AnalysisOutput, source: &str) {
    let document = output.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    let file = &document.files[0];
    assert_eq!(file.language, "jsonc");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(
            document
                .occurrences
                .iter()
                .map(|occurrence| &occurrence.source),
        )
        .chain(
            document
                .relations
                .iter()
                .filter_map(|relation| relation.evidence.source.as_ref()),
        )
        .chain(document.skipped_regions.iter().map(|gap| &gap.source))
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|extension| extension.evidence.source.as_ref()),
        )
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), file.content_hash);
        let _ = text(source, reference);
    }
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
    }
    for extension in &document.extensions {
        if extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            assert!(!lexical.is_truncated());
            if lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature {
                lexical
                    .verify_source_bytes(
                        text(source, extension.evidence.source.as_ref().unwrap()).as_bytes(),
                    )
                    .unwrap();
            }
        }
    }
}

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(JSONC, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, JSONC),
        &request(&fixture.snapshot, &fixture.source, JSONC, &budget),
        &ExtensionSupport::default(),
    );
    assert_original_evidence(&result, source);
    result
}

#[test]
fn jsonc_comments_and_trailing_commas_keep_exact_definitions_and_containment() {
    let result = output(JSONC.source);
    let document = result.document();
    assert_eq!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
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
    for (raw, summary) in [
        ("// leading λ😀\r", "leading λ😀"),
        ("/* closing ,]} */", "closing ,]}"),
        ("// tail\r", "tail"),
    ] {
        assert!(
            document.extensions.iter().any(|extension| {
                if extension.namespace != rootlight_ir::LEXICAL_EXTENSION_NAMESPACE {
                    return false;
                }
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                text(JSONC.source, extension.evidence.source.as_ref().unwrap()) == raw
                    && lexical.text() == summary
            }),
            "missing original comment and summary: {raw}"
        );
    }
    let properties: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 9, "{properties:?}");
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        properties.len()
    );
    assert!(properties.iter().all(|entity| entity.language == "jsonc"));
    assert!(
        properties
            .iter()
            .any(|entity| entity.canonical_name == r#""a""# && entity.display_name == "a")
    );
    for property in &properties {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == (OccurrenceTarget::Resolved {
                            symbol: property.id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        assert!(text(JSONC.source, &definitions[0].source).starts_with('"'));
        assert_eq!(definitions[0].evidence.source, property.evidence.source);
    }
    let items = properties
        .iter()
        .find(|property| property.canonical_name == r#""items""#)
        .unwrap();
    let contained_ids: BTreeSet<_> = document
        .relations
        .iter()
        .filter_map(|relation| {
            if relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(items.id)
            {
                match relation.object {
                    RelationEndpoint::Entity(entity) => Some(entity),
                    _ => None,
                }
            } else {
                None
            }
        })
        .collect();
    assert_eq!(
        properties
            .iter()
            .filter(|property| property.canonical_name == r#""id""#
                && contained_ids.contains(&property.id))
            .count(),
        2
    );
}

#[test]
fn jsonc_values_and_comment_edits_keep_member_identities_and_artifact_replay() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, JSONC);
    let fixture = Fixture::new(JSONC, JSONC.source.as_bytes());
    let budget = limits();
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, JSONC, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let required = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(
                &request(&fixture.snapshot, &fixture.source, JSONC, &budget).to_parse_request(),
                &deadline(),
            )
            .unwrap()
    );
    let rebound = fixture.next_generation();
    let minimum = limits_with_syntax_records(required);
    let (_, retained) = analyzer
        .analyze_and_capture(
            &request(&rebound.snapshot, &rebound.source, JSONC, &minimum),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &request(&rebound.snapshot, &rebound.source, JSONC, &minimum),
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(
        &analyzer,
        &request(&rebound.snapshot, &rebound.source, JSONC, &minimum),
        &ExtensionSupport::default(),
    );
    assert_eq!(replay.document(), fresh.document());
    assert_original_evidence(&replay, JSONC.source);
    let changed_text = JSONC
        .source
        .replace(JSONC.body_before, JSONC.body_after)
        .replace("leading", "different leading");
    let changed = fixture.rewrite(changed_text.as_bytes());
    let changed_output = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, JSONC, &budget),
        &ExtensionSupport::default(),
    );
    assert_original_evidence(&changed_output, &changed_text);
    assert_eq!(
        symbol_ids(first.document()),
        symbol_ids(changed_output.document())
    );
    let too_small = limits_with_syntax_records(required - 1);
    assert!(
        analyzer
            .analyze_and_capture(
                &request(&rebound.snapshot, &rebound.source, JSONC, &too_small),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err()
    );
}

#[test]
fn jsonc_malformed_input_keeps_exact_explicit_recovery_gaps() {
    for source in [
        "[,]",
        "[1,,]",
        "{,}",
        "{\"a\":,}",
        "{\"a\",}",
        "[1,}",
        "[1,/* unterminated ]",
        "[\"unterminated,]",
        "{\"a\":1} {\"b\":2}",
        "// comment only\n",
    ] {
        let result = output(source);
        assert_ne!(
            result.report().coverage().status(),
            CoverageStatus::Complete,
            "{source}"
        );
        assert!(!result.document().diagnostics.is_empty(), "{source}");
        assert!(!result.document().skipped_regions.is_empty(), "{source}");
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .all(|gap| !gap.detail.is_empty()),
            "{source}"
        );
    }
}

#[test]
fn jsonc_trailing_comma_acceptance_does_not_change_json_route() {
    let source = r#"{"key":[1,],}"#;
    let jsonc = output(source);
    assert_eq!(jsonc.report().coverage().status(), CoverageStatus::Complete);
    let provider = Arc::new(provider());
    let fixture = Fixture::new(JSON_CASE, source.as_bytes());
    let budget = limits();
    let json = analyze(
        &analyzer(&provider, JSON_CASE),
        &request(&fixture.snapshot, &fixture.source, JSON_CASE, &budget),
        &ExtensionSupport::default(),
    );
    assert_ne!(json.report().coverage().status(), CoverageStatus::Complete);
    assert!(!json.document().skipped_regions.is_empty());
    assert_eq!(json.document().files[0].language, "json");
}

#[test]
fn jsonc_utf16_definitions_keep_exact_physical_offsets_and_original_hashes() {
    let source = "// leading λ😀\r\n{\"λ\":1,\"items\":[{\"id\":2,},],}\r\n";
    for (encoding, little) in [("utf-16le", true), ("utf-16be", false)] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(JSONC, &bytes);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(JSONC),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, JSONC),
            &input,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(
            result.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(
            document.skipped_regions.is_empty(),
            "{:?}",
            document.skipped_regions
        );
        assert_eq!(document.files[0].content_hash, content_hash(&bytes));
        assert_eq!(document.files[0].encoding, encoding);
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
            .collect();
        assert_eq!(definitions.len(), 3);
        for definition in definitions {
            let start = usize::try_from(definition.source.span().start_byte()).unwrap();
            let end = usize::try_from(definition.source.span().end_byte()).unwrap();
            assert_eq!(definition.source.content_hash(), content_hash(&bytes));
            let units: Vec<_> = bytes[start..end]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    if little {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                })
                .collect();
            let key = String::from_utf16(&units).unwrap();
            assert_eq!(definition.syntactic_text_hash, content_hash(key.as_bytes()));
            assert!(["\"λ\"", "\"items\"", "\"id\""].contains(&key.as_str()));
            let text_offset = source.find(&key).unwrap();
            let expected = 2 + source[..text_offset].encode_utf16().count() * 2;
            assert_eq!(start, expected);
        }
    }
}

#[test]
fn markdown_jsonc_fences_keep_distinct_language_and_original_trailing_comma_bytes() {
    let source = "# Settings λ😀\r\n\r\n```jsonc\r\n// literal, comment\r\n{\"items\":[{\"id\":2,},],}\r\n```\r\n";
    let markdown = LanguageCase {
        name: "markdown",
        path: "docs/settings.md",
        frontend: "tree-sitter-md-0.5.3",
        source,
        generated: false,
        body_before: ":2",
        body_after: ":3",
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(markdown, source.as_bytes());
    let budget = limits();
    let analyzer = analyzer(&provider, markdown);
    let (result, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, markdown, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let details: BTreeSet<_> = result
        .document()
        .skipped_regions
        .iter()
        .map(|gap| gap.detail.as_str())
        .collect();
    assert_eq!(
        details,
        BTreeSet::from([
            "markdown-link-resolution-unavailable",
            "markdown-code-semantics-unavailable",
        ])
    );
    let jsonc: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "jsonc" && entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(jsonc.len(), 2);
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| entity.language != "json")
    );
    for entity in jsonc {
        assert!(text(source, entity.evidence.source.as_ref().unwrap()).contains(':'));
    }
    let rebound = fixture.next_generation();
    let replay = analyzer
        .analyze_from_artifact(
            &request(&rebound.snapshot, &rebound.source, markdown, &budget),
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(symbol_ids(result.document()), symbol_ids(replay.document()));
}

#[test]
fn markdown_jsonc_deindented_fragments_keep_definitions_and_cross_line_trailing_commas() {
    let source =
        "> ```jsonc\r\n> {\r\n>   \"items\": [\r\n>     {\"id\":2,},\r\n>   ],\r\n> }\r\n> ```\r\n";
    let markdown = LanguageCase {
        name: "markdown",
        path: "docs/settings.md",
        frontend: "tree-sitter-md-0.5.3",
        source,
        generated: false,
        body_before: ":2",
        body_after: ":3",
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(markdown, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, markdown),
        &request(&fixture.snapshot, &fixture.source, markdown, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    assert!(
        result.document().skipped_regions.iter().all(|gap| [
            "markdown-link-resolution-unavailable",
            "markdown-code-semantics-unavailable",
        ]
        .contains(&gap.detail.as_str())),
        "{:?}",
        result.document().skipped_regions
    );
    let properties: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "jsonc" && entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 2);
    for property in properties {
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target
                        == OccurrenceTarget::Resolved {
                            symbol: property.id,
                        }
            })
            .unwrap();
        assert!(["\"items\"", "\"id\""].contains(&text(source, &definition.source)));
        assert_eq!(
            definition.source.content_hash(),
            content_hash(source.as_bytes())
        );
    }
}
