//! CPP macros retain written names, headers and lexical replacement evidence.
//! Host provenance remains independent of embedded CPP language attribution;
//! neither private parser input nor lexical text establishes evaluated expansion.

use super::*;

const CPP: LanguageCase = LanguageCase {
    name: "cpp",
    path: "include/written.hpp",
    frontend: "tree-sitter-cpp-0.23.4",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

const MARKDOWN: LanguageCase = LanguageCase {
    name: "markdown",
    path: "docs/written.md",
    frontend: "tree-sitter-md-0.5.3",
    ..CPP
};

type SourceControl = (
    &'static str,
    &'static [u8],
    &'static [&'static str],
    &'static [&'static str],
);

const CONTROLS: &[SourceControl] = &[
    (
        "multiline-plain",
        b"#define WRITTEN 1000 /* comment\n#define FAKE \"hidden\"\n*/ \"tail\"\nint after(void) { return 0; }\n",
        &["WRITTEN"],
        &["\"tail\""],
    ),
    (
        "multiline-separator",
        b"#define WRITTEN 1'000 /* comment\n#define FAKE \"hidden\"\n*/ \"tail\"\nint after(void) { return 0; }\n",
        &["WRITTEN"],
        &["\"tail\""],
    ),
    (
        "numeric-full",
        b"#define DECIMAL 1'000\n#define FOLLOWED 1'000 \"written\"\n#define HEXADECIMAL 0xDEAD'BEEF\n#define BINARY 0b10'01\n#define FLOATING .1'25e+2 \"float\"\n#define CONTINUED 1'\\\n000 \"continued\"\n#define CHARACTER 'x'\n#define COMMENTED 1'000 /* \"hidden\" */ \"tail\"\n#define UNFINISHED 'unfinished\n",
        &[
            "DECIMAL",
            "FOLLOWED",
            "HEXADECIMAL",
            "BINARY",
            "FLOATING",
            "CONTINUED",
            "CHARACTER",
            "COMMENTED",
            "UNFINISHED",
        ],
        &["\"written\"", "\"float\"", "\"continued\"", "\"tail\""],
    ),
    (
        "replacement-call",
        b"#define CALL 1 /* comment */ phantom()\nint after(void) { return 0; }\n",
        &["CALL"],
        &[],
    ),
    (
        "replacement-declaration",
        b"#define DECL 1 /* comment */ int phantom(void);\nint after(void) { return 0; }\n",
        &["DECL"],
        &[],
    ),
    (
        "same-comment-plain",
        b"#define WRITTEN 1000 /* comment */ \"tail\"\nint after(void) { return 0; }\n",
        &["WRITTEN"],
        &["\"tail\""],
    ),
    (
        "same-comment-separator",
        b"#define WRITTEN 1'000 /* comment */ \"tail\"\nint after(void) { return 0; }\n",
        &["WRITTEN"],
        &["\"tail\""],
    ),
    (
        "same-no-comment-separator",
        b"#define WRITTEN 1'000 \"tail\"\nint after(void) { return 0; }\n",
        &["WRITTEN"],
        &["\"tail\""],
    ),
    (
        "written-full",
        b"#define WRITTEN 1'000 /* comment\r\n#define FAKE \"hidden\"\r\n*/ \"tail\"\r\n#define DECL 1 /* comment */ int phantom(void);\r\n#define CALL 1 /* comment */ phantom()\r\n#define NEXT \"next\"\r\nint after(void) { return 0; }\r\n",
        &["WRITTEN", "DECL", "CALL", "NEXT"],
        &["\"tail\"", "\"next\""],
    ),
    (
        "no-room",
        b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n#define ESCAPED\\\n /* comment */ \"tail\"\nint after(void) { return 0; }\n",
        &["EMPTY", "ESCAPED"],
        &["\"tail\""],
    ),
    (
        "cpp-written-baseline",
        b"#define CPP_OBJECT \"value\"\n#define CPP_FUNCTION(arg) u8\"prefix\" arg \"suffix\"\nnamespace written { int after(void) { return 0; } }\n",
        &["CPP_OBJECT", "CPP_FUNCTION"],
        &["\"value\"", "u8\"prefix\"", "\"suffix\""],
    ),
    (
        "cpp-raw-replacement",
        b"#define CPP_RAW R\"tag(/* literal \"quoted\" // not comment */)tag\"\n#define CPP_UTF8_RAW u8R\"sentinel(alpha \"quoted\" /* literal */)sentinel\"\nnamespace written { template<class T> T after(T value) { return value; } }\n",
        &["CPP_RAW", "CPP_UTF8_RAW"],
        &[
            "R\"tag(/* literal \"quoted\" // not comment */)tag\"",
            "u8R\"sentinel(alpha \"quoted\" /* literal */)sentinel\"",
        ],
    ),
];

