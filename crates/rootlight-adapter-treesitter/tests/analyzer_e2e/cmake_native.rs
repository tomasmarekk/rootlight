//! Written CMake owners and references stay bound to physical source bytes.
//! These contracts distinguish callable headers, macro placeholders and variable
//! namespaces without treating source order or spelling as evaluated bindings.

use super::*;

const CMAKE: LanguageCase = LanguageCase {
    name: "cmake",
    path: "build/Written.cmake",
    frontend: "tree-sitter-cmake-0.7.5",
    source: "# source marker λ😀\r\nFuNcTiOn(Written first second)\r\n set(local \"${first}\")\r\n message(\"${second}\")\r\nEnDfUnCtIoN()\r\nmacro(Forward arg)\r\n Written(\"${arg}\" value)\r\nendmacro()\r\nset(normal normal_value)\r\nset(cache cached CACHE STRING \"doc\")\r\nset(ENV{ENV_NAME} env)\r\nset(CACHE{new_cache} TYPE STRING VALUE cached)\r\nadd_library(written_target INTERFACE)\r\nWritten(hello world)\r\ninclude(literal/module.cmake)\r\nmessage(\"${normal} $CACHE{cache} $ENV{ENV_NAME}\")\r\nmessage([=[${bracket_literal}]=])\r\n",
    generated: false,
    body_before: "normal_value",
    body_after: "changed_value",
};

const CMAKE_HEADER: LanguageCase = LanguageCase {
    path: "include/config.h.cmake",
    ..CMAKE
};

fn header_output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CMAKE_HEADER, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, CMAKE_HEADER),
        &request(&fixture.snapshot, &fixture.source, CMAKE_HEADER, &budget),
        &ExtensionSupport::default(),
    );
    assert_physical(&result, source);
    result
}

#[test]
fn c_preprocessor_headers_keep_five_written_signatures_and_three_literal_values() {
    let source = concat!(
        "#ifndef FIRST_GUARD\n#define FIRST_GUARD\n",
        "#define WRITTEN_PACKAGE \"written-package\"\n",
        "#define WRITTEN_NAME \"Written Package\"\n",
        "#define WRITTEN_URL \"https://example.test/project\"\n",
        "#define SECOND_GUARD\n",
        "#cmakedefine CONDITIONAL \"@CONDITIONAL@\"\n#endif\n",
    );
    let result = header_output(source);
    for header in [
        "#define FIRST_GUARD\n",
        "#define WRITTEN_PACKAGE \"written-package\"\n",
        "#define WRITTEN_NAME \"Written Package\"\n",
        "#define WRITTEN_URL \"https://example.test/project\"\n",
        "#define SECOND_GUARD\n",
    ] {
        let name = header.split_ascii_whitespace().nth(1).unwrap();
        let entities = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.language == "c" && entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "{name}");
        assert_eq!(entities[0].kind, EntityKind::Constant);
        assert_eq!(
            signature(&result, source, entities[0].id),
            [header],
            "{name}"
        );
        assert!(result.document().relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.object == RelationEndpoint::Entity(entities[0].id)
                && result.document().entities.iter().any(|owner| {
                    owner.kind == EntityKind::Module
                        && owner.language == "c"
                        && relation.subject == RelationEndpoint::Entity(owner.id)
                })
        }));
    }
    let literals = string_evidence_in_source_order(&result, source);
    assert_eq!(
        literals,
        [
            "\"written-package\"",
            "\"Written Package\"",
            "\"https://example.test/project\""
        ]
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "CONDITIONAL")
    );
}

#[test]
fn c_preprocessor_literals_are_lexical_tokens_on_the_ordinary_c_route() {
    let source = concat!(
        "#define EMPTY_GUARD\n",
        "#define ORDINARY \"written\"\n",
        "#define WRAP(value) (\"left\" #value \"right\")\n",
        "#define CHARACTER '\"'\n",
        "#define COMMENT_ONLY /* \"not a literal\" */\n",
    );
    let case = LanguageCase {
        path: "include/written.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    assert_physical_language(&result, source, "c");
    for (name, kind) in [
        ("EMPTY_GUARD", EntityKind::Constant),
        ("ORDINARY", EntityKind::Constant),
        ("WRAP", EntityKind::Function),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.language == "c" && entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(signature(&result, source, entity.id).len(), 1, "{name}");
    }
    let literals = string_evidence_in_source_order(&result, source);
    assert_eq!(literals, ["\"written\"", "\"left\"", "\"right\""]);
}

#[test]
fn c_preprocessor_numeric_separators_do_not_hide_following_authored_strings() {
    let source = concat!(
        "#define DECIMAL 1'000\n",
        "#define FOLLOWED 1'000 \"written\"\n",
        "#define HEXADECIMAL 0xDEAD'BEEF\n",
        "#define BINARY 0b10'01\n",
        "#define FLOATING .1'25e+2 \"float\"\n",
        "#define CONTINUED 1'\\\n000 \"continued\"\n",
        "#define CHARACTER 'x'\n",
        "#define COMMENTED 1'000 /* \"hidden\" */ \"tail\"\n",
        "#define UNFINISHED 'unfinished\n",
    );
    let case = LanguageCase {
        path: "include/numeric.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    assert_physical_language(&result, source, "c");
    let literals = string_evidence_in_source_order(&result, source);
    assert_eq!(
        literals,
        ["\"written\"", "\"float\"", "\"continued\"", "\"tail\""]
    );
    let gaps = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "c-preprocessor-replacement-lexical-unavailable")
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 1);
    assert_eq!(text(source, &gaps[0].source), "'unfinished\n");
}

#[test]
fn cmake_header_templates_retain_authored_c_owners_and_template_gaps() {
    let source = concat!(
        "/* header marker λ😀 */\r\n",
        "#ifndef WRITTEN_CONFIG_H\r\n#define WRITTEN_CONFIG_H\r\n",
        "#cmakedefine FEATURE @VALUE@\r\n",
        "struct Before { int member; };\r\n",
        "#cmakedefine01 SWITCH\r\n",
        "#define WRITTEN_VALUE @VALUE@\r\n",
        "struct Generated_@NAME@ { int hidden; };\r\n",
        "int written_function(struct Before *value);\r\n",
        "struct After { int member; };\r\n#endif\r\n",
    );
    let result = header_output(source);
    let document = result.document();
    for name in ["Before", "After", "written_function"] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing authored C entity {name}"));
        assert_eq!(entity.language, "c");
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        assert_eq!(text(source, &definition.source), name);
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.object == RelationEndpoint::Entity(entity.id)
                && document.entities.iter().any(|owner| {
                    owner.kind == EntityKind::Module
                        && owner.language == "c"
                        && relation.subject == RelationEndpoint::Entity(owner.id)
                })
        }));
    }
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Module && entity.language == "cmake")
    );
    let value = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "WRITTEN_VALUE")
        .expect("written macro name remains available despite templated value");
    assert_eq!(value.kind, EntityKind::Constant);
    assert_eq!(value.language, "c");
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Field
                && entity.canonical_name == "member"
                && entity.language == "c")
    );
    assert!(
        document
            .entities
            .iter()
            .all(|entity| !entity.canonical_name.starts_with("Generated_")
                && !matches!(entity.canonical_name.as_str(), "FEATURE" | "SWITCH"))
    );
    for selected in [
        "#cmakedefine FEATURE @VALUE@",
        "#cmakedefine01 SWITCH",
        "@NAME@",
        "@VALUE@",
    ] {
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.detail == "cmake-header-template-expansion-unavailable"
                    && text(source, &gap.source) == selected
            }),
            "missing template gap {selected}"
        );
    }
}

#[test]
fn cmake_header_named_scripts_do_not_become_secondary_c() {
    for source in [
        "# #cmakedefine COMMENT\nset(written value)\n",
        "#define COMMENT\nset(written value);\n",
        "#cmakedefine FEATURE\nset(written value);\n",
        "#cmakedefine FEATURE \"unterminated\nset(written value)\n",
        "int(value)\nset(written value)\n",
        "struct(value)\nset(written value)\n",
        "set(\n text\n struct\n)\n#cmakedefine COMMENT\nset(written value)\n",
        "set(text [=[\n#cmakedefine FAKE\nstruct Pretend { int member; };\n]=])\nset(written value)\n",
        "set(text \"multiline\n#cmakedefine FAKE\nstruct Pretend { int member; };\n\")\nset(written value)\n",
        "message(\"#cmakedefine STRING @VALUE@\")\nset(written value)\n",
        "#[=[\nstruct Pretend { int member; };\n#cmakedefine COMMENT\n]=]\nset(written value)\n",
    ] {
        let result = header_output(source);
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "written" && entity.language == "cmake")
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.language != "c")
        );
    }
}

#[test]
fn cmake_header_shape_proof_precedes_marker_resource_admission() {
    let source = format!(
        "# {}\nset(written value)\n",
        "@VALUE@".repeat(MAX_SYNTAX_NODES + 1)
    );
    let result = header_output(&source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "cmake" && entity.canonical_name == "written")
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| entity.language != "c")
    );
}

#[test]
fn cmake_header_continued_directives_do_not_promote_macro_replacement_declarations() {
    let source = concat!(
        "#cmakedefine FEATURE \\\n",
        "struct Hidden { int member; };\n",
        "#define CALL() \\\n",
        " set(value)\n",
        "struct Visible { int member; };\n"
    );
    let result = header_output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "c" && entity.canonical_name == "Visible")
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "Hidden" | "set" | "value"))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.detail == "cmake-header-template-expansion-unavailable"
                    && text(source, &gap.source)
                        == "#cmakedefine FEATURE \\\nstruct Hidden { int member; };"
            )
    );
}

#[test]
fn cmake_header_plain_email_literals_are_not_incomplete_substitutions() {
    let source = "#cmakedefine FEATURE\n/* user@example.test */\nconst char *email = \"user@example.test\";\n#define URL \"user@example.test\"\nstruct Written {};\n";
    let result = header_output(source);
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "c" && entity.canonical_name == "Written")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "cmake-header-template-expansion-unavailable")
            .all(|gap| text(source, &gap.source) == "#cmakedefine FEATURE")
    );
}

#[test]
fn cmake_header_preprocessor_comments_keep_native_c_owners() {
    let source = "#define VALUE /* documentation\nset(value)\n*/\n#define OTHER 1\n";
    let result = header_output(source);
    let document = result.document();
    for name in ["VALUE", "OTHER"] {
        assert!(document.entities.iter().any(|entity| entity.language == "c"
            && entity.kind == EntityKind::Constant
            && entity.canonical_name == name));
    }
    assert!(
        document
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "set" | "value"))
    );
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Documentation
                && text(source, &site.source) == "/* documentation\nset(value)\n*/")
    );
}

#[test]
fn cmake_header_all_configure_forms_keep_complete_physical_gaps() {
    // configure_file supports these substitutions without an observed @ONLY mode.
    // Names and nested references match its source-backed transformation contract.
    let source = concat!(
        "#cmakedefine FEATURE\n",
        "struct Before { int member; };\n",
        "struct Generated_@NAME@ { int hidden_at; };\n",
        "struct Generated_${NAME} { int hidden_var; };\n",
        "struct Generated_$CACHE{NAME} { int hidden_cache; };\n",
        "struct Generated_$ENV{NAME} { int hidden_env; };\n",
        "struct Generated_${PREFIX_${INNER}} { int hidden_nested; };\n",
        "struct Generated_@DOT.NAME@ { int hidden_dot; };\n",
        "struct Generated_@WITH-DASH@ { int hidden_dash; };\n",
        "struct Generated_@SLASH/NAME@ { int hidden_slash; };\n",
        "struct Generated_@PLUS+NAME@ { int hidden_plus; };\n",
        "#define WRITTEN_VALUE ${VALUE}\n",
        "#define Generated_${NAME} 1\n",
        "/* @COMMENT.NAME@ ${COMMENT} $CACHE{COMMENT} $ENV{COMMENT} */\n",
        "const char *literal = \"@STRING.NAME@ ${STRING} $CACHE{STRING} $ENV{STRING}\";\n",
        "struct After { int member; };\n",
    );
    let result = header_output(source);
    let document = result.document();
    for name in ["Before", "After", "WRITTEN_VALUE", "literal"] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.language == "c" && entity.canonical_name == name),
            "{name}"
        );
    }
    assert!(
        document
            .entities
            .iter()
            .all(|entity| !entity.canonical_name.starts_with("Generated_")
                && !entity.canonical_name.starts_with("hidden_"))
    );
    for selected in [
        "@NAME@",
        "${NAME}",
        "$CACHE{NAME}",
        "$ENV{NAME}",
        "${PREFIX_${INNER}}",
        "@DOT.NAME@",
        "@WITH-DASH@",
        "@SLASH/NAME@",
        "@PLUS+NAME@",
        "${VALUE}",
        "@COMMENT.NAME@",
        "${COMMENT}",
        "$CACHE{COMMENT}",
        "$ENV{COMMENT}",
        "@STRING.NAME@",
        "${STRING}",
        "$CACHE{STRING}",
        "$ENV{STRING}",
    ] {
        assert!(
            document.skipped_regions.iter().any(|gap| gap.detail
                == "cmake-header-template-expansion-unavailable"
                && text(source, &gap.source) == selected),
            "missing exact gap {selected}"
        );
    }
    for (role, selected) in [
        (
            OccurrenceRole::Documentation,
            "/* @COMMENT.NAME@ ${COMMENT} $CACHE{COMMENT} $ENV{COMMENT} */",
        ),
        (
            OccurrenceRole::StringEvidence,
            "\"@STRING.NAME@ ${STRING} $CACHE{STRING} $ENV{STRING}\"",
        ),
    ] {
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == role && text(source, &site.source) == selected),
            "missing literal {selected}"
        );
    }
}

#[test]
fn cmake_header_typed_names_survive_ambiguous_top_level_invocations() {
    let source = "#cmakedefine FEATURE\nDECLARE(generated);\nint set(int value);\nint (parenthesized);\nstruct Written { int member; };\n";
    let result = header_output(source);
    for name in ["set", "Written", "parenthesized"] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.language == "c")
        );
    }
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| !matches!(entity.canonical_name.as_str(), "DECLARE" | "generated"))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.detail == "cmake-header-top-level-invocation-analysis-unavailable"
                    && text(source, &gap.source) == "DECLARE(generated);"
            )
    );
}

#[test]
fn cmake_header_literal_markers_and_malformed_substitutions_preserve_neighbors() {
    let source = concat!(
        "#cmakedefine FEATURE\n",
        "/* #cmakedefine FAKE @COMMENT@ */\n",
        "const char *literal = \"#cmakedefine FAKE \\\" @STRING@\";\n",
        "const char *continued = \"first\\\r\nset(value) ${CONTINUED}\";\n",
        "// continued C comment\\\nset(value) $ENV{CONTINUED_COMMENT}\n",
        "// CRLF C comment\\\r\nset(value) @CRLF_COMMENT@\r\n",
        "struct Before { int member; };\n",
        "int @UNCLOSED;\n",
        "int ${UNCLOSED;\n",
        "#define WRITTEN_BAD $CACHE{UNCLOSED\n",
        "int $ENV{UNCLOSED;\n",
        "struct After { int member; };\n",
    );
    let result = header_output(source);
    for name in ["Before", "After"] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.language == "c")
        );
    }
    for (role, selected) in [
        (
            OccurrenceRole::Documentation,
            "/* #cmakedefine FAKE @COMMENT@ */",
        ),
        (
            OccurrenceRole::StringEvidence,
            "\"#cmakedefine FAKE \\\" @STRING@\"",
        ),
        (
            OccurrenceRole::StringEvidence,
            "\"first\\\r\nset(value) ${CONTINUED}\"",
        ),
        (
            OccurrenceRole::Documentation,
            "// continued C comment\\\nset(value) $ENV{CONTINUED_COMMENT}",
        ),
        (
            OccurrenceRole::Documentation,
            "// CRLF C comment\\\r\nset(value) @CRLF_COMMENT@\r",
        ),
    ] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == role && text(source, &site.source) == selected),
            "missing literal {selected}"
        );
    }
    for selected in [
        "@COMMENT@",
        "@STRING@",
        "@UNCLOSED",
        "${UNCLOSED;",
        "$CACHE{UNCLOSED",
        "$ENV{UNCLOSED;",
        "${CONTINUED}",
        "$ENV{CONTINUED_COMMENT}",
        "@CRLF_COMMENT@",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(
                    |gap| gap.detail == "cmake-header-template-expansion-unavailable"
                        && text(source, &gap.source) == selected
                ),
            "missing gap {selected}"
        );
    }
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| !entity.canonical_name.contains("FAKE"))
    );
}

#[test]
fn cmake_header_preflight_replay_and_cancellation_share_the_same_projection() {
    let source = "#cmakedefine FEATURE @VALUE@\n#define WRITTEN_TEXT \"written\"\n#define UNFINISHED \"unfinished\nstruct Written { int member; };\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CMAKE_HEADER);
    let fixture = Fixture::new(CMAKE_HEADER, source.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, CMAKE_HEADER, &budget);
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
        .unwrap();
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        required,
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap()
    );
    assert!(
        fresh
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Written" && entity.language == "c")
    );
    assert!(fresh.document().skipped_regions.iter().any(|gap| {
        gap.detail == "c-preprocessor-replacement-lexical-unavailable"
            && text(source, &gap.source) == "\"unfinished\n"
    }));
    let literals = fresh
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::StringEvidence)
        .map(|site| text(source, &site.source))
        .collect::<Vec<_>>();
    assert_eq!(literals, ["\"written\""]);
    let replay = analyzer
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(
        provider
            .required_syntax_fact_count(&input.to_parse_request(), &cancellation)
            .is_err()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn cmake_header_utf16_and_utf8_boms_keep_c_definitions_on_original_bytes() {
    let source = "/* λ😀 */\r\n#cmakedefine FEATURE @VALUE@\r\nstruct Written { int member; };\r\n#define WRITTEN_TEXT L\"λ😀\"\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut bytes = match encoding {
            "utf-8" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            _ => vec![0xfe, 0xff],
        };
        if encoding == "utf-8" {
            bytes.extend_from_slice(source.as_bytes());
        } else {
            for unit in source.encode_utf16() {
                bytes.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(CMAKE_HEADER, &bytes);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(CMAKE_HEADER),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(CMAKE_HEADER.generated);
        let result = analyze(
            &analyzer(&provider, CMAKE_HEADER),
            &input,
            &ExtensionSupport::default(),
        );
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Written" && entity.language == "c")
            .unwrap();
        let definition = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let start = usize::try_from(definition.source.span().start_byte()).unwrap();
        let end = usize::try_from(definition.source.span().end_byte()).unwrap();
        let expected = if encoding == "utf-8" {
            b"Written".to_vec()
        } else {
            "Written"
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        };
        assert_eq!(&bytes[start..end], expected);
        assert_eq!(definition.source.content_hash(), content_hash(&bytes));
        assert_eq!(
            result.document().files[0].content_hash,
            content_hash(&bytes)
        );
        assert_eq!(result.document().files[0].encoding, encoding);
        let literal = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::StringEvidence)
            .unwrap();
        let expected = if encoding == "utf-8" {
            "L\"λ😀\"".as_bytes().to_vec()
        } else {
            "L\"λ😀\""
                .encode_utf16()
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect()
        };
        let start = usize::try_from(literal.source.span().start_byte()).unwrap();
        let end = usize::try_from(literal.source.span().end_byte()).unwrap();
        assert_eq!(&bytes[start..end], expected, "{encoding}");
        assert_eq!(literal.source.content_hash(), content_hash(&bytes));
    }
}