const HEADERS: &[(&str, &str, usize, usize)] = &[
    ("cpp-raw-replacement", "CPP_RAW", 0, 65),
    ("cpp-raw-replacement", "CPP_UTF8_RAW", 65, 138),
    ("cpp-written-baseline", "CPP_OBJECT", 0, 27),
    ("cpp-written-baseline", "CPP_FUNCTION", 27, 77),
    ("multiline-plain", "WRITTEN", 0, 64),
    ("multiline-separator", "WRITTEN", 0, 65),
    ("no-room", "EMPTY", 0, 15),
    ("no-room", "ESCAPED", 60, 99),
    ("numeric-full", "DECIMAL", 0, 22),
    ("numeric-full", "FOLLOWED", 22, 55),
    ("numeric-full", "HEXADECIMAL", 55, 87),
    ("numeric-full", "BINARY", 87, 110),
    ("numeric-full", "FLOATING", 110, 144),
    ("numeric-full", "CONTINUED", 144, 182),
    ("numeric-full", "CHARACTER", 182, 204),
    ("numeric-full", "COMMENTED", 204, 250),
    ("numeric-full", "UNFINISHED", 250, 281),
    ("replacement-call", "CALL", 0, 39),
    ("replacement-declaration", "DECL", 0, 48),
    ("same-comment-plain", "WRITTEN", 0, 42),
    ("same-comment-separator", "WRITTEN", 0, 43),
    ("same-no-comment-separator", "WRITTEN", 0, 29),
    ("written-full", "WRITTEN", 0, 68),
    ("written-full", "DECL", 68, 117),
    ("written-full", "CALL", 117, 157),
    ("written-full", "NEXT", 157, 178),
];

fn raw_text<'a>(raw: &'a [u8], source: &SourceRef) -> &'a [u8] {
    &raw[usize::try_from(source.span().start_byte()).unwrap()
        ..usize::try_from(source.span().end_byte()).unwrap()]
}

// Syntactic hashes use decoded spelling; SourceRefs still address physical bytes.
fn spelling_bytes(raw: &[u8], source: &SourceRef, encoding: &str) -> Vec<u8> {
    let bytes = raw_text(raw, source);
    match encoding {
        "utf-8" => bytes.to_vec(),
        "utf-16le" | "utf-16be" => {
            let mut pairs = bytes.chunks_exact(2);
            let units = pairs
                .by_ref()
                .map(|pair| {
                    if encoding == "utf-16le" {
                        u16::from_le_bytes([pair[0], pair[1]])
                    } else {
                        u16::from_be_bytes([pair[0], pair[1]])
                    }
                })
                .collect::<Vec<_>>();
            assert!(pairs.remainder().is_empty());
            String::from_utf16(&units).unwrap().into_bytes()
        }
        _ => panic!("unexpected fixture encoding: {encoding}"),
    }
}