#[test]
fn cmake_header_work_refusal_releases_parser_and_same_provider_recovers_c_language() {
    let source = "#cmakedefine FEATURE\nstruct Written { int member; };\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CMAKE_HEADER, source.as_bytes());
    let defaults = limits();
    let bounded = AnalysisLimits::new(
        defaults.max_source_bytes(),
        1,
        defaults.max_syntax_depth(),
        defaults.max_embedded_ranges(),
        defaults.max_reported_memory_bytes(),
        defaults.syntax_stream().clone(),
        defaults.ir_stream().clone(),
        defaults.ir().clone(),
    )
    .unwrap();
    let input = request(&fixture.snapshot, &fixture.source, CMAKE_HEADER, &bounded);
    let analyzer = analyzer(&provider, CMAKE_HEADER);
    let error = execute_analysis(
        &analyzer,
        &input,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect_err("one native work unit cannot commit a partial header document");
    assert!(matches!(error, AdapterError::ProviderFailed { code }
        if code.as_str() == "syntax-node-parse-work-limit"));
    assert_eq!(provider.stats().checked_out_parsers, 0);

    let recovered = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, CMAKE_HEADER, &defaults),
        &ExtensionSupport::default(),
    );
    // The requested template remains the host file; its written C body has
    // independent language evidence below.
    assert_physical(&recovered, source);
    let document = recovered.document();
    assert!(
        document
            .entities
            .iter()
            .any(|entity| { entity.kind == EntityKind::Module && entity.language == "c" })
    );
    let written: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "Written" && entity.language == "c")
        .collect();
    assert_eq!(written.len(), 1);
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target
                    == (OccurrenceTarget::Resolved {
                        symbol: written[0].id,
                    })
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    assert_eq!(text(source, &definitions[0].source), "Written");
    assert_eq!(
        definitions[0].source.content_hash(),
        fixture.source.content_hash()
    );
    assert_eq!(
        definitions[0].source.generation(),
        fixture.source.generation()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn cmake_header_previous_handles_bind_the_actual_path_projection_route() {
    use rootlight_adapter_treesitter::{ReuseInvalidation, ReuseStatus};

    let source = "#cmakedefine FEATURE\nstruct Written { int member; };\n";
    let header_peer = LanguageCase {
        path: "other/config.h.cmake",
        ..CMAKE_HEADER
    };
    let script_peer = LanguageCase {
        path: "other/script.cmake",
        ..CMAKE
    };
    let provider = provider();
    let budget = limits();
    let settings = ParserSettings::new(4096).unwrap();
    let memory = MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
    for (old_case, current_case, changed_route) in [
        (CMAKE, CMAKE_HEADER, true),
        (CMAKE_HEADER, CMAKE, true),
        (CMAKE_HEADER, header_peer, false),
        (CMAKE, script_peer, false),
    ] {
        let old = Fixture::new(old_case, source.as_bytes());
        let old_input = request(&old.snapshot, &old.source, old_case, &budget);
        let first = provider
            .execute_with_previous(
                &old_input.to_parse_request(),
                None,
                &[],
                settings,
                memory,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(first.previous().is_some());
        let current = Fixture::new(current_case, source.as_bytes());
        let current_input = request(&current.snapshot, &current.source, current_case, &budget);
        let incremental = provider
            .execute_with_previous(
                &current_input.to_parse_request(),
                first.previous(),
                &[],
                settings,
                memory,
                &Cancellation::new(),
            )
            .unwrap();
        let fresh = provider
            .execute_with_previous(
                &current_input.to_parse_request(),
                None,
                &[],
                settings,
                memory,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            incremental.output().facts(),
            fresh.output().facts(),
            "{} -> {}",
            old_case.path,
            current_case.path
        );
        assert_eq!(
            incremental.output().diagnostics(),
            fresh.output().diagnostics(),
            "{} -> {}",
            old_case.path,
            current_case.path
        );
        assert_eq!(
            incremental.output().report(),
            fresh.output().report(),
            "{} -> {}",
            old_case.path,
            current_case.path
        );
        if changed_route {
            assert_eq!(
                incremental.reuse_status(),
                ReuseStatus::Invalidated(ReuseInvalidation::FrontendRoute),
                "{} -> {}",
                old_case.path,
                current_case.path
            );
        } else {
            assert!(matches!(
                incremental.reuse_status(),
                ReuseStatus::Reused { .. }
            ));
        }
    }
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn cmake_empty_conditions_preserve_written_empty_and_hash_values() {
    let source = "if()\n set(first \"\")\nelseif()\n set(second \"${ROOT}/## ${package} license\")\nelse()\n set(third \"\")\nendif()\nset(after \"\")\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, header) in [
        ("first", "set(first \"\")"),
        ("second", "set(second \"${ROOT}/## ${package} license\")"),
        ("third", "set(third \"\")"),
        ("after", "set(after \"\")"),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Variable);
        assert_eq!(signature(&result, source, entity.id), [header]);
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cmake-control-flow-and-variable-analysis-unavailable")
    );
}

#[test]
fn cmake_configure_placeholders_keep_written_owners_without_evaluating_names() {
    let source = "@PACKAGE_INIT@\nfunction(Written out)\n set(local \"${z_@PORT@_root}/lib\")\nendfunction()\nfunction(z_@OWNER@_written out)\n set(nested value)\nendfunction()\nset(after \"\")\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let names = document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
        .map(|entity| entity.canonical_name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from(["Written", "out", "local", "nested", "after"])
    );
    let written = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Written")
        .unwrap();
    let local = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "local")
        .unwrap();
    assert_eq!(
        signature(&result, source, local.id),
        ["set(local \"${z_@PORT@_root}/lib\")"]
    );
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(written.id)
                && relation.object == RelationEndpoint::Entity(local.id))
    );
    for placeholder in ["@PACKAGE_INIT@", "@PORT@"] {
        assert!(
            document.skipped_regions.iter().any(|gap| gap.detail
                == "cmake-configure-substitution-and-generated-syntax-unavailable"
                && text(source, &gap.source) == placeholder),
            "{placeholder}"
        );
    }
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "cmake-declaration-name-analysis-unavailable"
        && text(source, &gap.source).starts_with("function(z_@OWNER@_written")));
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role == OccurrenceRole::Definition
                || matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn cmake_malformed_placeholders_and_empty_while_remain_explicit_parse_gaps() {
    for source in [
        "while()\n set(written value)\nendwhile()\n",
        "@UNCLOSED\nset(written value)\n",
        "set(written \"${z_@UNCLOSED_root}/lib\")\n",
    ] {
        let result = output(source);
        assert!(!result.document().diagnostics.is_empty(), "{source}");
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "syntax-error-recovery"),
            "{source}"
        );
    }
}

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
    let fixture = Fixture::new(CMAKE, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, CMAKE),
        &request(&fixture.snapshot, &fixture.source, CMAKE, &budget),
        &ExtensionSupport::default(),
    );
    assert_physical(&result, source);
    result
}

fn string_evidence_in_source_order<'a>(result: &AnalysisOutput, source: &'a str) -> Vec<&'a str> {
    let mut sites = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::StringEvidence)
        .collect::<Vec<_>>();
    // Normalized IR orders facts by identity; these fixtures compare physical tokens.
    sites.sort_by_key(|site| {
        (
            site.source.span().start_byte(),
            site.source.span().end_byte(),
        )
    });
    sites
        .into_iter()
        .map(|site| {
            assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
            text(source, &site.source)
        })
        .collect()
}

fn assert_physical(result: &AnalysisOutput, source: &str) {
    assert_physical_language(result, source, "cmake");
}

fn assert_physical_language(result: &AnalysisOutput, source: &str, expected_language: &str) {
    let document = result.document();
    validate_ir_document(document, limits().ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, expected_language);
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
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.evidence.source.as_ref()),
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
        if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    for coverage in &document.coverage_records {
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    if expected_language == "cmake" {
        // CMake fixtures require explicit evaluation gaps; C fixtures assert
        // their own lexical gaps without assuming CMake coverage semantics.
        assert_ne!(
            result.report().coverage().status(),
            CoverageStatus::Complete
        );
    }
}

fn signature(result: &AnalysisOutput, source: &str, symbol: SymbolId) -> Vec<String> {
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(symbol))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical
                    .verify_source_bytes(
                        text(source, extension.evidence.source.as_ref().unwrap()).as_bytes(),
                    )
                    .unwrap();
                lexical.text().to_owned()
            })
        })
        .collect()
}

#[test]
fn cmake_written_definitions_headers_parameters_assignments_and_contains_are_exact() {
    let result = output(CMAKE.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let expected = [
        ("Written", EntityKind::Function),
        ("Forward", EntityKind::Function),
        ("first", EntityKind::Parameter),
        ("second", EntityKind::Parameter),
        ("arg", EntityKind::Parameter),
        ("local", EntityKind::Variable),
        ("normal", EntityKind::Variable),
        ("cache", EntityKind::Variable),
        ("ENV{ENV_NAME}", EntityKind::Variable),
        ("CACHE{new_cache}", EntityKind::Variable),
        ("written_target", EntityKind::BuildTarget),
    ];
    assert_eq!(document.entities.len(), expected.len() + 1);
    let names = document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
        .map(|entity| (entity.canonical_name.as_str(), entity.kind))
        .collect::<BTreeSet<_>>();
    assert_eq!(names, expected.into_iter().collect());
    for entity in document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
    {
        let definitions = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{}", entity.canonical_name);
        assert_eq!(
            text(CMAKE.source, &definitions[0].source),
            entity.canonical_name
        );
        assert_eq!(
            signature(&result, CMAKE.source, entity.id).len(),
            1,
            "{}",
            entity.canonical_name
        );
        let parent = document
            .relations
            .iter()
            .find(|relation| relation.object == RelationEndpoint::Entity(entity.id))
            .unwrap();
        let RelationEndpoint::Entity(parent) = parent.subject else {
            panic!("written owner");
        };
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.id == parent)
            .unwrap();
        match entity.canonical_name.as_str() {
            "first" | "second" | "local" => assert_eq!(parent.canonical_name, "Written"),
            "arg" => assert_eq!(parent.canonical_name, "Forward"),
            _ => assert_eq!(parent.kind, EntityKind::Module),
        }
    }
    let written = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Written")
        .unwrap();
    assert_eq!(
        signature(&result, CMAKE.source, written.id),
        vec!["FuNcTiOn(Written first second)"]
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cmake-macro-expansion-and-placeholder-binding-unavailable")
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cmake-effective-target-configuration-unavailable")
    );
}

#[test]
fn cmake_body_gap_markers_do_not_replace_written_callable_headers() {
    let source = concat!(
        "function(Written arg)\n",
        " if(arg)\n  return()\n endif()\n",
        " set(${dynamic} value)\n",
        " unset(removed)\n",
        " cmake_language(EVAL CODE \"message(value)\")\n",
        " option(incomplete)\n",
        "endfunction()\n",
        "macro(Forward item)\n",
        " foreach(value IN LISTS item)\n  message(${value})\n endforeach()\n",
        " configure_file(input output)\n",
        "endmacro()\n",
    );
    let result = output(source);
    for (name, expected) in [
        ("Written", "function(Written arg)"),
        ("Forward", "macro(Forward item)"),
        ("arg", "arg"),
        ("item", "item"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(signature(&result, source, entity.id), [expected], "{name}");
    }
    for expected in [
        "cmake-control-flow-and-variable-analysis-unavailable",
        "cmake-declaration-name-analysis-unavailable",
        "cmake-unset-variable-write-unavailable",
        "cmake-generated-source-and-dynamic-command-evaluation-unavailable",
        "cmake-option-written-argument-shape-unavailable",
        "cmake-macro-expansion-and-placeholder-binding-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == expected),
            "{expected}"
        );
    }
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "signature-capture-unavailable")
    );
}

#[test]
fn cmake_long_written_headers_keep_bounded_text_and_complete_source_hashes() {
    let source = format!(
        "add_library(large STATIC\n{}tail.cpp)\n",
        "  source.cpp\n".repeat(400)
    );
    let changed = source.replace("tail.cpp", "changed.cpp");
    let mut symbols = Vec::new();
    for source in [&source, &changed] {
        let result = output(source);
        let target = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "large")
            .unwrap();
        symbols.push(target.id);
        let headers = result
            .document()
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
                (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(target.id))
                .then_some((extension, lexical))
            })
            .collect::<Vec<_>>();
        assert_eq!(headers.len(), 1);
        let (extension, lexical) = &headers[0];
        let header = text(source, extension.evidence.source.as_ref().unwrap());
        assert_eq!(header, source.trim_end());
        assert!(header.len() > rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES);
        assert!(lexical.is_truncated());
        assert_eq!(
            lexical.complete_text_hash(),
            content_hash(header.as_bytes())
        );
        assert_eq!(
            lexical.text().len(),
            rootlight_ir::MAX_LEXICAL_SIGNATURE_BYTES
        );
        assert!(header.starts_with(lexical.text()));
        lexical.verify_source_bytes(header.as_bytes()).unwrap();
        assert!(
            !result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "signature-capture-unavailable")
        );
    }
    assert_eq!(symbols[0], symbols[1]);
}

#[test]
fn cmake_written_calls_imports_and_variable_namespaces_never_claim_binding() {
    let result = output(CMAKE.source);
    let occurrences = &result.document().occurrences;
    for (label, expected) in [
        (
            "cmake.normal_variable.reference",
            vec!["first", "second", "arg", "normal"],
        ),
        ("cmake.cache_variable.reference", vec!["cache"]),
        ("cmake.environment_variable.reference", vec!["ENV_NAME"]),
        ("cmake.written_import.import", vec!["literal/module.cmake"]),
    ] {
        let mut selected = occurrences
            .iter()
            .filter(|occurrence| occurrence.syntax_kind == label)
            .collect::<Vec<_>>();
        selected.sort_unstable_by_key(|occurrence| occurrence.source.span().start_byte());
        assert_eq!(
            selected
                .into_iter()
                .map(|occurrence| text(CMAKE.source, &occurrence.source))
                .collect::<Vec<_>>(),
            expected,
            "{label}"
        );
    }
    assert_eq!(
        occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite
                && text(CMAKE.source, &occurrence.source) == "Written")
            .count(),
        2
    );
    assert!(
        !occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                && text(CMAKE.source, &occurrence.source) == "bracket_literal")
    );
}

#[test]
fn cmake_written_identity_survives_value_edits_and_repeated_names_stay_distinct() {
    let initial = output(CMAKE.source);
    let changed = output(&CMAKE.source.replace(CMAKE.body_before, CMAKE.body_after));
    let identities = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .map(|entity| (entity.canonical_name.clone(), entity.kind, entity.id))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&initial), identities(&changed));
    let repeated = output("set(written first)\nset(written second)\n");
    let symbols = repeated
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(symbols.len(), 2);
}

#[test]
fn cmake_option_names_headers_and_contains_describe_written_cache_candidates() {
    // The first header is retained from libuv; no effective cache value follows
    // from this source declaration or from the surrounding conditional.
    let source = "option(LIBUV_BUILD_SHARED \"Build shared lib\" ON)\r\nOPTION(\"quoted name\" \"help\")\r\noption([=[\r\nbracket_name]=] [=[literal ${help}]=] ${DEFAULT})\r\nif(CONDITION)\r\n option(conditional \"help\" OFF)\r\nendif()\r\nmessage(\"${LIBUV_BUILD_SHARED} $CACHE{LIBUV_BUILD_SHARED}\")\r\n";
    let result = output(source);
    let document = result.document();
    assert!(document.diagnostics.is_empty());
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    let expected = [
        (
            "LIBUV_BUILD_SHARED",
            "option(LIBUV_BUILD_SHARED \"Build shared lib\" ON)",
        ),
        ("quoted name", "OPTION(\"quoted name\" \"help\")"),
        (
            "bracket_name",
            "option([=[\r\nbracket_name]=] [=[literal ${help}]=] ${DEFAULT})",
        ),
        ("conditional", "option(conditional \"help\" OFF)"),
    ];
    assert_eq!(document.entities.len(), expected.len() + 1);
    for (name, header) in expected {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Variable);
        assert_eq!(signature(&result, source, entity.id), [header]);
        let definitions = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(text(source, &definitions[0].source), name);
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(module.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations
                && gap.detail == "cmake-option-initialization-and-effective-cache-state-unavailable"
                && text(source, &gap.source) == header
        }));
    }
    for label in [
        "cmake.normal_variable.reference",
        "cmake.cache_variable.reference",
    ] {
        assert!(document.occurrences.iter().any(|occurrence| {
            occurrence.syntax_kind == label
                && text(source, &occurrence.source) == "LIBUV_BUILD_SHARED"
                && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
        }));
    }
}

#[test]
fn cmake_option_identity_ignores_help_and_default_edits_but_not_written_owners() {
    let source = "option(Written \"original help\" ON)\noption(Written \"second help\" OFF)\n";
    let before = output(source);
    let after = output(
        &source
            .replace("original help", "replacement help")
            .replace("ON", "OFF"),
    );
    let ids = |result: &AnalysisOutput| {
        result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(&before).len(), 2);
    assert_eq!(ids(&before), ids(&after));
}

#[test]
fn cmake_option_unproven_names_and_argument_shapes_retain_exact_gaps() {
    for (source, detail) in [
        (
            "option(${NAME} \"help\" ON)\n",
            "cmake-declaration-name-analysis-unavailable",
        ),
        (
            "option(first;second \"help\")\n",
            "cmake-declaration-name-analysis-unavailable",
        ),
        (
            "option(@NAME@ \"help\")\n",
            "cmake-declaration-name-analysis-unavailable",
        ),
        (
            "option()\n",
            "cmake-option-written-argument-shape-unavailable",
        ),
        (
            "option(Written)\n",
            "cmake-option-written-argument-shape-unavailable",
        ),
        (
            "option(Written \"help\" ON extra)\n",
            "cmake-option-written-argument-shape-unavailable",
        ),
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty(), "{source}");
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module)
        );
        assert!(
            result.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Entities
                    && gap.detail == detail
                    && text(source, &gap.source) == source.trim_end()
            }),
            "{source}"
        );
    }
    let recovered = output("option(Written \"unfinished\n");
    assert!(!recovered.document().diagnostics.is_empty());
    assert!(
        recovered
            .document()
            .entities
            .iter()
            .all(|entity| entity.kind == EntityKind::Module)
    );
}

#[test]
fn cmake_computed_list_template_and_control_constructs_retain_specific_gaps() {
    for source in [
        "set(${NAME} value)\n",
        "set(@NAME@ value)\n",
        "set(first;second value)\n",
        "add_library(${TARGET} INTERFACE)\n",
        "function(${NAME} arg)\nendfunction()\n",
    ] {
        let result = output(source);
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && gap.detail == "cmake-declaration-name-analysis-unavailable"),
            "{source}"
        );
    }
    for (source, detail) in [
        (
            "function(Written ${ARG})\nendfunction()\n",
            "cmake-formal-argument-analysis-unavailable",
        ),
        (
            "if(CONDITION)\nset(value yes)\nendif()\n",
            "cmake-control-flow-and-variable-analysis-unavailable",
        ),
        ("unset(value)\n", "cmake-unset-variable-write-unavailable"),
        (
            "cmake_language(EVAL CODE \"set(value generated)\")\n",
            "cmake-generated-source-and-dynamic-command-evaluation-unavailable",
        ),
        (
            "configure_file(input.in output)\n",
            "cmake-generated-source-and-dynamic-command-evaluation-unavailable",
        ),
    ] {
        let result = output(source);
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "{source}: {:?}",
            result.document().skipped_regions
        );
    }
    let literal = output("set(\"first;second\" value)\nset([=[${literal}]=] value)\n");
    let names = literal
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .map(|entity| entity.canonical_name.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(names, ["first;second", "${literal}"].into_iter().collect());
}

#[test]
fn cmake_argument_recovery_keeps_written_calls_and_adjacent_declarations() {
    let source = concat!(
        "set(before value)\n",
        "set_target_properties(CppUnit PROPERTIES\n",
        "      IMPORTED_CONFIGURATIONS Debug\n",
        "      IMPORTED_LOCATION_DEBUG \"${CppUnit_LIBRARY_DEBUG_DLL\"\n",
        "      IMPORTED_IMPLIB_DEBUG \"${CppUnit_LIBRARY_DEBUG}\"\n",
        "      INTERFACE_INCLUDE_DIRECTORIES \"${CppUnit_INCLUDE_DIR}\"\n",
        "    )\n",
        "set(after value)\n",
    );
    let result = output(source);
    let document = result.document();
    let names = document
        .entities
        .iter()
        .filter(|entity| entity.kind != EntityKind::Module)
        .map(|entity| (entity.canonical_name.as_str(), entity.kind))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        names,
        BTreeSet::from([
            ("before", EntityKind::Variable),
            ("after", EntityKind::Variable),
        ])
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "syntax-error-recovery"
            && gap.source.span().start_byte() == 153
            && gap.source.span().end_byte() == 153
    }));
    assert!(!document.diagnostics.is_empty());
    let calls = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::CallSite
                && text(source, &site.source) == "set_target_properties"
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    let call = calls[0];
    assert_eq!(call.source.span().start_byte(), 18);
    assert_eq!(call.source.span().end_byte(), 39);
    assert_eq!(call.syntax_kind, "cmake.command.call");
    assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn cmake_parse_recovery_preserves_file_bytes_and_unproven_owner_gaps() {
    for source in [
        "function(Written arg)\nmessage([=[tail]===\n",
        "set(value \"unfinished\n",
        "function(Written arg)\nmessage(value)\n",
    ] {
        let result = output(source);
        assert!(!result.document().skipped_regions.is_empty());
        assert!(!result.document().diagnostics.is_empty(), "{source}");
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module),
            "{source}"
        );
    }
}

#[test]
fn cmake_static_unsupported_names_are_reported_without_claiming_computation() {
    for source in [
        "function(name-with-dash arg)\nendfunction()\n",
        "function(\"name with space\" arg)\nendfunction()\n",
        "function(λ arg)\nendfunction()\n",
        "function(escaped\\ name arg)\nendfunction()\n",
    ] {
        let result = output(source);
        assert!(result.document().diagnostics.is_empty(), "{source}");
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.kind == EntityKind::Module),
            "{source}"
        );
        let gap = result
            .document()
            .skipped_regions
            .iter()
            .find(|gap| {
                gap.domain == FactDomain::Entities
                    && gap.detail == "cmake-declaration-name-analysis-unavailable"
            })
            .unwrap();
        assert!(text(source, &gap.source).starts_with("function("));
        assert!(
            !result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail.contains("computed"))
        );
    }
    let quoted = output("function(\"Written\" arg)\nendfunction()\n");
    let callable = quoted
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function)
        .unwrap();
    assert_eq!(callable.canonical_name, "Written");
    let definition = quoted
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target
                    == OccurrenceTarget::Resolved {
                        symbol: callable.id,
                    }
        })
        .unwrap();
    assert_eq!(
        text(
            "function(\"Written\" arg)\nendfunction()\n",
            &definition.source
        ),
        "Written"
    );
}