fn assert_source(result: &AnalysisOutput, raw: &[u8], host: LanguageCase) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, host.name);
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_eq!(document.files[0].content_hash, content_hash(raw));
    assert_eq!(document.provenance.len(), 1);
    assert_eq!(document.provenance[0].language, host.name);
    assert_eq!(
        document.provenance[0].frontend_version.as_deref(),
        Some(host.frontend)
    );
    for occurrence in &document.occurrences {
        assert_eq!(occurrence.source.span().file(), document.files[0].id);
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(&spelling_bytes(
                raw,
                &occurrence.source,
                &document.files[0].encoding
            ))
        );
    }
    for source in document
        .entities
        .iter()
        .filter_map(|row| row.evidence.source.as_ref())
        .chain(document.skipped_regions.iter().map(|row| &row.source))
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|row| row.evidence.source.as_ref()),
        )
    {
        assert_eq!(source.content_hash(), content_hash(raw));
        assert_eq!(source.span().file(), document.files[0].id);
        assert_eq!(source.repository(), document.repository);
        assert_eq!(source.generation(), document.generation);
        let _ = raw_text(raw, source);
    }
}

fn checked(
    input: &AnalysisRequest<'_>,
    provider: &Arc<TreeSitterProvider>,
    host: LanguageCase,
) -> AnalysisOutput {
    let cancellation = Cancellation::new();
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &cancellation)
        .unwrap();
    let analyzer = analyzer(provider, host);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        required,
        artifact.required_syntax_fact_count(&cancellation).unwrap()
    );
    let replay = analyzer
        .analyze_from_artifact(
            input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    fresh
}

fn signatures(result: &AnalysisOutput, raw: &[u8], symbol: SymbolId) -> Vec<String> {
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let value = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            if value.kind() != rootlight_ir::LexicalEvidenceKind::Signature
                || value.subject() != rootlight_ir::FactRef::Entity(symbol)
            {
                return None;
            }
            assert!(!value.is_truncated());
            value
                .verify_source_bytes(raw_text(raw, extension.evidence.source.as_ref().unwrap()))
                .unwrap();
            Some(value.text().to_owned())
        })
        .collect()
}