#[test]
fn cmake_utf16_bom_definitions_keep_raw_hashes_and_physical_coordinates() {
    let source = "# λ😀\r\nfunction(Written arg)\r\nset(λ \"${arg}\")\r\nendfunction()\r\noption(option_name \"help\" ON)\r\n";
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
        let fixture = Fixture::new(CMAKE, &bytes);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(CMAKE),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, CMAKE),
            &input,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(document.files[0].encoding, encoding);
        assert_eq!(document.files[0].content_hash, content_hash(&bytes));
        assert_eq!(
            document.files[0].byte_length,
            u64::try_from(bytes.len()).unwrap()
        );
        let definitions = document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 4);
        for definition in definitions {
            let start = usize::try_from(definition.source.span().start_byte()).unwrap();
            let end = usize::try_from(definition.source.span().end_byte()).unwrap();
            assert_eq!(definition.source.content_hash(), content_hash(&bytes));
            let units = bytes[start..end]
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
                .collect::<Vec<_>>();
            let name = String::from_utf16(&units).unwrap();
            let offset = match name.as_str() {
                "Written" => source.find("Written").unwrap(),
                "arg" => source.find("arg)").unwrap(),
                "λ" => source.find("set(λ").unwrap() + 4,
                "option_name" => source.find("option_name").unwrap(),
                _ => panic!("unexpected definition {name}"),
            };
            assert_eq!(start, 2 + source[..offset].encode_utf16().count() * 2);
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
        }
    }
}

#[test]
fn cmake_retained_artifact_replay_and_cancellation_preserve_query_boundaries() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CMAKE);
    let fixture = Fixture::new(CMAKE, CMAKE.source.as_bytes());
    let budget = limits();
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, CMAKE, &budget),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &request(&fixture.snapshot, &fixture.source, CMAKE, &budget),
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));
    assert!(
        analyzer
            .analyze_from_artifact(
                &request(&fixture.snapshot, &fixture.source, CMAKE, &budget),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .is_err()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn cmake_non_owner_large_literals_preserve_later_definitions_and_lexical_facts() {
    let literal = format!("[=[{}]=]", "x".repeat(600_000));
    let source = format!(
        "# long-literal-marker\r\nmessage({literal})\r\nfunction(Written arg)\r\nendfunction()\r\n"
    );
    let result = output(&source);
    assert!(result.document().entities.iter().any(|entity| entity.kind == EntityKind::Function
        && entity.canonical_name == "Written"));
    for (role, selected) in [
        (OccurrenceRole::CallSite, "message"),
        (OccurrenceRole::Documentation, "# long-literal-marker"),
        (OccurrenceRole::StringEvidence, literal.as_str()),
    ] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == role
                    && text(&source, &occurrence.source) == selected),
            "{role:?}"
        );
    }
}

#[test]
fn cmake_written_imports_keep_exact_paths_and_conservative_dynamic_gaps() {
    let source = "include(literal/module.cmake)\nadd_subdirectory(literal/sub)\nfind_package(Written 1.2 COMPONENTS part)\ninclude(${MODULE})\n";
    let result = output(source);
    let mut imports = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::ImportUse)
        .collect::<Vec<_>>();
    imports.sort_unstable_by_key(|occurrence| occurrence.source.span().start_byte());
    assert_eq!(
        imports
            .into_iter()
            .map(|occurrence| text(source, &occurrence.source))
            .collect::<Vec<_>>(),
        ["literal/module.cmake", "literal/sub", "Written"]
    );
    assert_eq!(
        result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count(),
        4
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                && text(source, &occurrence.source) == "MODULE")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "cmake-variable-evaluation-and-binding-unavailable")
    );
    assert!(result.document().skipped_regions.iter().any(|gap| gap.domain == FactDomain::Relations
        && gap.detail == "cmake-variable-command-condition-macro-and-template-evaluation-unavailable"));
}

// Complete authored template callees keep their original spans and unresolved
// targets even though the native parser receives a private identifier mask.
#[test]
fn cmake_template_calls_keep_full_written_spelling_and_expansion_gaps() {
    let source = concat!(
        "# physical marker λ😀\r\n",
        "function(Written value)\r\n",
        " prefix_@PORT@_suffix@MODE@(value)\r\n",
        "endfunction()\r\n",
        "@ONLY@(value)\r\n",
    );
    let result = output(source);
    let document = result.document();
    let mut calls = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "cmake.template_command.call")
        .collect::<Vec<_>>();
    calls.sort_unstable_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 2);
    for (site, name) in calls.iter().zip(["prefix_@PORT@_suffix@MODE@", "@ONLY@"]) {
        assert_eq!(site.role, OccurrenceRole::CallSite);
        assert_eq!(text(source, &site.source), name);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert_eq!(site.evidence.source.as_ref(), Some(&site.source));
        assert!(document.skipped_regions.iter().any(|gap| gap.detail
            == "cmake-command-dispatch-target-unavailable"
            && gap.domain == FactDomain::Relations
            && text(source, &gap.source) == name));
    }
    for marker in ["@PORT@", "@MODE@", "@ONLY@"] {
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain
                        && text(source, &gap.source) == marker
                        && gap.detail
                            == "cmake-configure-substitution-and-generated-syntax-unavailable"),
                "missing {domain:?} gap for {marker}"
            );
        }
    }
    assert!(document.entities.iter().any(|entity|
        entity.canonical_name == "Written" && entity.kind == EntityKind::Function));
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite
                && matches!(
                    text(source, &site.source),
                    "prefix_" | "_suffix" | "_suffix@MODE@"
                ))
    );
}

#[test]
fn cmake_template_call_projection_keeps_quoted_bracket_and_argument_contexts_opaque() {
    let source = concat!(
        "message(\"fake_@QUOTED@()\")\n",
        "message([=[fake_@BRACKET@()]=])\n",
        "message(unquoted\"fake_@LEGACY@()\")\n",
        "message(nested(fake_@ARGUMENT@()))\n",
        "#[=[fake_@COMMENT@()]=]\n",
        "function(Written)\nendfunction()\n",
        "actual_@REAL@()\n",
    );
    let result = output(source);
    let calls = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "cmake.template_command.call")
        .map(|site| text(source, &site.source))
        .collect::<Vec<_>>();
    assert_eq!(calls, ["actual_@REAL@"]);
    assert!(result.document().entities.iter().any(|entity|
        entity.canonical_name == "Written" && entity.kind == EntityKind::Function));
}

#[test]
fn cmake_malformed_template_source_cannot_publish_recovered_call_fragments() {
    for source in [
        "pre_@NAME@_tail()\nmalformed",
        "pre_@NAME@_tail(",
        "pre_@NAME_tail()",
        "prefix_@A-B(value)",
        "prefix_@A/B(value)",
        "prefix_@A+B(value)",
        "prefix_@A.B(value)",
        "pre_@NAME@_tail()\nother_@BAD()",
    ] {
        let result = output(source);
        let calls = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .map(|site| text(source, &site.source))
            .collect::<Vec<_>>();
        assert!(
            !calls.iter().any(|name| matches!(
                *name,
                "pre_" | "_tail" | "NAME_tail" | "other_" | "BAD" | "prefix_" | "B"
            )),
            "{source:?}: {calls:?}"
        );
        if source.starts_with("pre_@NAME@_tail()") {
            assert!(calls.contains(&"pre_@NAME@_tail"), "{source:?}");
        }
        assert!(!result.document().diagnostics.is_empty(), "{source:?}");
    }
}

#[test]
fn cmake_complete_bracket_comments_preserve_adjacent_written_commands() {
    for (source, expected) in [
        ("#[=[physical comment]=]message(value)", "message"),
        (
            "#[==[physical @COMMENT@]==]actual_@REAL@()",
            "actual_@REAL@",
        ),
    ] {
        let result = output(source);
        let calls = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .map(|site| text(source, &site.source))
            .collect::<Vec<_>>();
        assert_eq!(calls, [expected]);
    }
    for source in [
        "message([==[#[=[comment]=]fake_@NAME@()]==])",
        "message(\"#[=[comment]=]fake_@NAME@()\")",
        "#[=[fake_@NAME@()]=]message(value)",
    ] {
        let result = output(source);
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "cmake.template_command.call")
        );
    }
}

// Conditional C contracts cover authored facts and exact original-byte gaps.
// Written branches do not establish a selected build configuration.
#[test]
fn c_preprocessor_conditions_bound_semantics_without_losing_written_symbols() {
    let source = "#ifndef READER_API\n#define READER_API\n/* contract: stable */\nextern int read_item(int key);\n#endif\n";
    let result = conditional_c_output(source);
    assert_eq!(result.report().coverage().status(), CoverageStatus::Bounded);
    assert_condition_gaps(&result, source, &[source.trim_end()]);
    for (name, written_signature) in [
        ("READER_API", "#define READER_API\n"),
        ("read_item", "(int key)"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.language == "c" && entity.canonical_name == name)
            .unwrap();
        assert_eq!(signature(&result, source, entity.id), [written_signature]);
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    && text(source, &site.source) == name)
        );
        assert!(
            result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id))
        );
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Documentation
                && text(source, &site.source) == "/* contract: stable */")
    );
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            result
                .document()
                .coverage_records
                .iter()
                .any(|record| record.domain == domain
                    && record.status == CoverageStatus::Bounded
                    && record.skipped > 0)
        );
    }
    for domain in [FactDomain::Files, FactDomain::SourceMappings] {
        assert!(
            result
                .document()
                .coverage_records
                .iter()
                .any(|record| record.domain == domain
                    && record.status == CoverageStatus::Complete
                    && record.skipped == 0)
        );
    }
}