#[test]
fn cpp_original_twelve_controls_keep_authored_macros_literals_and_later_owners_or_precise_gaps() {
    let provider = Arc::new(provider());
    let budget = limits();
    for &(name, raw, macros, literals) in CONTROLS {
        let source = std::str::from_utf8(raw).unwrap();
        let fixture = Fixture::new(CPP, raw);
        let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
        let result = checked(&input, &provider, CPP);
        assert_source(&result, raw, CPP);
        assert_ne!(
            result.report().coverage().status(),
            CoverageStatus::Complete,
            "{name}"
        );
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|row| matches!(row.canonical_name.as_str(), "FAKE" | "phantom")),
            "{name}"
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|row| row.role == OccurrenceRole::CallSite
                    && raw_text(raw, &row.source) == b"phantom"),
            "{name}"
        );
        if name == "no-room" {
            assert!(result.document().entities.is_empty());
            assert!(result.document().occurrences.is_empty());
            let gaps = result
                .document()
                .skipped_regions
                .iter()
                .filter(|row| row.detail == "cpp-preprocessor-replacement-not-analyzed")
                .collect::<Vec<_>>();
            assert_eq!(gaps.len(), 4);
            assert_eq!(
                gaps.iter().map(|row| row.domain).collect::<BTreeSet<_>>(),
                [
                    FactDomain::Diagnostics,
                    FactDomain::Entities,
                    FactDomain::Occurrences,
                    FactDomain::Relations
                ]
                .into_iter()
                .collect()
            );
            assert!(gaps.iter().all(|row| raw_text(raw, &row.source) == raw));
            continue;
        }
        for &macro_name in macros {
            let entities = result
                .document()
                .entities
                .iter()
                .filter(|row| row.language == "cpp" && row.canonical_name == macro_name)
                .collect::<Vec<_>>();
            assert_eq!(entities.len(), 1, "{name}: {macro_name}");
            assert_eq!(
                entities[0].kind,
                if macro_name == "CPP_FUNCTION" {
                    EntityKind::Function
                } else {
                    EntityKind::Constant
                }
            );
            let definitions = result
                .document()
                .occurrences
                .iter()
                .filter(|row| {
                    row.role == OccurrenceRole::Definition
                        && row.target
                            == OccurrenceTarget::Resolved {
                                symbol: entities[0].id,
                            }
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1, "{name}: {macro_name}");
            assert_eq!(raw_text(raw, &definitions[0].source), macro_name.as_bytes());
            let header = signatures(&result, raw, entities[0].id);
            assert_eq!(header.len(), 1, "{name}: {macro_name}");
            let &(_, _, start, end) = HEADERS
                .iter()
                .find(|row| row.0 == name && row.1 == macro_name)
                .unwrap();
            assert_eq!(
                header[0].as_bytes(),
                &raw[start..end],
                "{name}: {macro_name}"
            );
            assert!(
                result
                    .document()
                    .relations
                    .iter()
                    .any(|row| row.predicate == RelationPredicate::Contains
                        && row.subject == RelationEndpoint::File(result.document().files[0].id)
                        && row.object == RelationEndpoint::Entity(entities[0].id))
            );
            assert_eq!(
                header[0].as_bytes(),
                raw_text(raw, entities[0].evidence.source.as_ref().unwrap())
            );
        }
        let mut strings = result
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == OccurrenceRole::StringEvidence)
            .collect::<Vec<_>>();
        strings.sort_by_key(|row| row.source.span().start_byte());
        assert_eq!(
            strings
                .iter()
                .map(|row| raw_text(raw, &row.source))
                .collect::<Vec<_>>(),
            literals
                .iter()
                .map(|row| row.as_bytes())
                .collect::<Vec<_>>(),
            "{name}"
        );
        for string in strings {
            let owner = result
                .document()
                .entities
                .iter()
                .find(|row| Some(row.id) == string.enclosing)
                .unwrap();
            assert!(macros.contains(&owner.canonical_name.as_str()));
            let owner_span = owner.evidence.source.as_ref().unwrap().span();
            assert!(
                owner_span.start_byte() <= string.source.span().start_byte()
                    && string.source.span().end_byte() <= owner_span.end_byte()
            );
            let start = string.source.span().start_byte();
            let end = string.source.span().end_byte();
            assert!(
                !result
                    .document()
                    .occurrences
                    .iter()
                    .any(|row| row.role == OccurrenceRole::Documentation
                        && start <= row.source.span().start_byte()
                        && row.source.span().end_byte() <= end),
                "{name}"
            );
        }
        let expected_comments: &[&[u8]] = match name {
            "multiline-plain" | "multiline-separator" => {
                &[b"/* comment\n#define FAKE \"hidden\"\n*/"]
            }
            "numeric-full" => &[b"/* \"hidden\" */"],
            "replacement-call"
            | "replacement-declaration"
            | "same-comment-plain"
            | "same-comment-separator" => &[b"/* comment */"],
            "written-full" => &[
                b"/* comment\r\n#define FAKE \"hidden\"\r\n*/",
                b"/* comment */",
                b"/* comment */",
            ],
            _ => &[],
        };
        let mut comments = result
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == OccurrenceRole::Documentation)
            .collect::<Vec<_>>();
        comments.sort_by_key(|row| row.source.span().start_byte());
        assert_eq!(
            comments
                .iter()
                .map(|row| raw_text(raw, &row.source))
                .collect::<Vec<_>>(),
            expected_comments,
            "{name}"
        );
        for comment in comments {
            assert!(comment.enclosing.is_some());
        }
        if source.contains("int after(") || source.contains("T after(") {
            assert_eq!(
                result
                    .document()
                    .entities
                    .iter()
                    .filter(|row| row.language == "cpp" && row.canonical_name == "after")
                    .count(),
                1,
                "{name}"
            );
        }
        if source.contains("namespace written") {
            assert_eq!(
                result
                    .document()
                    .entities
                    .iter()
                    .filter(|row| row.language == "cpp"
                        && row.kind == EntityKind::Module
                        && row.canonical_name == "written")
                    .count(),
                1
            );
        }
        if name == "numeric-full" {
            let gaps = result
                .document()
                .skipped_regions
                .iter()
                .filter(|row| row.detail == "cpp-preprocessor-replacement-lexical-unavailable")
                .collect::<Vec<_>>();
            assert_eq!(gaps.len(), 3);
            assert!(
                gaps.iter()
                    .all(|row| raw_text(raw, &row.source) == b"'unfinished\n")
            );
        }
    }
}

#[test]
fn cpp_raw_literals_preserve_physical_bom_utf16_crlf_and_replay() {
    let source =
        "#define RAW u8R\"tag(λ😀 \\\r\n/* literal */)tag\"\r\nint after(void) { return 0; }\r\n";
    let expected = "u8R\"tag(λ😀 \\\r\n/* literal */)tag\"";
    let provider = Arc::new(provider());
    let budget = limits();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |value: &str| -> Vec<u8> {
            match encoding {
                "utf-8" => value.as_bytes().to_vec(),
                "utf-16le" => value.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                _ => value.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            }
        };
        let bom: &[u8] = match encoding {
            "utf-8" => b"\xef\xbb\xbf",
            "utf-16le" => b"\xff\xfe",
            _ => b"\xfe\xff",
        };
        let raw = [bom, encode(source).as_slice()].concat();
        let fixture = Fixture::new(CPP, &raw);
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(CPP),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = checked(&input, &provider, CPP);
        assert_source(&result, &raw, CPP);
        assert_eq!(result.document().files[0].encoding, encoding);
        let strings = result
            .document()
            .occurrences
            .iter()
            .filter(|row| row.role == OccurrenceRole::StringEvidence)
            .collect::<Vec<_>>();
        assert_eq!(strings.len(), 1);
        assert_eq!(raw_text(&raw, &strings[0].source), encode(expected));
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|row| row.language == "cpp" && row.canonical_name == "after")
        );
    }
}