#[test]
fn c_preprocessor_conditions_cover_nested_and_alternative_native_contexts() {
    let source = concat!(
        "#if 0\n#define FIRST \"first\"\n",
        "#elif OTHER\n#define SECOND \"second\"\n",
        "#else\n#define THIRD \"third\"\n#endif\n",
        "#ifdef OUTER\n#ifndef INNER\n#define INNER_VALUE \"inner\"\n#endif\n#endif\n",
        "struct Written {\n#if FIELD\nint first;\n#else\nint second;\n#endif\n};\n",
        "enum Choice {\n#if ENUMERATOR\nFIRST_CHOICE,\n#else\nSECOND_CHOICE,\n#endif\n};\n",
    );
    for result in [conditional_c_output(source), header_output(source)] {
        assert_condition_gaps(
            &result,
            source,
            &[
                "#if 0\n#define FIRST \"first\"\n#elif OTHER\n#define SECOND \"second\"\n#else\n#define THIRD \"third\"\n#endif",
                "#ifdef OUTER\n#ifndef INNER\n#define INNER_VALUE \"inner\"\n#endif\n#endif",
                "#ifndef INNER\n#define INNER_VALUE \"inner\"\n#endif",
                "#if FIELD\nint first;\n#else\nint second;\n#endif",
                "#if ENUMERATOR\nFIRST_CHOICE,\n#else\nSECOND_CHOICE,\n#endif",
            ],
        );
        for name in [
            "FIRST",
            "SECOND",
            "THIRD",
            "INNER_VALUE",
            "Written",
            "Choice",
        ] {
            let entity = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.language == "c" && entity.canonical_name == name)
                .unwrap();
            assert!(
                result
                    .document()
                    .occurrences
                    .iter()
                    .any(|site| site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                        && text(source, &site.source) == name)
            );
            if ["FIRST", "SECOND", "THIRD", "INNER_VALUE"].contains(&name) {
                assert_eq!(signature(&result, source, entity.id).len(), 1, "{name}");
            }
        }
        assert_eq!(
            string_evidence_in_source_order(&result, source),
            ["\"first\"", "\"second\"", "\"third\"", "\"inner\""]
        );
    }
}

#[test]
fn c_preprocessor_condition_markers_ignore_comments_and_replacement_strings() {
    let source =
        "/* #if COMMENT\n#endif */\n#define WRITTEN \"#ifdef QUOTED\"\nint written(int value);\n";
    let result = conditional_c_output(source);
    assert_condition_gaps(&result, source, &[]);
    assert_eq!(
        result.report().coverage().status(),
        CoverageStatus::Complete,
        "current semantic gaps: {:#?}",
        result.document().skipped_regions
    );
    assert_eq!(
        string_evidence_in_source_order(&result, source),
        ["\"#ifdef QUOTED\""]
    );
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| ["COMMENT", "QUOTED"].contains(&entity.canonical_name.as_str()))
    );
}

#[test]
fn c_preprocessor_condition_preflight_replay_encoding_and_limits_agree() {
    let source = "/* λ😀 */\r\n#ifndef WRITTEN\r\n#define WRITTEN \"λ😀\"\r\nint written(int value);\r\n#endif\r\n";
    let condition =
        "#ifndef WRITTEN\r\n#define WRITTEN \"λ😀\"\r\nint written(int value);\r\n#endif";
    let case = LanguageCase {
        path: "include/encoded.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let mut bytes = match encoding {
            "utf-8" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            _ => vec![0xfe, 0xff],
        };
        let bom_length = bytes.len();
        bytes.extend_from_slice(&encode(source));
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, &bytes);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(case.generated);
        let required = provider
            .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
            .unwrap();
        let (fresh, artifact) = analyzer
            .analyze_and_capture(
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        validate_ir_document(fresh.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(
            required,
            artifact
                .required_syntax_fact_count(&Cancellation::new())
                .unwrap()
        );
        let mut ir = IrLimits::default();
        ir.max_skipped_regions = 2;
        let short_gap_budget = AnalysisLimits::new(
            budget.max_source_bytes(),
            budget.max_syntax_nodes(),
            budget.max_syntax_depth(),
            budget.max_embedded_ranges(),
            budget.max_reported_memory_bytes(),
            budget.syntax_stream().clone(),
            budget.ir_stream().clone(),
            ir,
        )
        .unwrap();
        let short_gap_input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &short_gap_budget,
        )
        .unwrap()
        .with_generated_status(case.generated);
        assert!(
            analyzer
                .analyze_from_artifact(
                    &short_gap_input,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new()
                )
                .is_err()
        );
        let gaps = fresh
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "c-preprocessor-conditional-evaluation-unavailable")
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), 3, "{encoding}");
        let expected = encode(condition);
        let expected_start = bom_length + encode("/* λ😀 */\r\n").len();
        for gap in gaps {
            assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
            assert_eq!(gap.source.content_hash(), content_hash(&bytes));
            let start = usize::try_from(gap.source.span().start_byte()).unwrap();
            let end = usize::try_from(gap.source.span().end_byte()).unwrap();
            assert_eq!(start, expected_start);
            assert_eq!(bytes.get(start..end).unwrap(), expected);
        }
        let replay = analyzer
            .analyze_from_artifact(
                &input,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(fresh.document(), replay.document());
        assert_eq!(fresh.report().coverage().status(), CoverageStatus::Bounded);
        let bounded_budget = limits_with_syntax_records(required);
        let bounded = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &bounded_budget,
        )
        .unwrap()
        .with_generated_status(case.generated);
        let (bounded_output, bounded_artifact) = analyzer
            .analyze_and_capture(
                &bounded,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            bounded_artifact
                .required_syntax_fact_count(&Cancellation::new())
                .unwrap(),
            required
        );
        assert_eq!(
            bounded_output
                .document()
                .skipped_regions
                .iter()
                .filter(|gap| gap.detail == "c-preprocessor-conditional-evaluation-unavailable")
                .count(),
            3
        );
        let short_budget = limits_with_syntax_records(required.checked_sub(1).unwrap());
        let short = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &short_budget,
        )
        .unwrap()
        .with_generated_status(case.generated);
        assert!(
            matches!(analyzer.analyze_and_capture(&short, ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &Cancellation::new()),
            Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts,
                observed, limit })) if observed == required && limit == required - 1)
        );
        let cancelled = Cancellation::new();
        assert!(cancelled.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            provider.required_syntax_fact_count(&input.to_parse_request(), &cancelled),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(matches!(
            analyzer.analyze_from_artifact(
                &input,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancelled
            ),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

fn conditional_c_output(source: &str) -> AnalysisOutput {
    let case = LanguageCase {
        path: "include/written.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    assert_physical_language(&result, source, "c");
    result
}

fn assert_condition_gaps(result: &AnalysisOutput, source: &str, written: &[&str]) {
    let gaps = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "c-preprocessor-conditional-evaluation-unavailable")
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), written.len() * 3);
    for condition in written {
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            assert_eq!(
                gaps.iter()
                    .filter(|gap| gap.domain == domain
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.source.content_hash() == content_hash(source.as_bytes())
                        && text(source, &gap.source) == *condition)
                    .count(),
                1,
                "{domain:?}: {condition}"
            );
        }
    }
}

#[test]
fn c_macro_comments_keep_complete_headers_single_literals_and_later_owners() {
    let header = concat!(
        "#define WRITTEN 1'000 /* comment\r\n",
        "#define FAKE \"hidden\"\r\n",
        "*/ \"tail\"\r\n",
    );
    let source = format!(
        "{header}#define DECL 1 /* comment */ int phantom(void);\r\n#define CALL 1 /* comment */ phantom()\r\n#define NEXT \"next\"\r\nint after(void) {{ return 0; }}\r\n"
    );
    let ordinary = LanguageCase {
        path: "include/written.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for case in [ordinary, CMAKE_HEADER] {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let result = analyze(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        assert_physical_language(&result, &source, case.name);
        let document = result.document();
        let written = document
            .entities
            .iter()
            .find(|entity| entity.language == "c" && entity.canonical_name == "WRITTEN")
            .unwrap();
        assert_eq!(signature(&result, &source, written.id), [header]);
        assert_eq!(
            text(&source, written.evidence.source.as_ref().unwrap()),
            header
        );
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "FAKE")
        );
        assert_eq!(
            string_evidence_in_source_order(&result, &source),
            ["\"tail\"", "\"next\""]
        );

        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "phantom")
        );
        for fragment in ["int phantom(void);", "phantom()"] {
            let start = u64::try_from(source.find(fragment).unwrap()).unwrap();
            let end = start + u64::try_from(fragment.len()).unwrap();
            assert!(!document.occurrences.iter().any(|occurrence| {
                occurrence.source.span().start_byte() >= start
                    && occurrence.source.span().end_byte() <= end
                    && matches!(
                        occurrence.role,
                        OccurrenceRole::Definition
                            | OccurrenceRole::CallSite
                            | OccurrenceRole::Reference
                            | OccurrenceRole::ImportUse
                    )
            }));
        }
        let tail = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::StringEvidence
                    && text(&source, &occurrence.source) == "\"tail\""
            })
            .collect::<Vec<_>>();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].enclosing, Some(written.id));
        assert_eq!(tail[0].syntactic_text_hash, content_hash(b"\"tail\""));
        let after = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "after")
            .unwrap();
        assert_eq!(after.kind, EntityKind::Function);
        assert_eq!(
            text(&source, after.evidence.source.as_ref().unwrap()),
            "int after(void) { return 0; }"
        );
        assert_ne!(
            after.container,
            Some(rootlight_ir::ContainerRef::Entity(written.id))
        );
    }
}

#[test]
fn c_macro_extents_respect_disjoint_included_ranges_and_excluded_decoys() {
    let prefix = "#define FIRST 1 /* selected */";
    let excluded = " \"decoy\"\n#define EXCLUDED \"hidden\"\n";
    let second = "\n#define SECOND \"second\"\n";
    let source = format!("{prefix}{excluded}{second}");
    let case = LanguageCase {
        path: "include/ranged.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let end_first = u64::try_from(prefix.len()).unwrap();
    let start_second = u64::try_from(prefix.len() + excluded.len()).unwrap();
    let end_second = u64::try_from(source.len()).unwrap();
    let ranges = vec![
        IncludedRange::new(
            SourceSpan::new(fixture.source.span().file(), 0, end_first).unwrap(),
            language(case),
        ),
        IncludedRange::new(
            SourceSpan::new(fixture.source.span().file(), start_second, end_second).unwrap(),
            language(case),
        ),
    ];
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        EncodingId::utf8(),
        ranges,
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(case.generated);
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
        .unwrap();
    let analyzer = analyzer(&provider, case);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(
        required,
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap()
    );
    let replay = analyzer
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    let document = fresh.document();
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "EXCLUDED")
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "SECOND")
    );
    let strings = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::StringEvidence)
        .collect::<Vec<_>>();
    assert_eq!(strings.len(), 1);
    assert_eq!(text(&source, &strings[0].source), "\"second\"");
    for string in strings {
        let span = string.source.span();
        assert!(
            (span.start_byte() < end_first && span.end_byte() <= end_first)
                || (span.start_byte() >= start_second && span.end_byte() <= end_second)
        );
    }
}