#[test]
fn cpp_embedded_projection_preserves_markdown_host_and_other_fences() {
    let source = "# Written\n\n```cpp\n#define RAW R\"tag(/* literal */)tag\"\nnamespace written { int after(void) { return 0; } }\n```\n\n```python\ndef neighbor():\n    return 1\n```\n\n```cpp\n#define EMPTY \nint withheld(void) { return 0; }\n```\n";
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(MARKDOWN, source.as_bytes());
    let input = request(&fixture.snapshot, &fixture.source, MARKDOWN, &budget);
    let result = checked(&input, &provider, MARKDOWN);
    assert_source(&result, source.as_bytes(), MARKDOWN);
    for (name, language) in [
        ("RAW", "cpp"),
        ("written", "cpp"),
        ("after", "cpp"),
        ("neighbor", "python"),
    ] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|row| row.canonical_name == name && row.language == language),
            "{name}"
        );
    }
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|row| matches!(row.canonical_name.as_str(), "EMPTY" | "withheld"))
    );
    let gaps = result
        .document()
        .skipped_regions
        .iter()
        .filter(|row| row.detail == "cpp-preprocessor-replacement-not-analyzed")
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 3);
    assert!(
        gaps.iter()
            .all(|row| raw_text(source.as_bytes(), &row.source)
                == b"#define EMPTY \nint withheld(void) { return 0; }\n")
    );
    let cpp_body = result
        .document()
        .entities
        .iter()
        .find(|row| {
            row.language == "cpp"
                && row.kind == EntityKind::Module
                && row.evidence.source.as_ref().is_some_and(|source_ref| {
                    raw_text(source.as_bytes(), source_ref).starts_with(b"#define EMPTY")
                })
        })
        .unwrap();
    let document = result.document();
    let host_module = document
        .entities
        .iter()
        .find(|row| {
            row.language == "markdown"
                && row.kind == EntityKind::Module
                && row.canonical_name == MARKDOWN.path
        })
        .unwrap();
    let section = document
        .entities
        .iter()
        .find(|row| {
            row.language == "markdown"
                && row.kind == EntityKind::DocumentSection
                && row.canonical_name == "Written"
        })
        .unwrap();
    assert_eq!(
        raw_text(source.as_bytes(), section.evidence.source.as_ref().unwrap()),
        source.as_bytes()
    );
    let blocked_fence = "```cpp\n#define EMPTY \nint withheld(void) { return 0; }\n```\n";
    let fence_start = source.find(blocked_fence).unwrap();
    let fence_end = fence_start + blocked_fence.len();
    let fences: Vec<_> = document
        .entities
        .iter()
        .filter(|row| {
            row.language == "markdown"
                && row.kind == EntityKind::Module
                && row.canonical_name == "<code-block>"
                && row.evidence.source.as_ref().is_some_and(|site| {
                    site.span().start_byte() == u64::try_from(fence_start).unwrap()
                        && site.span().end_byte() == u64::try_from(fence_end).unwrap()
                        && raw_text(source.as_bytes(), site) == blocked_fence.as_bytes()
                })
        })
        .collect();
    assert_eq!(fences.len(), 1);
    let fence = fences[0];
    assert_eq!(
        fence.container,
        Some(rootlight_ir::ContainerRef::Entity(section.id))
    );
    let cpp_source = cpp_body.evidence.source.as_ref().unwrap();
    assert_eq!(
        cpp_source.span().start_byte(),
        u64::try_from(fence_start + "```cpp\n".len()).unwrap()
    );
    assert_eq!(
        cpp_source.span().end_byte(),
        u64::try_from(fence_end - "```\n".len()).unwrap()
    );
    assert_eq!(
        raw_text(source.as_bytes(), cpp_source),
        b"#define EMPTY \nint withheld(void) { return 0; }\n"
    );
    assert_eq!(
        cpp_body.container,
        Some(rootlight_ir::ContainerRef::Entity(fence.id))
    );
    for (parent, child) in [
        (RelationEndpoint::File(document.files[0].id), host_module.id),
        (RelationEndpoint::Entity(host_module.id), section.id),
        (RelationEndpoint::Entity(section.id), fence.id),
        (RelationEndpoint::Entity(fence.id), cpp_body.id),
    ] {
        assert!(document.relations.iter().any(|row| {
            row.predicate == RelationPredicate::Contains
                && row.subject == parent
                && row.object == RelationEndpoint::Entity(child)
        }));
    }
}

#[test]
fn cpp_included_ranges_do_not_expose_excluded_macros_or_literals() {
    let first = "#define FIRST R\"tag(/* literal */)tag\"\n";
    let excluded = "#define EXCLUDED \"hidden\"\n";
    let second = "#define SECOND \"second\"\nint after(void) { return 0; }\n";
    let source = format!("{first}{excluded}{second}");
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(CPP, source.as_bytes());
    let spans = [0..first.len(), (first.len() + excluded.len())..source.len()];
    let ranges = spans
        .map(|span| {
            IncludedRange::new(
                SourceSpan::new(
                    fixture.source.span().file(),
                    u64::try_from(span.start).unwrap(),
                    u64::try_from(span.end).unwrap(),
                )
                .unwrap(),
                language(CPP),
            )
        })
        .to_vec();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(CPP),
        EncodingId::utf8(),
        ranges,
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = checked(&input, &provider, CPP);
    assert_source(&result, source.as_bytes(), CPP);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|row| row.canonical_name == "EXCLUDED")
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|row| row.canonical_name == "SECOND")
    );
    let strings = result
        .document()
        .occurrences
        .iter()
        .filter(|row| row.role == OccurrenceRole::StringEvidence)
        .collect::<Vec<_>>();
    assert_eq!(strings.len(), 2);
    assert!(
        !strings
            .iter()
            .any(|row| raw_text(source.as_bytes(), &row.source) == b"\"hidden\"")
    );
}

#[test]
fn cpp_projection_work_cache_memory_and_cancellation_remain_admitted() {
    use rootlight_adapter_treesitter::{ReuseInvalidation, ReuseStatus, SourceEdit};
    let clean = "#define VALUE 1'000 \"tail\"\nint after(void) { return 0; }\n";
    let mut native = tree_sitter::Parser::new();
    native
        .set_language(&tree_sitter_cpp::LANGUAGE.into())
        .unwrap();
    let nodes = native
        .parse(clean, None)
        .unwrap()
        .root_node()
        .descendant_count();
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CPP, clean.as_bytes());
    let base = limits();
    for maximum in [2 * nodes - 1, 2 * nodes] {
        let budget = AnalysisLimits::new(
            base.max_source_bytes(),
            maximum,
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            base.ir().clone(),
        )
        .unwrap();
        let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
        let parsed = provider
            .execute_with_previous(
                &input.to_parse_request(),
                None,
                &[],
                ParserSettings::new(4096).unwrap(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(parsed.report().resources().syntax_nodes(), maximum);
        if maximum < 2 * nodes {
            assert!(parsed.output().facts().is_empty());
            assert!(parsed.previous().is_none());
            assert!(
                parsed
                    .output()
                    .diagnostics()
                    .iter()
                    .any(|row| row.code().as_str() == "syntax-node-limit")
            );
        } else {
            let previous = parsed.previous().unwrap();
            let repeated = provider
                .execute_with_previous(
                    &input.to_parse_request(),
                    Some(previous),
                    &[],
                    ParserSettings::new(4096).unwrap(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            assert!(matches!(
                repeated.reuse_status(),
                ReuseStatus::Reused { .. }
            ));
            let offset = clean.find(" \"tail\"").unwrap();
            let inserted = " /* comment */";
            let changed = format!("{}{inserted}{}", &clean[..offset], &clean[offset..]);
            let rewritten = fixture.rewrite(changed.as_bytes());
            let next = request(&rewritten.snapshot, &rewritten.source, CPP, &base);
            let projected = provider
                .execute_with_previous(
                    &next.to_parse_request(),
                    Some(previous),
                    &[SourceEdit::new(offset, offset, inserted).unwrap()],
                    ParserSettings::new(4096).unwrap(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                projected.reuse_status(),
                ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute)
            );
            assert!(projected.previous().is_none());
        }
    }
    let input = request(&fixture.snapshot, &fixture.source, CPP, &base);
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(
        provider
            .required_syntax_fact_count(&input.to_parse_request(), &cancellation)
            .is_err()
    );
    let tiny = AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        1,
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        base.ir().clone(),
    )
    .unwrap();
    let short = request(&fixture.snapshot, &fixture.source, CPP, &tiny);
    assert!(
        provider
            .required_syntax_fact_count(&short.to_parse_request(), &Cancellation::new())
            .is_err()
    );
}

#[test]
fn cpp_unavailable_projection_publishes_four_exact_gap_domains_at_the_real_quota() {
    let raw = b"#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n#define ESCAPED\\\n /* comment */ \"tail\"\nint after(void) { return 0; }\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CPP, raw);
    let base = limits();
    for maximum in [3, 4] {
        let mut ir = base.ir().clone();
        ir.max_skipped_regions = maximum;
        let budget = AnalysisLimits::new(
            base.max_source_bytes(),
            base.max_syntax_nodes(),
            base.max_syntax_depth(),
            base.max_embedded_ranges(),
            base.max_reported_memory_bytes(),
            base.syntax_stream().clone(),
            base.ir_stream().clone(),
            ir,
        )
        .unwrap();
        let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
        let result = analyzer(&provider, CPP).analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        );
        if maximum == 3 {
            assert!(matches!(
                result.unwrap_err(),
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::Records,
                    observed: 4,
                    limit: 3
                })
            ));
        } else {
            let (output, _) = result.unwrap();
            assert_eq!(output.document().skipped_regions.len(), 4);
            assert_source(&output, raw, CPP);
        }
    }
}