#[test]
fn c_macro_projection_restores_later_functions_comments_and_exact_replay() {
    let ordinary = LanguageCase {
        path: "include/projected.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for value in ["1000", "1'000"] {
        let header = format!("#define WRITTEN {value} /** \"hidden\" */ \"tail\"\r\n");
        let source = format!("{header}int after(void) {{ return 0; }}\r\n");
        for case in [ordinary, CMAKE_HEADER] {
            let provider = Arc::new(provider());
            let fixture = Fixture::new(case, source.as_bytes());
            let budget = limits();
            let input = request(&fixture.snapshot, &fixture.source, case, &budget);
            let required = provider
                .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
                .unwrap();
            let analyzer = analyzer(&provider, case);
            let (fresh, artifact) = analyzer
                .analyze_and_capture(
                    &input,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            let replay = analyzer
                .analyze_from_artifact(
                    &input,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                required,
                artifact
                    .required_syntax_fact_count(&Cancellation::new())
                    .unwrap()
            );
            assert_eq!(fresh.document(), replay.document());
            assert_physical_language(&fresh, &source, case.name);
            let document = fresh.document();
            let written = document
                .entities
                .iter()
                .find(|entity| entity.language == "c" && entity.canonical_name == "WRITTEN")
                .unwrap();
            assert_eq!(signature(&fresh, &source, written.id), [header.as_str()]);
            assert_eq!(
                text(&source, written.evidence.source.as_ref().unwrap()),
                header
            );
            let after = document
                .entities
                .iter()
                .find(|entity| entity.language == "c" && entity.canonical_name == "after")
                .unwrap();
            assert_eq!(after.kind, EntityKind::Function);
            assert_eq!(
                text(&source, after.evidence.source.as_ref().unwrap()),
                "int after(void) { return 0; }"
            );
            assert_ne!(
                after.container,
                Some(rootlight_ir::ContainerRef::Entity(written.id))
            );
            assert_eq!(
                string_evidence_in_source_order(&fresh, &source),
                ["\"tail\""]
            );
            let docs = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Documentation
                        && text(&source, &site.source) == "/** \"hidden\" */"
                })
                .collect::<Vec<_>>();
            assert_eq!(docs.len(), 1);
            assert_eq!(docs[0].enclosing, Some(written.id));
            assert_eq!(
                docs[0].source.content_hash(),
                content_hash(source.as_bytes())
            );
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| site.role == OccurrenceRole::CallSite)
            );
        }
    }
}

#[test]
fn c_macro_projection_encoded_evidence_and_lexical_gaps_use_original_bytes() {
    let source = "#define WRITTEN 1 /** doc \u{03bb}\u{1f600} */ L\"\u{03bb}\u{1f600}\"\r\n#define BROKEN 1 /* comment */ 'unfinished\r\nint after(void) { return 0; }\r\n";
    let ordinary = LanguageCase {
        path: "include/encoded-projection.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for case in [ordinary, CMAKE_HEADER] {
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            let encode = |text: &str| -> Vec<u8> {
                if encoding == "utf-8" {
                    text.as_bytes().to_vec()
                } else {
                    text.encode_utf16()
                        .flat_map(|unit| {
                            if encoding == "utf-16le" {
                                unit.to_le_bytes()
                            } else {
                                unit.to_be_bytes()
                            }
                        })
                        .collect()
                }
            };
            let mut bytes = match encoding {
                "utf-8" => vec![0xef, 0xbb, 0xbf],
                "utf-16le" => vec![0xff, 0xfe],
                _ => vec![0xfe, 0xff],
            };
            let bom = bytes.len();
            bytes.extend_from_slice(&encode(source));
            let provider = Arc::new(provider());
            let fixture = Fixture::new(case, &bytes);
            let budget = limits();
            let input = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
                language(case),
                EncodingId::new(encoding).unwrap(),
                Vec::new(),
                AnalysisTier::TierD,
                build_context(),
                &budget,
            )
            .unwrap()
            .with_generated_status(case.generated);
            let required = provider
                .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
                .unwrap();
            let analyzer = analyzer(&provider, case);
            let (fresh, artifact) = analyzer
                .analyze_and_capture(
                    &input,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            let replay = analyzer
                .analyze_from_artifact(
                    &input,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                required,
                artifact
                    .required_syntax_fact_count(&Cancellation::new())
                    .unwrap()
            );
            assert_eq!(fresh.document(), replay.document());
            validate_ir_document(fresh.document(), budget.ir(), &ExtensionSupport::default())
                .unwrap();
            let written = fresh
                .document()
                .entities
                .iter()
                .find(|entity| entity.language == "c" && entity.canonical_name == "WRITTEN")
                .unwrap();
            let header = source.split_inclusive('\n').next().unwrap();
            let owner = written.evidence.source.as_ref().unwrap();
            assert_eq!(owner.span().start_byte(), u64::try_from(bom).unwrap());
            assert_eq!(
                &bytes[usize::try_from(owner.span().start_byte()).unwrap()
                    ..usize::try_from(owner.span().end_byte()).unwrap()],
                encode(header)
            );
            let literals = fresh
                .document()
                .occurrences
                .iter()
                .filter(|site| site.role == OccurrenceRole::StringEvidence)
                .collect::<Vec<_>>();
            assert_eq!(literals.len(), 1);
            let literal = literals[0];
            assert_eq!(
                &bytes[usize::try_from(literal.source.span().start_byte()).unwrap()
                    ..usize::try_from(literal.source.span().end_byte()).unwrap()],
                encode("L\"\u{03bb}\u{1f600}\"")
            );
            assert_eq!(literal.enclosing, Some(written.id));
            assert_eq!(literal.source.content_hash(), content_hash(&bytes));
            let gaps = fresh
                .document()
                .skipped_regions
                .iter()
                .filter(|gap| gap.detail == "c-preprocessor-replacement-lexical-unavailable")
                .collect::<Vec<_>>();
            assert_eq!(gaps.len(), 1);
            let gap = &gaps[0].source;
            assert_eq!(
                &bytes[usize::try_from(gap.span().start_byte()).unwrap()
                    ..usize::try_from(gap.span().end_byte()).unwrap()],
                encode("'unfinished\r\n")
            );
            assert!(
                fresh
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == "after"
                        && entity.kind == EntityKind::Function)
            );
        }
    }
}

#[test]
fn c_macro_projection_withholds_projected_cache_and_keeps_clean_reuse() {
    use rootlight_adapter_treesitter::{ReuseInvalidation, ReuseStatus, SourceEdit};
    let case = LanguageCase {
        path: "include/reuse-projection.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    let clean = "#define VALUE 1000\nint after(void) { return 0; }\n";
    let insertion = " /* comment */ \"tail\"";
    let offset = clean.find('\n').unwrap();
    let changed = format!("{}{insertion}{}", &clean[..offset], &clean[offset..]);
    let provider = Arc::new(provider());
    let budget = limits();
    let first_fixture = Fixture::new(case, clean.as_bytes());
    let first_input = request(
        &first_fixture.snapshot,
        &first_fixture.source,
        case,
        &budget,
    );
    let first = provider
        .execute_with_previous(
            &first_input.to_parse_request(),
            None,
            &[],
            ParserSettings::new(4096).unwrap(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    let previous = first
        .previous()
        .expect("clean native macro tree remains reusable");
    let second_fixture = Fixture::new(case, changed.as_bytes());
    let second_input = request(
        &second_fixture.snapshot,
        &second_fixture.source,
        case,
        &budget,
    );
    let edit = SourceEdit::new(offset, offset, insertion).unwrap();
    let projected = provider
        .execute_with_previous(
            &second_input.to_parse_request(),
            Some(previous),
            &[edit],
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
    let fresh = provider
        .execute_with_previous(
            &second_input.to_parse_request(),
            None,
            &[],
            ParserSettings::new(4096).unwrap(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(projected.output().facts(), fresh.output().facts());
    assert_eq!(
        projected.output().diagnostics(),
        fresh.output().diagnostics()
    );
    assert!(fresh.previous().is_none());
    let repeated = provider
        .execute_with_previous(
            &first_input.to_parse_request(),
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
}

#[test]
fn c_macro_projection_uses_written_function_header_and_not_replacement_calls() {
    let object = "#define OBJECT 1 /* comment */ phantom()\n";
    let function = "#define APPLY(value, /* parameter */ ...) 1 /* comment */ \"tail\"\n";
    let source = format!("{object}{function}int after(void) {{ return 0; }}\n");
    let ordinary = LanguageCase {
        path: "include/header-shape.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for case in [ordinary, CMAKE_HEADER] {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let result = analyze(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        for (name, kind, header) in [
            ("OBJECT", EntityKind::Constant, object),
            ("APPLY", EntityKind::Function, function),
        ] {
            let entity = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.language == "c" && entity.canonical_name == name)
                .unwrap();
            assert_eq!(entity.kind, kind);
            assert_eq!(signature(&result, &source, entity.id), [header]);
        }
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "phantom")
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite)
        );
        assert_eq!(
            string_evidence_in_source_order(&result, &source),
            ["\"tail\""]
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "after"
                    && entity.kind == EntityKind::Function)
        );
    }
}

#[test]
fn c_macro_projection_in_markdown_preserves_host_bytes_and_child_replay() {
    let source = "before\n\n```c\n#define WRITTEN 1 /* comment */ \"tail\"\nint after(void) { return 0; }\n```\n\nafter\n";
    let case = LanguageCase {
        name: "markdown",
        path: "notes/written.md",
        frontend: "tree-sitter-md-0.5.3",
        ..CASES[0]
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &budget);
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
        .unwrap();
    let analyzer = analyzer(&provider, case);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(
        required,
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap()
    );
    let written = fresh
        .document()
        .entities
        .iter()
        .find(|entity| entity.language == "c" && entity.canonical_name == "WRITTEN")
        .unwrap();
    assert_eq!(
        signature(&fresh, source, written.id),
        ["#define WRITTEN 1 /* comment */ \"tail\"\n"]
    );
    assert!(
        fresh
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "c" && entity.canonical_name == "after")
    );
    let strings = fresh
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::StringEvidence && site.enclosing == Some(written.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(strings.len(), 1);
    assert_eq!(text(source, &strings[0].source), "\"tail\"");
    assert_eq!(
        strings[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
}

const C_PROJECTION_NO_ROOM: &str = "#define EMPTY \nint ordinary_after_empty(void) { return 0; }\n#define ESCAPED\\\n /* comment */ \"tail\"\nint after(void) { return 0; }\n";

#[test]
fn c_macro_projection_preserves_empty_guards_without_synthetic_values() {
    let source =
        "#define EMPTY\n#define WRITTEN 1 /* comment */ \"tail\"\nint after(void) { return 0; }\n";
    let ordinary = LanguageCase {
        path: "include/empty-projection.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for case in [ordinary, CMAKE_HEADER] {
        let provider = Arc::new(provider());
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let result = analyze(
            &analyzer(&provider, case),
            &request(&fixture.snapshot, &fixture.source, case, &budget),
            &ExtensionSupport::default(),
        );
        let empty = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.language == "c" && entity.canonical_name == "EMPTY")
            .unwrap();
        assert_eq!(empty.kind, EntityKind::Constant);
        assert_eq!(signature(&result, source, empty.id), ["#define EMPTY\n"]);
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "after"
                    && entity.kind == EntityKind::Function)
        );
        assert_eq!(
            string_evidence_in_source_order(&result, source),
            ["\"tail\""]
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::StringEvidence
                    && site.enclosing == Some(empty.id))
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| text(source, &site.source) == "0"
                    && site.role == OccurrenceRole::Reference)
        );
    }
}