#[test]
fn cpp_macro_decoys_stay_opaque_and_unproved_directives_are_explicitly_unavailable() {
    let source = "/* #define COMMENTED 1 */\nconst char* ordinary = \"#define QUOTED 2\";\nconst char* raw = R\"tag(\n#define RAW_DECOY 3\n)tag\";\nint after(void) { return 0; }\n";
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(CPP, source.as_bytes());
    let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
    let result = checked(&input, &provider, CPP);
    assert_source(&result, source.as_bytes(), CPP);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|row| row.canonical_name == "after")
    );
    assert!(!result.document().entities.iter().any(|row| matches!(
        row.canonical_name.as_str(),
        "COMMENTED" | "QUOTED" | "RAW_DECOY"
    )));
    for source in [
        "#/**/define UNPROVED 1\nint after(void) { return 0; }\n",
        "#defineD 1\nint after(void) { return 0; }\n",
    ] {
        let fixture = Fixture::new(CPP, source.as_bytes());
        let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
        let result = checked(&input, &provider, CPP);
        assert_source(&result, source.as_bytes(), CPP);
        assert!(result.document().entities.is_empty());
        assert_eq!(result.document().skipped_regions.len(), 4);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .all(
                    |row| row.detail == "cpp-preprocessor-replacement-not-analyzed"
                        && raw_text(source.as_bytes(), &row.source) == source.as_bytes()
                )
        );
    }
}

#[test]
fn cpp_conditional_selection_stays_unresolved_with_both_written_branch_owners() {
    let source = "#if SELECTED\n#define LEFT \"left\"\nint written_left(void) { return 0; }\n#else\n#define RIGHT \"right\"\nint written_right(void) { return 0; }\n#endif\n";
    let provider = Arc::new(provider());
    let budget = limits();
    let fixture = Fixture::new(CPP, source.as_bytes());
    let input = request(&fixture.snapshot, &fixture.source, CPP, &budget);
    let output = checked(&input, &provider, CPP);
    assert_source(&output, source.as_bytes(), CPP);
    for name in ["LEFT", "RIGHT"] {
        let entity = output
            .document()
            .entities
            .iter()
            .find(|row| row.language == "cpp" && row.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Constant);
        let headers = signatures(&output, source.as_bytes(), entity.id);
        assert_eq!(headers.len(), 1);
        assert!(headers[0].starts_with(&format!("#define {name}")));
        assert!(!headers[0].contains("#if"));
    }
    for name in ["written_left", "written_right"] {
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|row| row.language == "cpp"
                    && row.kind == EntityKind::Function
                    && row.canonical_name == name)
        );
    }
    let gaps = output
        .document()
        .skipped_regions
        .iter()
        .filter(|row| row.detail == "cpp-preprocessor-conditional-evaluation-unavailable")
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 3);
    assert_eq!(
        gaps.iter().map(|row| row.domain).collect::<BTreeSet<_>>(),
        [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations
        ]
        .into_iter()
        .collect()
    );
    assert!(
        gaps.iter()
            .all(|row| raw_text(source.as_bytes(), &row.source)
                == source.trim_end_matches('\n').as_bytes())
    );
    assert_ne!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
}