#[test]
fn c_macro_projection_no_room_returns_source_bound_gaps_with_replay_and_limits() {
    let ordinary = LanguageCase {
        path: "include/no-room-projection.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    for case in [ordinary, CMAKE_HEADER] {
        for encoding in ["utf-8", "utf-16le", "utf-16be"] {
            let mut bytes = match encoding {
                "utf-8" => vec![0xef, 0xbb, 0xbf],
                "utf-16le" => vec![0xff, 0xfe],
                _ => vec![0xfe, 0xff],
            };
            if encoding == "utf-8" {
                bytes.extend_from_slice(C_PROJECTION_NO_ROOM.as_bytes());
            } else {
                bytes.extend(C_PROJECTION_NO_ROOM.encode_utf16().flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                }));
            }
            let provider = Arc::new(provider());
            let fixture = Fixture::new(case, &bytes);
            let budget = limits();
            let input = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
                language(case),
                EncodingId::new(encoding).unwrap(),
                Vec::new(),
                AnalysisTier::TierD,
                build_context(),
                &budget,
            )
            .unwrap()
            .with_generated_status(case.generated);
            let required = provider
                .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
                .unwrap();
            let analyzer = analyzer(&provider, case);
            let (fresh, artifact) = analyzer
                .analyze_and_capture(
                    &input,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            let replay = analyzer
                .analyze_from_artifact(
                    &input,
                    &artifact,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                required,
                artifact
                    .required_syntax_fact_count(&Cancellation::new())
                    .unwrap()
            );
            assert_eq!(fresh.document(), replay.document());
            validate_ir_document(fresh.document(), budget.ir(), &ExtensionSupport::default())
                .unwrap();
            assert_eq!(fresh.document().files.len(), 1);
            assert_eq!(fresh.document().files[0].content_hash, content_hash(&bytes));
            assert!(
                fresh
                    .document()
                    .entities
                    .iter()
                    .filter(|entity| entity.language == "c")
                    .all(|entity| entity.kind == EntityKind::Module)
            );
            assert!(!fresh.document().occurrences.iter().any(|site| matches!(
                site.role,
                OccurrenceRole::Definition
                    | OccurrenceRole::CallSite
                    | OccurrenceRole::StringEvidence
            )));
            for domain in [
                rootlight_ir::FactDomain::Entities,
                rootlight_ir::FactDomain::Occurrences,
                rootlight_ir::FactDomain::Relations,
            ] {
                let gaps = fresh
                    .document()
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == domain
                            && gap.detail == "c-preprocessor-replacement-not-analyzed"
                    })
                    .collect::<Vec<_>>();
                assert_eq!(gaps.len(), 1);
                assert_eq!(
                    gaps[0].reason,
                    rootlight_ir::SkippedRegionReason::UnsupportedConstruct
                );
                assert_eq!(gaps[0].source.span(), fixture.source.span());
                assert_eq!(gaps[0].source.content_hash(), content_hash(&bytes));
                assert_eq!(gaps[0].evidence.source.as_ref(), Some(&gaps[0].source));
                assert!(
                    fresh
                        .document()
                        .coverage_records
                        .iter()
                        .any(|coverage| coverage.domain == domain
                            && coverage.status != rootlight_ir::CoverageStatus::Complete)
                );
            }
            assert!(
                !fresh
                    .document()
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "syntax-context-limit")
            );
            if case.name == "c" {
                assert_eq!(required, 0);
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
                assert!(parsed.output().facts().is_empty());
                assert!(parsed.previous().is_none());
                assert_eq!(parsed.output().diagnostics().len(), 1);
                assert_eq!(
                    parsed.output().diagnostics()[0].code().as_str(),
                    "c-preprocessor-replacement-not-analyzed"
                );
            }
        }
    }
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ordinary, C_PROJECTION_NO_ROOM.as_bytes());
    let normal = limits();
    let mut ir = normal.ir().clone();
    ir.max_skipped_regions = 3;
    let tight = AnalysisLimits::new(
        normal.max_source_bytes(),
        normal.max_syntax_nodes(),
        normal.max_syntax_depth(),
        normal.max_embedded_ranges(),
        normal.max_reported_memory_bytes(),
        normal.syntax_stream().clone(),
        normal.ir_stream().clone(),
        ir,
    )
    .unwrap();
    let error = analyzer(&provider, ordinary)
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, ordinary, &tight),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap_err();
    assert!(
        matches!(
            error,
            rootlight_adapter_sdk::AdapterError::Sink(
                rootlight_adapter_sdk::SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::Records,
                    observed: 4,
                    limit: 3
                }
            )
        ),
        "{error:?}"
    );
    let mut ir = normal.ir().clone();
    ir.max_skipped_regions = 4;
    let exact = AnalysisLimits::new(
        normal.max_source_bytes(),
        normal.max_syntax_nodes(),
        normal.max_syntax_depth(),
        normal.max_embedded_ranges(),
        normal.max_reported_memory_bytes(),
        normal.syntax_stream().clone(),
        normal.ir_stream().clone(),
        ir,
    )
    .unwrap();
    let result = analyze(
        &analyzer(&provider, ordinary),
        &request(&fixture.snapshot, &fixture.source, ordinary, &exact),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), exact.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().skipped_regions.len(), 4);
    assert_eq!(
        result
            .document()
            .skipped_regions
            .iter()
            .map(|gap| gap.domain)
            .collect::<std::collections::BTreeSet<_>>(),
        [
            rootlight_ir::FactDomain::Diagnostics,
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ]
        .into_iter()
        .collect()
    );
    assert!(result.document().skipped_regions.iter().all(|gap| {
        gap.detail == "c-preprocessor-replacement-not-analyzed" && gap.source == fixture.source
    }));
}

#[test]
fn c_macro_projection_blocked_fences_keep_host_and_other_language_facts() {
    let source = format!(
        "# Written host\n\n```c\n{C_PROJECTION_NO_ROOM}```\n\n```python\ndef retained_neighbor():\n    return 1\n```\n\n```c\n{C_PROJECTION_NO_ROOM}```\n"
    );
    let case = LanguageCase {
        name: "markdown",
        path: "notes/no-room.md",
        frontend: "tree-sitter-md-0.5.3",
        ..CASES[0]
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &budget);
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &Cancellation::new())
        .unwrap();
    let analyzer = analyzer(&provider, case);
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    let replay = analyzer
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    assert_eq!(
        required,
        artifact
            .required_syntax_fact_count(&Cancellation::new())
            .unwrap()
    );
    assert!(
        fresh
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "markdown")
    );
    assert!(
        fresh
            .document()
            .entities
            .iter()
            .any(|entity| entity.language == "python"
                && entity.canonical_name == "retained_neighbor")
    );
    let modules = fresh
        .document()
        .entities
        .iter()
        .filter(|entity| entity.language == "c")
        .collect::<Vec<_>>();
    assert_eq!(modules.len(), 2);
    assert!(
        modules
            .iter()
            .all(|entity| entity.kind == EntityKind::Module)
    );
    let expected_starts = source
        .match_indices(C_PROJECTION_NO_ROOM)
        .map(|(offset, _)| u64::try_from(offset).unwrap())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        modules
            .iter()
            .map(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte())
            .collect::<std::collections::BTreeSet<_>>(),
        expected_starts
    );
    for module in modules {
        let owner = module.evidence.source.as_ref().unwrap();
        assert_eq!(text(&source, owner), C_PROJECTION_NO_ROOM);
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            let gaps = fresh
                .document()
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.domain == domain
                        && gap.detail == "c-preprocessor-replacement-not-analyzed"
                        && gap.source == *owner
                })
                .collect::<Vec<_>>();
            assert_eq!(gaps.len(), 1);
            assert_eq!(
                gaps[0].reason,
                rootlight_ir::SkippedRegionReason::UnsupportedConstruct
            );
        }
    }
    assert!(
        !fresh
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-context-limit")
    );
}

#[test]
fn c_macro_projection_clean_proof_and_inspection_share_one_node_budget() {
    let source = "#define WRITTEN 1'000 \"tail\"\nint after(void) { return 0; }\n";
    let mut native = tree_sitter::Parser::new();
    native
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .unwrap();
    let tree = native.parse(source, None).unwrap();
    let nodes = tree.root_node().descendant_count();
    assert_eq!(nodes, 21);
    let ordinary = LanguageCase {
        path: "include/proof-credit.h",
        ..*CASES.iter().find(|case| case.name == "c").unwrap()
    };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(ordinary, source.as_bytes());
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
        let input = request(&fixture.snapshot, &fixture.source, ordinary, &budget);
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
                    .any(|diagnostic| diagnostic.code().as_str() == "syntax-node-limit")
            );
        } else {
            assert!(!parsed.output().facts().is_empty());
            assert!(parsed.previous().is_some());
            assert!(parsed.output().diagnostics().is_empty());
            let reused = provider
                .execute_with_previous(
                    &input.to_parse_request(),
                    parsed.previous(),
                    &[],
                    ParserSettings::new(4096).unwrap(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(reused.output().facts(), parsed.output().facts());
            assert_eq!(reused.report().resources().syntax_nodes(), maximum);
            assert!(matches!(
                reused.reuse_status(),
                rootlight_adapter_treesitter::ReuseStatus::Reused { .. }
            ));
        }
    }
}

#[test]
fn c_written_prototype_formals_keep_exact_parameter_ownership_without_reads() {
    let source = "int chosen(int value);\nconst char *buffer(const char *text);\n";
    let result = conditional_c_output(source);
    let document = result.document();
    assert_eq!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    for (name, owner_name, written) in [
        ("value", "chosen", "int value"),
        ("text", "buffer", "const char *text"),
    ] {
        let parameters: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == name)
            .collect();
        assert_eq!(parameters.len(), 1);
        let parameter = parameters[0];
        let owners: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == owner_name
            })
            .collect();
        assert_eq!(owners.len(), 1);
        let owner = owners[0];
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        assert_eq!(text(source, &definitions[0].source), name);
        assert_eq!(
            definitions[0].source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(definitions[0].source.generation(), parameter.generation);
        assert!(source.contains(written));
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner.id)
                    && edge.object == RelationEndpoint::Entity(parameter.id))
                .count(),
            1
        );
    }
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.role, OccurrenceRole::Read | OccurrenceRole::CallSite))
    );
    assert!(document.relations.iter().all(|edge| !matches!(
        edge.predicate,
        RelationPredicate::Reads | RelationPredicate::Calls
    )));
}
