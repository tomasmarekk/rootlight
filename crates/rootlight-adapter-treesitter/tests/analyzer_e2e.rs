//! Public-boundary integration contracts for the real Tree-sitter analyzer.
//!
//! The audited grammars flow from VFS snapshots through parsing, lowering,
//! canonical normalized IR, and explicit validation without native parser types.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageAnalyzer, LanguageId, MemoryAdmissionPolicy,
    MemoryAdmissionStatus, MemoryEnforcement, ParseProvider, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, RepositoryId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageScope, CoverageStatus, EntityFlag, EntityKind,
    ExtensionSupport, FactDomain, FactEvidence, IrDocument, IrLimits, OccurrenceRole,
    OccurrenceTarget, ProducerIdentity, ProducerKind, RelationEndpoint, RelationPredicate,
    SkippedRegionReason, SourceRef, SourceSpan, decode_ir_document, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_SYNTAX_NODES: usize = 16_384;
const MAX_SYNTAX_DEPTH: usize = 128;
const BINARY_SEED: &[u8] = b"rootlight-treesitter-e2e-binary";
const BUILD_CONTEXT_SEED: &[u8] = b"rootlight-treesitter-e2e-build";
const CONFIGURATION_SEED: &[u8] = b"rootlight-treesitter-e2e-configuration";

#[path = "analyzer_e2e/astro_native.rs"]
mod astro_native;
#[path = "analyzer_e2e/dart_native.rs"]
mod dart_native;
#[path = "analyzer_e2e/html_native.rs"]
mod html_native;
#[path = "analyzer_e2e/markdown_native.rs"]
mod markdown_native;
#[path = "analyzer_e2e/powershell_native.rs"]
mod powershell_native;
#[path = "analyzer_e2e/r_native.rs"]
mod r_native;
#[path = "analyzer_e2e/scala_native.rs"]
mod scala_native;
#[path = "analyzer_e2e/solidity_native.rs"]
mod solidity_native;
#[path = "analyzer_e2e/sql_native.rs"]
mod sql_native;
#[path = "analyzer_e2e/yaml_native.rs"]
mod yaml_native;
const EXPECTED_DOMAINS: [FactDomain; 8] = [
    FactDomain::Files,
    FactDomain::Entities,
    FactDomain::Occurrences,
    FactDomain::Relations,
    FactDomain::Provenance,
    FactDomain::SourceMappings,
    FactDomain::Diagnostics,
    FactDomain::Extensions,
];

const LUA_CASE: LanguageCase = LanguageCase {
    name: "lua",
    path: "src/module.lua",
    frontend: "tree-sitter-lua-0.5.0",
    source: include_str!("fixtures/structural/lua.lua"),
    generated: false,
    body_before: "return value + limit",
    body_after: "return  value + limit",
};

const RUBY_CASE: LanguageCase = LanguageCase {
    name: "ruby",
    path: "src/example.rb",
    frontend: "tree-sitter-ruby-0.23.1",
    source: include_str!("fixtures/structural/ruby.rb"),
    generated: false,
    body_before: "puts(name)",
    body_after: "puts( name )",
};

const SWIFT_CASE: LanguageCase = LanguageCase {
    name: "swift",
    path: "src/example.swift",
    frontend: "tree-sitter-swift-0.7.3",
    source: include_str!("fixtures/structural/swift.swift"),
    generated: false,
    body_before: "return self",
    body_after: "return  self",
};

const CSS_CASE: LanguageCase = LanguageCase {
    name: "css",
    path: "src/theme.css",
    frontend: "tree-sitter-css-0.25.0",
    source: include_str!("fixtures/css.css"),
    generated: false,
    body_before: "opacity: 0.8",
    body_after: "opacity: 0.9",
};

const BASH_CASE: LanguageCase = LanguageCase {
    name: "bash",
    path: "src/commands.sh",
    frontend: "tree-sitter-bash-0.25.1",
    source: include_str!("fixtures/structural/bash.sh"),
    generated: false,
    body_before: "first $ROOT",
    body_after: "changed $ROOT",
};

const JSON_CASE: LanguageCase = LanguageCase {
    name: "json",
    path: "src/settings.json",
    frontend: "tree-sitter-json-0.24.8",
    source: r#"{"name":1,"name":2,"items":[{"id":3},null,{"id":4}],"nested":{"id":5},"":6,"\u0061":7}"#,
    generated: false,
    body_before: ":3",
    body_after: ":30",
};

#[test]
fn json_native_preserves_every_member_and_distinct_data_container() {
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(JSON_CASE, JSON_CASE.source.as_bytes());
    let analyzer = analyzer(&provider, JSON_CASE);
    let initial_request = request(&fixture.snapshot, &fixture.source, JSON_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    let document = output.document();
    validate_ir_document(document, limits.ir(), &ExtensionSupport::default())
        .expect("valid JSON IR");
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
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    let properties: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 9, "{:?}", properties);
    let ids: BTreeSet<_> = properties.iter().map(|entity| entity.id).collect();
    assert_eq!(ids.len(), properties.len());
    for entity in properties {
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        let span = definition.source.span();
        let raw = &JSON_CASE.source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(definition.syntactic_text_hash, content_hash(raw.as_bytes()));
        assert_eq!(definition.evidence.source, entity.evidence.source);
        assert_eq!(definition.source.generation(), fixture.source.generation());
    }
    let edited = JSON_CASE
        .source
        .replace(JSON_CASE.body_before, JSON_CASE.body_after);
    let changed = fixture.rewrite(edited.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, JSON_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    assert_eq!(symbol_ids(document), symbol_ids(reparsed.document()));
}

#[test]
fn json_addresses_follow_values_not_comments_or_object_member_order() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, JSON_CASE);
    let limits = limits();
    for (before, after, stable) in [
        (
            r#"[{"key":101},null,{"key":202}]"#,
            r#"[ { "key" : 101 },false,/*gap*/{"key":202}]"#,
            true,
        ),
        (
            r#"[[{"key":101}],null,[{"key":202}]]"#,
            r#"[[{"key":101}],"gap",[{"key":202}]]"#,
            true,
        ),
        (
            r#"{"other":0,"key":101,"key":202}"#,
            r#"{"key":101,"key":202,"other":99}"#,
            true,
        ),
        (
            r#"{"key":101,"key":202}"#,
            r#"{"k\u0065y":101,"key":202}"#,
            true,
        ),
        (
            r#"[{"key":101},null,{"key":202}]"#,
            r#"[false,{"key":101},null,{"key":202}]"#,
            false,
        ),
        (
            r#"{"key":101} {"key":202}"#,
            r#"null {"key":101} {"key":202}"#,
            false,
        ),
    ] {
        let fixture = Fixture::new(JSON_CASE, before.as_bytes());
        let first = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, JSON_CASE, &limits),
            &ExtensionSupport::default(),
        );
        let changed = fixture.rewrite(after.as_bytes());
        let second = analyze(
            &analyzer,
            &request(&changed.snapshot, &changed.source, JSON_CASE, &limits),
            &ExtensionSupport::default(),
        );
        let select = |output: &AnalysisOutput| {
            assert!(output.document().skipped_regions.is_empty());
            assert!(output.document().diagnostics.is_empty());
            output
                .document()
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == r#""key""#)
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
        };
        let first = select(&first);
        let second = select(&second);
        assert_eq!(first.len(), 2, "{before}");
        assert_eq!(second.len(), 2, "{after}");
        assert_eq!(first == second, stable, "{before} -> {after}");
    }
}

#[test]
fn json_artifacts_preserve_position_accounting_at_the_required_fact_boundary() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, JSON_CASE);
    let fixture = Fixture::new(JSON_CASE, JSON_CASE.source.as_bytes());
    let initial_limits = limits();
    let initial = request(
        &fixture.snapshot,
        &fixture.source,
        JSON_CASE,
        &initial_limits,
    );
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("complete JSON artifact");
    let required = artifact
        .required_syntax_fact_count(&deadline())
        .expect("required facts");
    assert_eq!(
        required,
        provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .expect("independent native preflight")
    );
    let changed = fixture.next_generation();
    let bounded_limits = limits_with_syntax_records(required);
    let bounded_request = request(
        &changed.snapshot,
        &changed.source,
        JSON_CASE,
        &bounded_limits,
    );
    let (_, bounded_artifact) = analyzer
        .analyze_and_capture(
            &bounded_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("fresh bounded capture retains all positions");
    let reused = analyzer
        .analyze_from_artifact(
            &bounded_request,
            &bounded_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("identity closure fits");
    let clean = analyze(&analyzer, &bounded_request, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(
        reused
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .count(),
        9
    );
    let too_small = limits_with_syntax_records(required - 1);
    let rejected = analyzer.analyze_and_capture(
        &request(&changed.snapshot, &changed.source, JSON_CASE, &too_small),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    );
    assert!(
        rejected.is_err(),
        "never drop scalar position evidence to fit a budget"
    );
}

const TOML_CASE: LanguageCase = LanguageCase {
    name: "toml",
    path: "src/settings.toml",
    frontend: "tree-sitter-toml-ng-0.7.0",
    source: r#"title = "demo"
mixed = [1, 2.0, true, 1979-05-27T07:32Z, 1979-05-27T07:32, 1979-05-27, 07:32, "text", [{"key" = 9}], {key = 10}]
[[items]]
name = "first"
[[items.children]]
name = "child"
[[items]]
name = "second"
[items.meta]
"" = 11
"a.b" = 12
a.b = 13
"k\x65y" = 14
" = " = 15
"#,
    generated: false,
    body_before: "key = 10",
    body_after: "key = 100",
};

#[test]
fn toml_native_resolves_full_key_paths_and_latest_table_array_parents() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, TOML_CASE);
    let fixture = Fixture::new(TOML_CASE, TOML_CASE.source.as_bytes());
    let limits = limits();
    let output = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, TOML_CASE, &limits),
        &ExtensionSupport::default(),
    );
    let document = output.document();
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
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    let expected = [
        (r#""title""#, TOML_CASE.path),
        (r#""mixed""#, TOML_CASE.path),
        (r#""mixed"[8][0]."key""#, r#""mixed""#),
        (r#""mixed"[9]."key""#, r#""mixed""#),
        (r#""items"[0]"#, TOML_CASE.path),
        (r#""items"[0]."name""#, r#""items"[0]"#),
        (r#""items"[0]."children"[0]"#, r#""items"[0]"#),
        (
            r#""items"[0]."children"[0]."name""#,
            r#""items"[0]."children"[0]"#,
        ),
        (r#""items"[1]"#, TOML_CASE.path),
        (r#""items"[1]."name""#, r#""items"[1]"#),
        (r#""items"[1]."meta""#, r#""items"[1]"#),
        (r#""items"[1]."meta"."""#, r#""items"[1]."meta""#),
        (r#""items"[1]."meta"."a.b""#, r#""items"[1]."meta""#),
        (r#""items"[1]."meta"."a"."b""#, r#""items"[1]."meta""#),
        (r#""items"[1]."meta"."key""#, r#""items"[1]."meta""#),
        (r#""items"[1]."meta"." = ""#, r#""items"[1]."meta""#),
    ];
    assert_eq!(
        document.entities.len(),
        expected.len() + 1,
        "{:?}",
        document.entities
    );
    for (qualified, parent) in expected {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.qualified_name == qualified)
            .unwrap_or_else(|| panic!("missing {qualified}: {:?}", document.entities));
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.qualified_name == parent)
            .expect("written parent is retained");
        assert!(
            document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(entity.id)
            }),
            "{qualified}"
        );
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "{qualified}");
        let definition = definitions[0];
        let span = definition.source.span();
        let raw = &TOML_CASE.source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(definition.syntactic_text_hash, content_hash(raw.as_bytes()));
        assert_eq!(definition.source.generation(), fixture.source.generation());
        assert_eq!(definition.evidence.source, entity.evidence.source);
    }
    let edited = TOML_CASE
        .source
        .replace(TOML_CASE.body_before, TOML_CASE.body_after);
    let changed = fixture.rewrite(edited.as_bytes());
    let reparsed = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, TOML_CASE, &limits),
        &ExtensionSupport::default(),
    );
    assert_eq!(symbol_ids(document), symbol_ids(reparsed.document()));
}

#[test]
fn toml_artifacts_keep_scalar_positions_at_the_required_fact_boundary() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, TOML_CASE);
    let fixture = Fixture::new(TOML_CASE, TOML_CASE.source.as_bytes());
    let initial_limits = limits();
    let initial = request(
        &fixture.snapshot,
        &fixture.source,
        TOML_CASE,
        &initial_limits,
    );
    let (full, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("complete TOML artifact");
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
    let bounded = request(
        &changed.snapshot,
        &changed.source,
        TOML_CASE,
        &bounded_limits,
    );
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &bounded,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("bounded identity capture");
    let reused = analyzer
        .analyze_from_artifact(
            &bounded,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("bounded identity replay");
    let clean = analyze(&analyzer, &bounded, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(symbol_ids(full.document()), symbol_ids(reused.document()));
    let too_small = limits_with_syntax_records(required - 1);
    assert!(
        analyzer
            .analyze_and_capture(
                &request(&changed.snapshot, &changed.source, TOML_CASE, &too_small),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err(),
        "never omit scalar positions to meet a budget"
    );
}

#[test]
fn toml_native_identity_tracks_data_addresses_instead_of_lexical_headers() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, TOML_CASE);
    let limits = limits();
    for (before, after, stable) in [
        ("a.key=1\n", "[a]\nkey=2\n", true),
        ("a.key=1\n", "a.key=2\n[a]\n", true),
        ("key=1\n", "\"k\\x65y\" = 2\n", true),
        (
            "a=[1,[{key=2}]]\n",
            "a=[false, [ # gap\n {key=3} ]]\n",
            true,
        ),
        ("a=[1,[{key=2}]]\n", "a=[1,2,[{key=2}]]\n", false),
        ("[[a]]\nkey=1\n", "[[a]]\nother=0\n[[a]]\nkey=1\n", false),
    ] {
        let fixture = Fixture::new(TOML_CASE, before.as_bytes());
        let first = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, TOML_CASE, &limits),
            &ExtensionSupport::default(),
        );
        let changed = fixture.rewrite(after.as_bytes());
        let second = analyze(
            &analyzer,
            &request(&changed.snapshot, &changed.source, TOML_CASE, &limits),
            &ExtensionSupport::default(),
        );
        for output in [&first, &second] {
            assert!(
                output.document().skipped_regions.is_empty(),
                "{before} -> {after}"
            );
            assert!(
                output.document().diagnostics.is_empty(),
                "{before} -> {after}"
            );
        }
        assert_eq!(
            symbol_id_named(first.document(), r#""key""#)
                == symbol_id_named(second.document(), r#""key""#),
            stable,
            "{before} -> {after}"
        );
    }
}

#[test]
fn toml_native_ambiguous_tables_and_invalid_keys_leave_explicit_gaps() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, TOML_CASE);
    let limits = limits();
    for source in [
        "[bad]\nkey=1\n[bad]\nkey=2\n[good]\nkey=3\n",
        "[\"\\uD800\"]\nkey=1\n[good]\nkey=3\n",
    ] {
        let fixture = Fixture::new(TOML_CASE, source.as_bytes());
        let output = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, TOML_CASE, &limits),
            &ExtensionSupport::default(),
        );
        let document = output.document();
        assert!(!document.skipped_regions.is_empty(), "{source}");
        assert_ne!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        let properties: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Property)
            .collect();
        assert_eq!(properties.len(), 1, "{source}: {properties:?}");
        assert_eq!(properties[0].qualified_name, r#""good"."key""#);
        assert!(
            document
                .skipped_regions
                .iter()
                .all(|region| { region.source.generation() == fixture.source.generation() })
        );
    }
}

const CASES: [LanguageCase; 15] = [
    LanguageCase {
        name: "rust",
        path: "src/lib.rs",
        frontend: "tree-sitter-rust-0.24.2",
        source: include_str!("fixtures/structural/rust.rs"),
        generated: false,
        body_before: " {\n        let text = \"Hello 🌍\";\n        greet(name);\n        text\n    }",
        body_after: " {\r\n            let text = \"Hello 🌍\";\r\n            greet(name);\r\n            text\r\n    }",
    },
    LanguageCase {
        name: "python",
        path: "src/example.py",
        frontend: "tree-sitter-python-0.25.0",
        source: include_str!("fixtures/structural/python.py"),
        generated: true,
        body_before: "def greet(self, name):\n        \"\"\"Function documentation.\"\"\"\n        text = \"Hello 🌍\"\n        print(name)\n        \"a standalone string is not documentation\"\n        return text",
        body_after: "def greet(self, name):\r\n            \"\"\"Function documentation.\"\"\"\r\n            text = \"Hello 🌍\"\r\n            print(name)\r\n            \"a standalone string is not documentation\"\r\n            return text",
    },
    LanguageCase {
        name: "javascript",
        path: "src/example.js",
        frontend: "tree-sitter-typescript-tsx-0.23.2",
        source: include_str!("fixtures/structural/javascript.js"),
        generated: false,
        body_before: "greet(name) {\n    const text = \"Hello 🌍\";\n    console.log(name);\n    return text;\n  }",
        body_after: "greet(name) {\r\n        const text = \"Hello 🌍\";\r\n        console.log(name);\r\n        return text;\r\n  }",
    },
    LanguageCase {
        name: "java",
        path: "src/Greeter.java",
        frontend: "tree-sitter-java-0.23.5",
        source: include_str!("fixtures/structural/java.java"),
        generated: true,
        body_before: "String greet(String name) {\n        String text = \"Hello 🌍\";\n        return name + text;\n    }",
        body_after: "String greet(String name) {\r\n            String text = \"Hello 🌍\";\r\n            return name + text;\r\n    }",
    },
    LanguageCase {
        name: "go",
        path: "src/structural.go",
        frontend: "tree-sitter-go-0.25.0",
        source: include_str!("fixtures/structural/go.go"),
        generated: false,
        body_before: "func (greeter Greeter) Greet(name string) string {\n\ttext := \"Hello 🌍\"\n\tfmt.Println(name)\n\treturn greeter.Prefix + text\n}",
        body_after: "func (greeter Greeter) Greet(name string) string {\r\n\t\ttext := \"Hello 🌍\"\r\n\t\tfmt.Println(name)\r\n\t\treturn greeter.Prefix + text\r\n}",
    },
    LanguageCase {
        name: "typescript",
        path: "src/structural.ts",
        frontend: "tree-sitter-typescript-0.23.2",
        source: include_str!("fixtures/structural/typescript.ts"),
        generated: true,
        body_before: "greet(name: string): string {\n    const text: Greeting = \"Hello 🌍\";\n    logger.info(name);\n    return `${text}, ${name}`;\n  }",
        body_after: "greet(name: string): string {\r\n        const text: Greeting = \"Hello 🌍\";\r\n        logger.info(name);\r\n        return `${text}, ${name}`;\r\n  }",
    },
    LanguageCase {
        name: "c",
        path: "src/structural.c",
        frontend: "tree-sitter-c-0.24.2",
        source: include_str!("fixtures/structural/c.c"),
        generated: false,
        body_before: "int greet(int value) {\n    puts(\"olá\");\n    return value;\n}",
        body_after: "int greet(int value) {\r\n        puts(\"olá\");\r\n        return value;\r\n}",
    },
    LanguageCase {
        name: "cpp",
        path: "src/structural.cpp",
        frontend: "tree-sitter-cpp-0.23.4",
        source: include_str!("fixtures/structural/cpp.cpp"),
        generated: false,
        body_before: "std::string greet(const std::string &name) {\n        return decorate(\"olá\", name);\n    }",
        body_after: "std::string greet(const std::string &name) {\r\n            return decorate(\"olá\", name);\r\n    }",
    },
    LanguageCase {
        name: "csharp",
        path: "src/Structural.cs",
        frontend: "tree-sitter-c-sharp-0.23.5",
        source: include_str!("fixtures/structural/csharp.cs"),
        generated: true,
        body_before: "public string Greet(string name)\n    {\n        return string.Concat(\"olá\", name);\n    }",
        body_after: "public string Greet(string name)\r\n    {\r\n            return string.Concat(\"olá\", name);\r\n    }",
    },
    LanguageCase {
        name: "kotlin",
        path: "src/structural.kt",
        frontend: "tree-sitter-kotlin-ng-1.1.0",
        source: include_str!("fixtures/structural/kotlin.kt"),
        generated: true,
        body_before: "fun greet(name: String): String {\n        println(\"olá\")\n        return name\n    }",
        body_after: "fun greet(name: String): String {\r\n            println(\"olá\")\r\n            return name\r\n    }",
    },
    LanguageCase {
        name: "php",
        path: "src/structural.php",
        frontend: "tree-sitter-php-0.24.2",
        source: include_str!("fixtures/structural/php.php"),
        generated: false,
        body_before: "public function greet(string $name): string\n    {\n        return Formatter::format(\"olá\", $name);\n    }",
        body_after: "public function greet(string $name): string\r\n    {\r\n            return Formatter::format(\"olá\", $name);\r\n    }",
    },
    LUA_CASE,
    RUBY_CASE,
    SWIFT_CASE,
    BASH_CASE,
];

#[derive(Clone, Copy)]
struct LanguageCase {
    name: &'static str,
    path: &'static str,
    frontend: &'static str,
    source: &'static str,
    generated: bool,
    body_before: &'static str,
    body_after: &'static str,
}

#[test]
fn bash_native_declarations_keep_exact_sources_and_body_stable_ids() {
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(BASH_CASE, BASH_CASE.source.as_bytes());
    let analyzer = analyzer(&provider, BASH_CASE);
    let initial_request = request(&fixture.snapshot, &fixture.source, BASH_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    let document = output.document();
    validate_ir_document(document, limits.ir(), &ExtensionSupport::default())
        .expect("valid Bash IR");
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
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    for (name, kind, marker) in [
        ("emit", EntityKind::Function, "function emit()"),
        ("process", EntityKind::Function, "process()"),
        ("render", EntityKind::Function, "render()"),
        ("ROOT", EntityKind::Variable, "ROOT="),
        ("ITEMS", EntityKind::Variable, "ITEMS="),
        ("message", EntityKind::Variable, "message="),
        ("pending", EntityKind::Variable, "pending"),
        ("item", EntityKind::Variable, "item in"),
    ] {
        let found: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect();
        assert_eq!(found.len(), 1, "{name}: {:?}", document.entities);
        let source = found[0]
            .evidence
            .source
            .as_ref()
            .expect("declaration source");
        assert_eq!(source.generation(), fixture.source.generation());
        let start = usize::try_from(source.span().start_byte()).expect("offset fits");
        assert_eq!(start, BASH_CASE.source.find(marker).expect("marker"));
        assert_eq!(found[0].language, "bash");
    }
    for (name, header) in [
        ("emit", "function emit()"),
        ("process", "process()"),
        ("render", "render()"),
    ] {
        let symbol = symbol_id_named(document, name);
        let signatures: Vec<_> = document
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let value = rootlight_ir::decode_lexical_evidence_envelope(extension)
                    .expect("lexical data");
                (value.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && value.subject() == rootlight_ir::FactRef::Entity(symbol))
                .then_some(value)
            })
            .collect();
        assert_eq!(signatures.len(), 1);
        assert_eq!(signatures[0].text(), header);
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    let source = BASH_CASE
        .source
        .replace(BASH_CASE.body_before, BASH_CASE.body_after);
    let changed = fixture.rewrite(source.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, BASH_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    assert!(reparsed.document().diagnostics.is_empty());
    assert_eq!(symbol_ids(document), symbol_ids(reparsed.document()));
    assert_ne!(fixture.source.generation(), changed.source.generation());
}

#[test]
fn real_analyzer_produces_valid_deterministic_ir_for_all_grammars() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();

    for case in CASES {
        let fixture = Fixture::new(case, case.source.as_bytes());
        let analyzer = analyzer(&provider, case);
        assert_descriptor(&analyzer, case);
        let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);

        let first = analyze(&analyzer, &initial_request, &extensions);
        let repeated = analyze(&analyzer, &initial_request, &extensions);
        assert_eq!(
            first.document(),
            repeated.document(),
            "{} logical document changed on repeat",
            case.name
        );
        assert_eq!(
            first.report().coverage(),
            repeated.report().coverage(),
            "{} coverage report changed on repeat",
            case.name
        );
        assert_contract(&first, &fixture, case, &limits, &extensions);

        let variant_source = case.source.replacen(case.body_before, case.body_after, 1);
        assert_ne!(
            variant_source, case.source,
            "{} body fixture did not change",
            case.name
        );
        let variant = fixture.rewrite(variant_source.as_bytes());
        assert_eq!(
            fixture.snapshot.file(),
            variant.snapshot.file(),
            "{} VFS file identity changed after rewrite",
            case.name
        );
        assert_ne!(
            fixture.source.content_hash(),
            variant.source.content_hash(),
            "{} body rewrite did not change content",
            case.name
        );
        assert_ne!(
            fixture.source.generation(),
            variant.source.generation(),
            "{} body rewrite did not advance the generation",
            case.name
        );
        let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
        let reparsed = analyze(&analyzer, &variant_request, &extensions);
        assert_contract(&reparsed, &variant, case, &limits, &extensions);
        assert_eq!(
            symbol_ids(first.document()),
            symbol_ids(reparsed.document()),
            "{} symbol IDs changed after body-only whitespace/CRLF reparse",
            case.name
        );
    }
}

#[test]
fn structural_artifact_reuse_matches_a_clean_generation_analysis() {
    let case = CASES[0];
    let primary_provider = Arc::new(provider());
    let primary_analyzer = analyzer(&primary_provider, case);
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let fixture = Fixture::new(case, b"fn broken(");
    let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let (_, artifact) = primary_analyzer
        .analyze_and_capture(
            &initial_request,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("initial structural artifact is captured");
    let parse_request = initial_request.to_parse_request();
    let required = primary_provider
        .required_syntax_fact_count(&parse_request, &deadline())
        .expect("identity preflight completes");
    assert_eq!(
        artifact
            .required_syntax_fact_count(&deadline())
            .expect("retained identity demand is recoverable"),
        required
    );
    let successor = fixture.next_generation();
    let successor_request = request(&successor.snapshot, &successor.source, case, &limits);

    let reused = primary_analyzer
        .analyze_from_artifact(
            &successor_request,
            &artifact,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("exact structural artifact is reusable");
    let clean = analyze(&primary_analyzer, &successor_request, &extensions);

    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
    assert!(
        !reused.document().diagnostics.is_empty(),
        "malformed fixture must exercise diagnostic rebinding"
    );
    assert!(reused.document().diagnostics.iter().all(|diagnostic| {
        diagnostic.generation == successor.source.generation()
            && diagnostic
                .source
                .as_ref()
                .is_none_or(|source| source.generation() == successor.source.generation())
    }));
    assert_eq!(artifact.file(), successor.snapshot.file());
    assert_eq!(artifact.content_hash(), successor.snapshot.content_hash());
    assert!(artifact.accounted_bytes() > 0);

    let other_provider = Arc::new(provider());
    let other_analyzer = analyzer(&other_provider, case);
    assert!(
        other_analyzer
            .analyze_from_artifact(
                &successor_request,
                &artifact,
                extensions,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_err(),
        "identical public metadata must not authorize cross-provider reuse"
    );
}

#[test]
fn overlapping_captures_preserve_fresh_and_cached_fact_budget_parity() {
    for (language, source) in [
        (
            "rust",
            "/// Reads a value.\nfn read(value: i32) -> i32 { value }\n",
        ),
        (
            "python",
            "def read(value):\n    \"\"\"Reads a value.\"\"\"\n    return value\n",
        ),
        (
            "java",
            "/** Stores values. */\nclass Store { int read(int value) { return value; } }\n",
        ),
    ] {
        let case = *CASES.iter().find(|case| case.name == language).unwrap();
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let fixture = Fixture::new(case, source.as_bytes());
        let initial_limits = limits();
        let initial_request = request(&fixture.snapshot, &fixture.source, case, &initial_limits);
        let (_, artifact) = analyzer
            .analyze_and_capture(
                &initial_request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let reduced_records = artifact.syntax_fact_count().checked_add(1).unwrap();
        let reduced_limits = limits_with_syntax_records(reduced_records);
        assert!(reduced_records < initial_limits.syntax_stream().max_records());
        let successor = fixture.next_generation();
        let successor_request = request(
            &successor.snapshot,
            &successor.source,
            case,
            &reduced_limits,
        );
        let reused = analyzer
            .analyze_from_artifact(
                &successor_request,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let fresh = analyze(&analyzer, &successor_request, &ExtensionSupport::default());
        assert_eq!(reused.document(), fresh.document(), "{language}");
        assert_eq!(reused.report(), fresh.report(), "{language}");
    }
}

#[test]
fn complete_structural_artifact_replays_under_a_smaller_fact_partition() {
    let case = CASES[0];
    let primary_provider = Arc::new(provider());
    let primary_analyzer = analyzer(&primary_provider, case);
    let initial_limits = limits();
    let extensions = ExtensionSupport::default();
    let fixture = Fixture::new(case, b"fn stable() { dependency(); }\n");
    let initial_request = request(&fixture.snapshot, &fixture.source, case, &initial_limits);
    let (_, artifact) = primary_analyzer
        .analyze_and_capture(
            &initial_request,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("complete structural artifact is captured");
    let required_records = artifact.required_syntax_fact_count(&deadline()).unwrap();
    assert!(artifact.syntax_fact_count() > required_records);
    let bounded_limits = limits_with_syntax_records(required_records);
    let bounded_request = request(&fixture.snapshot, &fixture.source, case, &bounded_limits);
    let (bounded, bounded_artifact) = primary_analyzer
        .analyze_and_capture(
            &bounded_request,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("bounded structural artifact is captured");
    assert!(
        bounded
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == "syntax-extraction-limit" })
    );
    assert!(
        !bounded_artifact.is_compatible_with_limits(&initial_limits),
        "an explicitly truncated artifact must not be reused under a larger partition"
    );
    let bounded_parse_request = bounded_request.to_parse_request();
    let required = primary_provider
        .required_syntax_fact_count(&bounded_parse_request, &deadline())
        .expect("identity preflight completes independently of optional truncation");
    assert_eq!(
        bounded_artifact
            .required_syntax_fact_count(&deadline())
            .expect("bounded artifact retains complete identity demand"),
        required
    );

    let reduced_records = artifact
        .syntax_fact_count()
        .checked_add(1)
        .expect("test fact capacity remains bounded");
    let reduced_limits = limits_with_syntax_records(reduced_records);
    assert!(
        reduced_limits.syntax_stream().max_records() < initial_limits.syntax_stream().max_records()
    );
    let successor = fixture.next_generation();
    let successor_request = request(
        &successor.snapshot,
        &successor.source,
        case,
        &reduced_limits,
    );

    assert!(artifact.is_compatible_with_limits(&reduced_limits));
    let reused = primary_analyzer
        .analyze_from_artifact(
            &successor_request,
            &artifact,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("complete output remains reusable under the reduced partition");
    let clean = analyze(&primary_analyzer, &successor_request, &extensions);
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
}

#[test]
fn reviewed_queries_preserve_explicit_call_sites() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();

    for case in CASES.into_iter().filter(|case| {
        matches!(
            case.name,
            "rust"
                | "python"
                | "javascript"
                | "go"
                | "typescript"
                | "c"
                | "cpp"
                | "csharp"
                | "kotlin"
                | "php"
        )
    }) {
        let fixture = Fixture::new(case, case.source.as_bytes());
        let analyzer = analyzer(&provider, case);
        let request = request(&fixture.snapshot, &fixture.source, case, &limits);
        let output = analyze(&analyzer, &request, &extensions);

        assert!(
            output
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::CallSite),
            "{} reviewed query omitted every call site",
            case.name
        );
    }
}

#[test]
fn lua_local_references_obey_visibility_shadowing_and_closure_boundaries() {
    let source = "local outer = 1\nlocal index = 4\ndo\n  local outer = outer\n  local function capture(parameter)\n    local snapshot = outer\n    local recursive = capture\n    return parameter\n  end\n  local assigned = function() return assigned end\nend\nrepeat\n  local ready = outer\nuntil ready\nfor index = index, 8 do\n  consume(index)\nend\nreturn outer\n";
    assert_lua_reference_bindings(
        source,
        &[
            ("local outer = outer", "outer", Some("local outer = 1")),
            (
                "local snapshot = outer",
                "outer",
                Some("local outer = outer"),
            ),
            (
                "local recursive = capture",
                "capture",
                Some("local function capture"),
            ),
            ("return parameter", "parameter", Some("parameter)")),
            ("return assigned", "assigned", None),
            ("local ready = outer", "outer", Some("local outer = 1")),
            ("until ready", "ready", Some("local ready")),
            ("for index = index", "index", Some("local index")),
            ("consume(index)", "index", Some("index = index")),
            ("return outer", "outer", Some("local outer = 1")),
        ],
    );
}

#[test]
fn ruby_parameterless_methods_have_source_backed_signatures_and_stable_ids() {
    let source = "class Store\n def ready\n  true\n end\n def self.build\n  nil\n end\n def version = 1\n def -@\n  self\n end\nend\n";
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(RUBY_CASE, source.as_bytes());
    let analyzer = analyzer(&provider, RUBY_CASE);
    let initial_request = request(&fixture.snapshot, &fixture.source, RUBY_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    assert!(output.document().diagnostics.is_empty());
    assert_eq!(
        output.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(output.document().skipped_regions.is_empty());
    for (name, header) in [
        ("ready", "def ready"),
        ("self.build", "def self.build"),
        ("version", "def version"),
        ("-@", "def -@"),
    ] {
        let symbol = symbol_id_named(output.document(), name);
        let signatures = output
            .document()
            .extensions
            .iter()
            .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|extension| {
                let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension)
                    .expect("lexical evidence validates");
                (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && lexical.subject() == rootlight_ir::FactRef::Entity(symbol))
                .then_some((extension, lexical))
            })
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1, "{name}");
        let (extension, lexical) = &signatures[0];
        assert_eq!(lexical.text(), header);
        assert!(!lexical.is_truncated());
        let reference = extension
            .evidence
            .source
            .as_ref()
            .expect("header has source");
        assert_eq!(reference.generation(), fixture.source.generation());
        let start = usize::try_from(reference.span().start_byte()).expect("span start fits");
        let end = usize::try_from(reference.span().end_byte()).expect("span end fits");
        assert_eq!(source.get(start..end), Some(header));
    }
    let changed_source = source
        .replace("true", "false")
        .replace("version = 1", "version = 200");
    let changed = fixture.rewrite(changed_source.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, RUBY_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    assert_ne!(fixture.source.content_hash(), changed.source.content_hash());
    assert_ne!(fixture.source.generation(), changed.source.generation());
    assert_eq!(
        reparsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        symbol_ids(output.document()),
        symbol_ids(reparsed.document())
    );
}

#[test]
fn ruby_declarations_and_operator_methods_preserve_exact_identities() {
    let source = "module Garden\n class Store\n  DEFAULT = 1\n  def read(key)\n   @value = key\n  end\n  def self.read(key)\n   new(key)\n  end\n  def [](key)\n   key\n  end\n  def []=(key, value)\n   value\n  end\n  def /(other)\n   other\n  end\n end\nend\n";
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(RUBY_CASE, source.as_bytes());
    let analyzer = analyzer(&provider, RUBY_CASE);
    let request = request(&fixture.snapshot, &fixture.source, RUBY_CASE, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    assert!(output.document().diagnostics.is_empty());
    for (name, kind, marker) in [
        ("Garden", EntityKind::Namespace, "module Garden"),
        ("Store", EntityKind::Class, "class Store"),
        ("DEFAULT", EntityKind::Constant, "DEFAULT = 1"),
        ("read", EntityKind::Method, "def read(key)"),
        ("self.read", EntityKind::Method, "def self.read(key)"),
        ("[]", EntityKind::Method, "def [](key)"),
        ("[]=", EntityKind::Method, "def []=(key, value)"),
        ("/", EntityKind::Method, "def /(other)"),
        ("@value", EntityKind::Field, "@value = key"),
    ] {
        let matches = output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(matches.len(), 1, "{name}");
        let reference = matches[0]
            .evidence
            .source
            .as_ref()
            .expect("declaration has source");
        assert_eq!(reference.generation(), fixture.source.generation());
        assert_eq!(
            reference.span().start_byte(),
            u64::try_from(source.find(marker).expect("declaration marker exists"))
                .expect("fixture offset fits")
        );
    }
}

#[test]
fn css_declarations_preserve_exact_sources_and_body_stable_identity() {
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(CSS_CASE, CSS_CASE.source.as_bytes());
    let analyzer = analyzer(&provider, CSS_CASE);
    let initial_request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    let document = output.document();
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_2);
    validate_ir_document(document, limits.ir(), &ExtensionSupport::default())
        .expect("source-bound CSS IR validates");
    let expected = [
        ("src/theme.css", EntityKind::Module, CSS_CASE.source),
        (
            ":root",
            EntityKind::StyleRule,
            ":root { --accent: #1a2b3c; }",
        ),
        ("--accent", EntityKind::Property, "--accent: #1a2b3c;"),
        (
            ".card > .π, [data-label=\"A B\"]",
            EntityKind::StyleRule,
            ".card > .π, [data-label=\"A B\"] {\n  color: var(--accent);\n  & > .icon { opacity: 0.8; }\n}",
        ),
        (
            "& > .icon",
            EntityKind::StyleRule,
            "& > .icon { opacity: 0.8; }",
        ),
        (
            ".responsive",
            EntityKind::StyleRule,
            ".responsive { display: grid; }",
        ),
        (
            "spin",
            EntityKind::Keyframes,
            "@keyframes spin {\n  from { transform: rotate(0deg); }\n  to { transform: rotate(360deg); }\n}",
        ),
    ];
    assert_eq!(
        document.entities.len(),
        expected.len(),
        "{:?}",
        document.entities
    );
    for (name, kind, declaration) in expected {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .expect("each CSS declaration survives lowering");
        assert_eq!(entity.language, "css");
        let evidence = entity
            .evidence
            .source
            .as_ref()
            .expect("exact declaration source");
        assert_eq!(evidence.generation(), fixture.source.generation());
        assert_eq!(evidence.content_hash(), fixture.source.content_hash());
        assert_eq!(evidence.span().file(), fixture.source.span().file());
        if kind != EntityKind::Module {
            let definition = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .expect("definition targets its exact entity");
            let span = definition.source.span();
            let start = usize::try_from(span.start_byte()).expect("fixture offset fits");
            let end = usize::try_from(span.end_byte()).expect("fixture offset fits");
            assert_eq!(CSS_CASE.source.get(start..end), Some(name));
            assert_eq!(
                definition.syntactic_text_hash,
                content_hash(name.as_bytes())
            );
            assert_eq!(definition.evidence.source.as_ref(), Some(evidence));
            let declaration_span = evidence.span();
            let start = usize::try_from(declaration_span.start_byte()).expect("offset");
            let end = usize::try_from(declaration_span.end_byte()).expect("offset");
            assert_eq!(CSS_CASE.source.get(start..end), Some(declaration));
        }
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    let changed_source = CSS_CASE
        .source
        .replace(CSS_CASE.body_before, CSS_CASE.body_after);
    let changed = fixture.rewrite(changed_source.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, CSS_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    assert_eq!(symbol_ids(document), symbol_ids(reparsed.document()));
    assert!(reparsed.document().entities.iter().all(|entity| {
        entity.evidence.source.as_ref().is_some_and(|source| {
            source.generation() == changed.source.generation()
                && source.content_hash() == changed.source.content_hash()
        })
    }));
}

#[test]
fn css_selector_identity_preserves_escape_terminators_and_non_ascii_whitespace() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let limits = limits();
    let mut identities = BTreeSet::new();
    for selector in [
        r".\31 a",
        r".\31  a",
        ".a\u{a0}b",
        ".a b",
        ".π > .🚀",
        "[title=\"a  b\"]",
        ".card,\r\n.card2",
    ] {
        let source = format!("{selector} {{ color: red; }}\n");
        let fixture = Fixture::new(CSS_CASE, source.as_bytes());
        let request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
        let output = analyze(&analyzer, &request, &ExtensionSupport::default());
        let rules = output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::StyleRule)
            .collect::<Vec<_>>();
        assert_eq!(rules.len(), 1, "{selector:?}");
        assert_eq!(rules[0].canonical_name, selector);
        assert!(
            identities.insert(rules[0].id),
            "distinct selectors cannot share identity"
        );
    }
}

#[test]
fn css_conditional_rules_keep_distinct_contexts_and_all_definition_sources() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let limits = limits();
    let source = concat!(
        ".card { --accent: red; }\n",
        ".card { --accent: blue; --accent: green; }\n",
        "@media screen { .card { --accent: yellow; } }\n",
        "@media print { .card { --accent: black; } }\n",
        "@supports (display: grid) { .card { --accent: purple; } }\n",
        "@media screen { .card { --accent: orange; } }\n",
    );
    let fixture = Fixture::new(CSS_CASE, source.as_bytes());
    let initial_request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    let document = output.document();
    let mut property_groups = BTreeMap::new();
    for occurrence in &document.occurrences {
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            continue;
        };
        if occurrence.role != OccurrenceRole::Definition {
            continue;
        }
        let context = occurrence
            .evidence
            .source
            .as_ref()
            .expect("declaration context");
        let start = usize::try_from(context.span().start_byte()).expect("fixture offset");
        let end = usize::try_from(context.span().end_byte()).expect("fixture offset");
        if let Some(declaration) = source
            .get(start..end)
            .filter(|text| text.starts_with("--accent:"))
        {
            property_groups.insert(declaration, symbol);
        }
    }
    assert_eq!(property_groups.len(), 7);
    assert_eq!(
        property_groups["--accent: red;"],
        property_groups["--accent: blue;"]
    );
    assert_eq!(
        property_groups["--accent: red;"],
        property_groups["--accent: green;"]
    );
    assert_eq!(
        property_groups["--accent: yellow;"],
        property_groups["--accent: orange;"]
    );
    assert_eq!(
        ["red", "yellow", "black", "purple"]
            .map(|value| { property_groups[format!("--accent: {value};").as_str()] })
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        4
    );
    for (kind, expected_symbols, expected_definitions) in
        [(EntityKind::StyleRule, 4, 6), (EntityKind::Property, 4, 7)]
    {
        let symbols = document
            .entities
            .iter()
            .filter(|entity| entity.kind == kind)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            symbols.len(),
            expected_symbols,
            "{kind:?}: {:?}",
            document.entities
        );
        let definitions = document.occurrences.iter().filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition && matches!(
                occurrence.target, OccurrenceTarget::Resolved { symbol } if symbols.contains(&symbol)
            )
        }).collect::<Vec<_>>();
        assert_eq!(definitions.len(), expected_definitions, "{kind:?}");
        let expected_spans = if kind == EntityKind::Property {
            source
                .match_indices("--accent:")
                .map(|(start, _)| {
                    let end =
                        start + source[start..].find(';').expect("declaration terminator") + 1;
                    (start, end)
                })
                .collect::<BTreeSet<_>>()
        } else {
            source
                .match_indices(".card {")
                .map(|(start, _)| {
                    let end = start + source[start..].find('}').expect("rule terminator") + 1;
                    (start, end)
                })
                .collect::<BTreeSet<_>>()
        };
        let observed_spans = definitions
            .iter()
            .map(|occurrence| {
                assert_eq!(occurrence.source.generation(), fixture.source.generation());
                assert_eq!(
                    occurrence.source.content_hash(),
                    fixture.source.content_hash()
                );
                (
                    usize::try_from(
                        occurrence
                            .evidence
                            .source
                            .as_ref()
                            .expect("declaration context")
                            .span()
                            .start_byte(),
                    )
                    .expect("fixture offset"),
                    usize::try_from(
                        occurrence
                            .evidence
                            .source
                            .as_ref()
                            .expect("declaration context")
                            .span()
                            .end_byte(),
                    )
                    .expect("fixture offset"),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(observed_spans, expected_spans);
    }
    let changed_text = format!(
        ".unrelated {{ color: pink; }}\n{}",
        source.replace("yellow", "gold")
    );
    let changed = fixture.rewrite(changed_text.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, CSS_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    let old_ids = document
        .entities
        .iter()
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    let new_ids = reparsed
        .document()
        .entities
        .iter()
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert!(
        old_ids.is_subset(&new_ids),
        "body and unrelated sibling edits preserve all groups"
    );
}

#[test]
fn css_nested_context_headers_preserve_raw_identity_without_body_offsets() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let limits = limits();
    let contexts = [
        "@layer base { @media screen { .card { --accent: red; } } }",
        "@layer theme { @media screen { .card { --accent: red; } } }",
        "@media screen { @layer base { .card { --accent: red; } } }",
        "@container narrow (width > 20px) { .card { --accent: red; } }",
        "@container wide (width > 20px) { .card { --accent: red; } }",
        "@scope (.panel) { .card { --accent: red; } }",
        "@scope (.dialog) { .card { --accent: red; } }",
        "@supports (content: \"{\") { .card { --accent: red; } }",
        r"@layer \31 a { .card { --accent: red; } }",
        r"@layer \31  a { .card { --accent: red; } }",
    ];
    let source = contexts.join("\n");
    let fixture = Fixture::new(CSS_CASE, source.as_bytes());
    let initial_request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    let ids = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids.len(), contexts.len());
    let changed_source = contexts
        .iter()
        .rev()
        .map(|context| context.replace("red", "purple"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let changed = fixture.rewrite(changed_source.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, CSS_CASE, &limits);
    let output = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    let changed_ids = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(ids, changed_ids);
}

#[test]
fn css_keyframe_steps_keep_each_custom_property_definition() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let limits = limits();
    let source = "@keyframes pulse { from { --accent: red; } 50% { --accent: blue; } to { --accent: green; } }";
    let fixture = Fixture::new(CSS_CASE, source.as_bytes());
    let request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    let symbols = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .map(|entity| entity.id)
        .collect::<BTreeSet<_>>();
    assert_eq!(symbols.len(), 3);
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && matches!(occurrence.target,
            OccurrenceTarget::Resolved { symbol } if symbols.contains(&symbol))
            })
            .count(),
        3
    );
}

#[test]
fn css_incomplete_contexts_keep_file_evidence_without_fabricated_definitions() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let limits = limits();
    for source in [
        "@media screen",
        "@supports (display:",
        "@scope (.card)",
        "@layer base;",
        "@layer base {",
        "@keyframes pulse { from",
    ] {
        let fixture = Fixture::new(CSS_CASE, source.as_bytes());
        let request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
        let output = analyze(&analyzer, &request, &ExtensionSupport::default());
        assert_eq!(output.document().files.len(), 1, "{source}");
        assert_eq!(
            output.document().files[0].content_hash,
            fixture.source.content_hash()
        );
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| matches!(entity.kind, EntityKind::StyleRule | EntityKind::Property)),
            "{source}"
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn css_custom_properties_preserve_case_and_unicode_codepoint_identity() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CSS_CASE);
    let limits = limits();
    let names = [
        "--foo",
        "--FOO",
        "--foó",
        "--foo\u{301}",
        r"--\61 ccent",
        "--α",
    ];
    let declarations = names
        .iter()
        .map(|name| format!("{name}: red;"))
        .collect::<Vec<_>>()
        .join("\n");
    let source = format!(":root {{ {declarations} }}\n");
    let fixture = Fixture::new(CSS_CASE, source.as_bytes());
    let request = request(&fixture.snapshot, &fixture.source, CSS_CASE, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    let properties = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect::<Vec<_>>();
    assert_eq!(properties.len(), names.len());
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.canonical_name.as_str())
            .collect::<BTreeSet<_>>(),
        names.into_iter().collect::<BTreeSet<_>>()
    );
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        names.len()
    );
}

#[test]
fn swift_kinds_and_extension_methods_preserve_source_and_body_stable_identity() {
    let provider = Arc::new(provider());
    let limits = limits();
    let source = SWIFT_CASE.source;
    let fixture = Fixture::new(SWIFT_CASE, source.as_bytes());
    let analyzer = analyzer(&provider, SWIFT_CASE);
    let initial_request = request(&fixture.snapshot, &fixture.source, SWIFT_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    assert_contract(
        &output,
        &fixture,
        SWIFT_CASE,
        &limits,
        &ExtensionSupport::default(),
    );
    for (name, kind, marker) in [
        ("Store", EntityKind::Protocol, "protocol Store"),
        ("Entry", EntityKind::Struct, "struct Entry"),
        ("Cache", EntityKind::Class, "class Cache"),
        ("Worker", EntityKind::Class, "actor Worker"),
        ("Result", EntityKind::Enum, "enum Result"),
        ("render", EntityKind::Method, "func render"),
        ("copy", EntityKind::Method, "func copy"),
        ("greet", EntityKind::Function, "func greet"),
        ("load", EntityKind::Method, "func load"),
        ("init", EntityKind::Constructor, "init()"),
        ("deinit", EntityKind::Method, "deinit {}"),
        ("value", EntityKind::Property, "value: String"),
        ("size", EntityKind::Property, "size = 1"),
        ("title", EntityKind::Variable, "title ="),
        ("Label", EntityKind::TypeAlias, "typealias Label"),
        ("ready", EntityKind::Constant, "ready, missing"),
        ("missing", EntityKind::Constant, "missing\n"),
        ("loaded", EntityKind::Constructor, "loaded(String)"),
        ("key", EntityKind::Parameter, "_ key: String"),
        ("name", EntityKind::Parameter, "_ name: String"),
    ] {
        let matches = output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(
            matches.len(),
            1,
            "{name}: {:?}",
            output
                .document()
                .entities
                .iter()
                .map(|e| (&e.canonical_name, e.kind))
                .collect::<Vec<_>>()
        );
        let reference = matches[0]
            .evidence
            .source
            .as_ref()
            .expect("declaration has exact source");
        assert_eq!(reference.generation(), fixture.source.generation());
        assert_eq!(
            reference.span().start_byte(),
            u64::try_from(source.find(marker).expect("marker exists")).expect("offset fits"),
            "{name}"
        );
    }
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "Entry")
            .count(),
        1,
        "extension is not another type definition"
    );
    let method = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "copy")
        .expect("extension method");
    assert!(
        method.qualified_name.contains("Entry"),
        "{}",
        method.qualified_name
    );
    let changed_source = source.replace("return self", "return  self");
    let changed = Fixture::new(SWIFT_CASE, changed_source.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, SWIFT_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    assert_eq!(
        symbol_ids(output.document()),
        symbol_ids(reparsed.document())
    );
}

#[test]
fn swift_mixed_enum_cases_and_constrained_extensions_keep_distinct_identity() {
    let source = "struct Box<T> {}\nextension Box where T: Equatable {\n  func matches() -> Bool {\n    func helper() -> Bool { return true }\n    return helper()\n  }\n}\nextension Box where T: Hashable {\n  func matches() -> Bool { return false }\n}\nenum Response { case empty, value(Int), missing, pair(Int, Int) }\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, SWIFT_CASE);
    let limits = limits();
    let fixture = Fixture::new(SWIFT_CASE, source.as_bytes());
    let initial_request = request(&fixture.snapshot, &fixture.source, SWIFT_CASE, &limits);
    let output = analyze(&analyzer, &initial_request, &ExtensionSupport::default());
    for (name, kind) in [
        ("empty", EntityKind::Constant),
        ("value", EntityKind::Constructor),
        ("missing", EntityKind::Constant),
        ("pair", EntityKind::Constructor),
        ("helper", EntityKind::Function),
    ] {
        let entities = output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 1, "{name}");
        assert_eq!(entities[0].kind, kind, "{name}");
    }
    let methods = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "matches")
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 2);
    assert!(methods.iter().all(|entity| {
        entity.kind == EntityKind::Method && entity.qualified_name.contains("Box")
    }));
    assert_ne!(methods[0].id, methods[1].id);
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "Box")
            .count(),
        1
    );
    let changed_source = source.replace("return true", "return false");
    let changed = Fixture::new(SWIFT_CASE, changed_source.as_bytes());
    let changed_request = request(&changed.snapshot, &changed.source, SWIFT_CASE, &limits);
    let reparsed = analyze(&analyzer, &changed_request, &ExtensionSupport::default());
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        reparsed
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
}

#[test]
fn lua_callee_reads_resolve_bindings_without_claiming_runtime_call_targets() {
    let source = "local callback = factory()\ncallback()\ndo\n  local callback = callback()\n  callback(1)\nend\nlocal function recurse(value)\n  recurse(value)\nend\nlocal record = {}\nrecord.callback()\nrecord:callback()\n";
    assert_lua_reference_bindings(
        source,
        &[
            ("factory()", "factory", None),
            ("\ncallback()", "callback", Some("local callback = factory")),
            (
                "local callback = callback()",
                "callback",
                Some("local callback = factory"),
            ),
            ("callback(1)", "callback", Some("local callback = callback")),
            (
                "recurse(value)\nend",
                "recurse",
                Some("local function recurse"),
            ),
            ("record.callback()", "record", Some("local record")),
            ("record:callback()", "record", Some("local record")),
        ],
    );
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(LUA_CASE, source.as_bytes());
    let analyzer = analyzer(&provider, LUA_CASE);
    let request = request(&fixture.snapshot, &fixture.source, LUA_CASE, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    let calls = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 7);
    assert!(
        calls
            .iter()
            .all(|call| matches!(call.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        !output
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == rootlight_ir::RelationPredicate::Calls)
    );
}

#[test]
fn lua_implicit_self_and_unavailable_inner_bindings_do_not_resolve_to_outer_names() {
    let source = "local self = {}\nlocal value = 0\nfunction M:run() return self end\nfunction M:explicit(self) return self end\ndo local value = 1; consume(value) end\ndo local value = 2; consume(value) end\nreturn value\n";
    assert_lua_reference_bindings(
        source,
        &[
            ("return self", "self", None),
            (
                "explicit(self) return self",
                "self",
                Some("self) return self"),
            ),
            ("value = 1; consume(value)", "value", None),
            ("value = 2; consume(value)", "value", None),
            ("return value", "value", Some("local value = 0")),
        ],
    );
}

#[test]
fn lua_computed_keys_resolve_but_literal_keys_and_runtime_calls_do_not() {
    let source = "local key = 1\nlocal record = { key = key, [key] = key }\nlocal literal = record.key\nlocal method = record:key()\n::key::\ngoto key\n";
    assert_lua_reference_bindings(
        source,
        &[
            ("key = key,", "key", Some("local key")),
            ("[key]", "key", Some("local key")),
            ("[key] = key", "key", Some("local key")),
            ("literal = record", "record", Some("local record")),
            ("method = record", "record", Some("local record")),
        ],
    );
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(LUA_CASE, source.as_bytes());
    let analyzer = analyzer(&provider, LUA_CASE);
    let request = request(&fixture.snapshot, &fixture.source, LUA_CASE, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    for (marker, offset) in [
        ("key = key,", 0),
        ("record.key", "record.".len()),
        ("::key::", 2),
        ("goto key", "goto ".len()),
    ] {
        let start = source.find(marker).expect("literal key exists") + offset;
        assert!(!output.document().occurrences.iter().any(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && occurrence.source.span().start_byte()
                    == u64::try_from(start).expect("offset fits")
        }));
    }
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
}

#[test]
fn lua_bounded_capture_plans_do_not_assert_exact_lexical_targets() {
    let source = format!(
        "local value = 0\ndo\n local value = value\n{}end\nreturn value\n",
        " value = value\n".repeat(40)
    );
    let provider = Arc::new(provider());
    let limits = limits_with_syntax_records(48);
    let fixture = Fixture::new(LUA_CASE, source.as_bytes());
    let analyzer = analyzer(&provider, LUA_CASE);
    let request = request(&fixture.snapshot, &fixture.source, LUA_CASE, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    let references = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert!(
        !references.is_empty(),
        "bounded text references remain available"
    );
    assert!(
        references
            .iter()
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
}

fn assert_lua_reference_bindings(source: &str, expected: &[(&str, &str, Option<&str>)]) {
    assert_lua_reference_bindings_in(LUA_CASE, source, expected);
    let markdown = LanguageCase {
        name: "markdown",
        path: "docs/example.md",
        frontend: "tree-sitter-md-0.5.3",
        ..LUA_CASE
    };
    assert_lua_reference_bindings_in(markdown, &format!("~~~lua\n{source}\n~~~\n"), expected);
}

fn assert_lua_reference_bindings_in(
    case: LanguageCase,
    source: &str,
    expected: &[(&str, &str, Option<&str>)],
) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let limits = limits();
    let fixture = Fixture::new(case, source.as_bytes());
    let analyzer = analyzer(&provider, case);
    let request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    let document = output.document();
    for &(marker, name, declaration) in expected {
        let start = source.find(marker).expect("reference marker exists")
            + marker.rfind(name).expect("reference name exists");
        let occurrence = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Reference
                    && occurrence.source.span().start_byte()
                        == u64::try_from(start).expect("fixture offset fits")
                    && occurrence.source.span().end_byte()
                        == u64::try_from(start + name.len()).expect("fixture end fits")
            })
            .expect("exact Lua reference is retained");
        if let Some(declaration) = declaration {
            let expected_start =
                u64::try_from(source.find(declaration).expect("declaration marker exists"))
                    .expect("fixture offset fits");
            let entity = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == name
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().start_byte() == expected_start)
                })
                .expect("source-backed lexical binding exists");
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: entity.id },
                "{marker}"
            );
            assert!(
                document.relations.iter().any(|relation| {
                    relation.subject == rootlight_ir::RelationEndpoint::Occurrence(occurrence.id)
                        && relation.predicate == rootlight_ir::RelationPredicate::RefersTo
                        && relation.object == rootlight_ir::RelationEndpoint::Entity(entity.id)
                        && relation.evidence.source.as_ref() == Some(&occurrence.source)
                }),
                "{marker}"
            );
        } else {
            assert!(
                matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                "{marker}"
            );
        }
    }
    output
}

#[test]
fn lua_bindings_and_qualified_functions_keep_source_backed_identities() {
    let case = LUA_CASE;
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let fixture = Fixture::new(case, case.source.as_bytes());
    let analyzer = analyzer(&provider, case);
    let request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let output = analyze(&analyzer, &request, &extensions);
    let document = output.document();
    for (name, kind) in [
        ("M", EntityKind::Variable),
        ("second", EntityKind::Variable),
        ("left", EntityKind::Variable),
        ("right", EntityKind::Variable),
        ("limit", EntityKind::Constant),
        ("transform", EntityKind::Function),
        ("M.map", EntityKind::Function),
        ("M:run", EntityKind::Method),
        ("M.finish", EntityKind::Function),
        ("index", EntityKind::Variable),
        ("key", EntityKind::Variable),
        ("entry", EntityKind::Variable),
        ("value", EntityKind::Parameter),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| { entity.canonical_name == name && entity.kind == kind }),
            "missing Lua {kind:?} {name}"
        );
    }
    let mut actual = BTreeMap::new();
    for entity in &document.entities {
        if entity.kind != EntityKind::Module {
            *actual
                .entry(entity.canonical_name.as_str())
                .or_insert(0usize) += 1;
        }
    }
    let mut expected = [
        "M",
        "M.finish",
        "M.map",
        "M:run",
        "alpha",
        "beta",
        "block",
        "callback",
        "close",
        "dependency",
        "entry",
        "first",
        "first_fn",
        "handlers",
        "index",
        "item",
        "key",
        "left",
        "limit",
        "name",
        "quoted",
        "require",
        "right",
        "second",
        "second_fn",
        "transform",
        "value",
    ]
    .into_iter()
    .map(|name| (name, 1usize))
    .collect::<BTreeMap<_, _>>();
    expected.insert("first", 2);
    expected.insert("value", 6);
    assert_eq!(
        actual, expected,
        "every source binding must survive IR lowering"
    );
    for name in ["invented", "also_invented", "comment_only"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    assert!(
        document
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "syntax-error-recovery")
    );
    assert_ne!(
        symbol_id_named(document, "M.map"),
        symbol_id_named(document, "M:run")
    );
}

#[test]
fn lua_qualified_name_trivia_preserves_identity_without_inventing_dynamic_receivers() {
    let case = LUA_CASE;
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(
        case,
        b"function M.nested:run(value) return value end\nfunction N.run() end\n",
    );
    let first_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let first = analyze(&analyzer, &first_request, &extensions);
    let variant = fixture.rewrite(b"function M --[=[qualifier]=]\n . nested : run(value) return value end\nfunction N.run() end\n");
    let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
    let second = analyze(&analyzer, &variant_request, &extensions);
    for name in ["M.nested:run", "N.run"] {
        assert_eq!(
            symbol_id_named(first.document(), name),
            symbol_id_named(second.document(), name)
        );
    }
    let dynamic = fixture.rewrite(b"factory().run = function(value) return value end\n");
    let dynamic_request = request(&dynamic.snapshot, &dynamic.source, case, &limits);
    let output = analyze(&analyzer, &dynamic_request, &extensions);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| matches!(entity.kind, EntityKind::Function | EntityKind::Method))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|region| region.domain == FactDomain::Entities
                && region.detail == "declaration-name-unavailable")
    );
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
}

#[test]
fn lua_identical_sibling_scopes_remain_explicitly_ambiguous() {
    let case = LUA_CASE;
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let analyzer = analyzer(&provider, case);
    let fixture = Fixture::new(case, b"do local value = 1 end\ndo local value = 2 end\n");
    let request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let output = analyze(&analyzer, &request, &extensions);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "value")
    );
    assert!(output.document().skipped_regions.iter().any(|region| {
        region.domain == FactDomain::Entities
            && region.detail == "stable-scope-identity-unavailable"
    }));
    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
}

#[test]
fn javascript_typed_jsx_preserves_structural_declarations_without_recovery() {
    let case = CASES[2];
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let fixture = Fixture::new(
        case,
        br#"// @flow
export function renderLabel(input: InputValue): React.Node {
  return <span>{input.label}</span>;
}
"#,
    );
    let analyzer = analyzer(&provider, case);
    let request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let output = analyze(&analyzer, &request, &extensions);

    assert!(output.document().entities.iter().any(|entity| {
        entity.kind == EntityKind::Function && entity.canonical_name == "renderLabel"
    }));
    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "syntax-error-recovery")
    );
}

#[test]
fn typescript_tsx_preserves_jsx_without_breaking_plain_type_assertions() {
    let provider = Arc::new(provider());
    let limits = limits();
    for (path, source, name) in [
        (
            "src/view.tsx",
            "export function View(props: Props) { return <Panel title={props.title}><span>Hello</span></Panel>; }\n",
            "View",
        ),
        (
            "src/convert.ts",
            "export function convert(value: unknown) { return <number>value; }\n",
            "convert",
        ),
    ] {
        let case = LanguageCase { path, ..CASES[5] };
        let fixture = Fixture::new(case, source.as_bytes());
        let analyzer = analyzer(&provider, case);
        let request = request(&fixture.snapshot, &fixture.source, case, &limits);
        let output = analyze(&analyzer, &request, &ExtensionSupport::default());
        assert!(
            output
                .document()
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "syntax-error-recovery"),
            "{path}"
        );
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function),
            "{path}"
        );
    }
}

#[test]
fn tsx_structural_artifact_rebinds_to_a_clean_generation() {
    let provider = Arc::new(provider());
    let limits = limits();
    let case = LanguageCase {
        path: "src/view.tsx",
        ..CASES[5]
    };
    let fixture = Fixture::new(
        case,
        b"export function View() { return <span>Hello</span>; }\n",
    );
    let analyzer = analyzer(&provider, case);
    let initial = request(&fixture.snapshot, &fixture.source, case, &limits);
    let (_, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("TSX captures a complete structural artifact");
    assert!(
        artifact
            .required_syntax_fact_count(&deadline())
            .expect("identity demand is retained")
            > 0
    );
    let successor = fixture.next_generation();
    let successor_request = request(&successor.snapshot, &successor.source, case, &limits);
    let reused = analyzer
        .analyze_from_artifact(
            &successor_request,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("TSX artifact rebinds");
    let clean = analyze(&analyzer, &successor_request, &ExtensionSupport::default());
    assert_eq!(reused.document(), clean.document());
    assert_eq!(reused.report(), clean.report());
    assert!(reused.document().diagnostics.is_empty());
}

#[test]
fn reviewed_rust_structural_profile_reports_tier_b_without_tier_a_claims() {
    let case = CASES[0];
    let rust_provider = Arc::new(provider());
    let parser: Arc<dyn ParseProvider> = rust_provider;
    let analyzer = TreeSitterAnalyzer::new_rust_structural(
        parser,
        producer_identity(),
        language(case),
        case.frontend,
        content_hash(BINARY_SEED),
    )
    .expect("reviewed Rust structural profile is valid");
    let limits = limits();
    let fixture = Fixture::new(case, case.source.as_bytes());
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source)
            .expect("snapshot binds to source"),
        language(case),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierB,
        build_context(),
        &limits,
    )
    .expect("Tier B Rust request is valid")
    .with_generated_status(case.generated);

    let output = analyze(&analyzer, &request, &ExtensionSupport::default());

    assert_eq!(analyzer.descriptor().tier(), AnalysisTier::TierB);
    assert!(
        output
            .document()
            .provenance
            .iter()
            .all(|record| record.tier == AnalysisTier::TierB)
    );
    assert!(
        output
            .document()
            .entities
            .iter()
            .all(|entity| entity.tier == AnalysisTier::TierB)
    );
    assert!(
        output
            .document()
            .coverage_records
            .iter()
            .all(|record| record.tier == AnalysisTier::TierB)
    );

    let parser: Arc<dyn ParseProvider> = Arc::new(provider());
    assert!(matches!(
        TreeSitterAnalyzer::new_rust_structural(
            parser,
            producer_identity(),
            language(CASES[1]),
            CASES[1].frontend,
            content_hash(BINARY_SEED),
        ),
        Err(
            rootlight_adapter_treesitter::TreeSitterAnalyzerConfigError::UnsupportedRustStructuralLanguage
        )
    ));
}

#[test]
fn reviewed_rust_structural_profile_marks_tests_and_scoped_calls() {
    const SOURCE: &str =
        "#[test]\nfn checks_handler() { crate::worker::handle(); }\nfn handle() {}\n";
    let case = CASES[0];
    let fixture = Fixture::new(case, SOURCE.as_bytes());
    let parser: Arc<dyn ParseProvider> = Arc::new(provider());
    let analyzer = TreeSitterAnalyzer::new_rust_structural(
        parser,
        producer_identity(),
        language(case),
        case.frontend,
        content_hash(BINARY_SEED),
    )
    .expect("reviewed Rust structural profile is valid");
    let limits = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source)
            .expect("snapshot binds to source"),
        language(case),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierB,
        build_context(),
        &limits,
    )
    .expect("Tier B Rust request is valid")
    .with_generated_status(false);

    let output = analyze(&analyzer, &request, &ExtensionSupport::default());
    let test = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "checks_handler")
        .expect("test function is indexed");
    assert!(test.flags.contains(&EntityFlag::Test));
    let scoped_call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.syntactic_text_hash == content_hash(b"handle")
        })
        .expect("qualified scoped call is captured");
    assert_eq!(
        scoped_call.syntax_kind, "rust.scoped_call.scoped_call",
        "the reviewed Rust profile preserves scoped-call evidence for resolution"
    );
    assert_eq!(
        scoped_call.source.span().end_byte() - scoped_call.source.span().start_byte(),
        u64::try_from("crate::worker::handle".len()).expect("fixture length fits")
    );
}

#[test]
fn reviewed_terminal_call_names_keep_complete_call_evidence() {
    let cases = [
        (
            CASES[6],
            "const char *uv_err_name(int error);\nvoid echo(int error) { uv_err_name(error); }\n",
            "uv_err_name",
            "uv_err_name(error)",
        ),
        (
            CASES[7],
            "struct Props {};\nstruct Parser { template<class T> void prepare(); };\nvoid test(Parser& parser) { parser.prepare<Props>(); }\n",
            "prepare",
            "parser.prepare<Props>()",
        ),
        (
            CASES[8],
            "class WildcardPattern { static void Init(string value, object options) {} bool IsMatch(string value, object options) { WildcardPattern.Init(value, options); return true; } }\n",
            "Init",
            "WildcardPattern.Init(value, options)",
        ),
        (
            CASES[10],
            "<?php class Container { private function resolveDependencies($dependencies) {} public function build($dependencies) { $this->resolveDependencies($dependencies); } }\n",
            "resolveDependencies",
            "$this->resolveDependencies($dependencies)",
        ),
    ];
    let provider = Arc::new(provider());
    let limits = limits();

    for (case, source, terminal, complete_call) in cases {
        let fixture = Fixture::new(case, source.as_bytes());
        let analyzer = analyzer(&provider, case);
        let request = request(&fixture.snapshot, &fixture.source, case, &limits);
        let output = analyze(&analyzer, &request, &ExtensionSupport::default());
        let expected_hash = content_hash(terminal.as_bytes());
        let call = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::CallSite
                    && occurrence.syntactic_text_hash == expected_hash
            })
            .unwrap_or_else(|| panic!("{} terminal call is captured", case.name));
        assert_eq!(
            call.target,
            OccurrenceTarget::Unresolved {
                text_hash: expected_hash
            }
        );
        let span = call.source.span();
        let start = usize::try_from(span.start_byte()).expect("call start fits");
        let end = usize::try_from(span.end_byte()).expect("call end fits");
        assert_eq!(
            source.get(start..end),
            Some(complete_call),
            "{} retains receiver and argument evidence",
            case.name
        );
        assert!(output.document().relations.iter().all(|relation| {
            !matches!(
                relation.predicate,
                RelationPredicate::Calls | RelationPredicate::DispatchCandidate
            )
        }));
    }
}

#[test]
fn definition_evidence_covers_complete_go_and_rust_declarations() {
    const GO_SOURCE: &str =
        "package api\n\nfunc (handler Handler) GenerateHandler() {\n\tserve()\n}\n";
    const GO_DECLARATION: &str = "func (handler Handler) GenerateHandler() {\n\tserve()\n}";
    const RUST_SOURCE: &str =
        "pub enum DenoSubcommand {\n    Run,\n    Test {\n        watch: bool,\n    },\n}\n";
    const RUST_DECLARATION: &str =
        "pub enum DenoSubcommand {\n    Run,\n    Test {\n        watch: bool,\n    },\n}";
    for (case, source, symbol, expected) in [
        (CASES[4], GO_SOURCE, "GenerateHandler", GO_DECLARATION),
        (CASES[0], RUST_SOURCE, "DenoSubcommand", RUST_DECLARATION),
    ] {
        let fixture = Fixture::new(case, source.as_bytes());
        let provider = Arc::new(provider());
        let analyzer = analyzer(&provider, case);
        let limits = limits();
        let request = request(&fixture.snapshot, &fixture.source, case, &limits);
        let output = analyze(&analyzer, &request, &ExtensionSupport::default());
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == symbol)
            .expect("definition entity is indexed");
        let span = entity
            .evidence
            .source
            .as_ref()
            .expect("definition carries source evidence")
            .span();
        let start = usize::try_from(span.start_byte()).expect("span start fits");
        let end = usize::try_from(span.end_byte()).expect("span end fits");

        assert_eq!(
            source.get(start..end),
            Some(expected),
            "{symbol} evidence must compose with a complete definition read"
        );
    }
}

#[test]
fn real_analyzer_reports_invalid_utf8_without_source_material() {
    const SECRET: &str = "do-not-leak-this-source-material";
    let case = CASES[0];
    let mut bytes = SECRET.as_bytes().to_vec();
    bytes.push(0xff);
    let fixture = Fixture::new(case, &bytes);
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, case);
    let limits = limits();
    let request = request(&fixture.snapshot, &fixture.source, case, &limits);

    let error = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect_err("invalid UTF-8 must fail before lowering");
    assert!(matches!(
        &error,
        AdapterError::ProviderFailed { code } if code.as_str() == "invalid-utf8"
    ));
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(SECRET));
    assert!(!rendered.contains(case.path));

    let fallback = analyzer
        .analyze_unsupported_encoding(
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("repository fallback preserves bounded coverage");
    validate_ir_document(
        fallback.document(),
        limits.ir(),
        &ExtensionSupport::default(),
    )
    .expect("fallback IR validates");
    assert_eq!(fallback.document().files.len(), 1);
    assert_eq!(fallback.document().diagnostics.len(), 1);
    assert_eq!(fallback.document().diagnostics[0].code, "invalid-utf8");
    assert_eq!(fallback.document().skipped_regions.len(), 3);
    assert!(fallback.document().skipped_regions.iter().all(|region| {
        region.reason == SkippedRegionReason::UnsupportedEncoding && region.detail == "invalid-utf8"
    }));
    assert!(fallback.document().coverage_records.iter().any(|coverage| {
        coverage.domain == FactDomain::Entities
            && coverage.status == CoverageStatus::Bounded
            && coverage.skipped == 1
    }));
    let fallback_json = serde_json::to_string(fallback.document()).expect("fallback IR encodes");
    assert!(!fallback_json.contains(SECRET));
}

#[test]
fn real_analyzer_keeps_rust_methods_bound_to_stable_impl_headers() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let case = CASES[0];
    let before =
        "struct A;\nstruct B;\nimpl A { fn same(&self) {} }\nimpl B { fn same(&self) {} }\n";
    let inserted = "struct C;\nimpl C { fn other(&self) {} }\nstruct A;\nstruct B;\nimpl A { fn same(&self) {} }\nimpl B { fn same(&self) {} }\n";
    let reordered =
        "struct A;\nstruct B;\nimpl B { fn same(&self) {} }\nimpl A { fn same(&self) {} }\n";
    let commented = "struct A;\nstruct B;\nimpl /* explanatory comment */ A { fn same(&self) {} }\nimpl B { fn same(&self) {} }\n";
    let macro_item = "struct A;\nstruct B;\nimpl A {\n  generate_helpers! { unrelated_tokens }\n  fn same(&self) {}\n}\nimpl B { fn same(&self) {} }\n";
    let fixture = Fixture::new(case, before.as_bytes());
    let analyzer = analyzer(&provider, case);
    let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let initial = analyze(&analyzer, &initial_request, &extensions);
    validate_ir_document(initial.document(), limits.ir(), &extensions)
        .expect("initial Rust impl IR must validate");
    let initial_symbols = same_symbols_by_qualified_name(initial.document());
    assert_eq!(
        initial_symbols
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["A::same", "B::same"]
    );
    assert!(
        initial_symbols
            .values()
            .all(|(_, kind)| *kind == EntityKind::Method)
    );
    assert_ne!(
        initial_symbols["A::same"].0, initial_symbols["B::same"].0,
        "methods in distinct semantic impl scopes must have distinct IDs"
    );

    for (description, source) in [
        ("inserting an unrelated earlier impl", inserted),
        ("reordering sibling impls", reordered),
        ("adding non-semantic impl-header trivia", commented),
        ("adding an unsupported earlier impl item", macro_item),
    ] {
        let variant = fixture.rewrite(source.as_bytes());
        let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
        let reparsed = analyze(&analyzer, &variant_request, &extensions);
        validate_ir_document(reparsed.document(), limits.ir(), &extensions)
            .expect("reparsed Rust impl IR must validate");
        let reparsed_symbols = same_symbols_by_qualified_name(reparsed.document());
        for qualified_name in ["A::same", "B::same"] {
            assert_eq!(
                reparsed_symbols.get(qualified_name),
                initial_symbols.get(qualified_name),
                "{qualified_name} changed identity after {description}"
            );
        }
    }
}

#[test]
fn rust_signatures_keep_return_types_and_where_clauses_without_bodies() {
    let provider = Arc::new(provider());
    let limits = limits();
    let case = CASES[0];
    let analyzer = analyzer(&provider, case);
    for header in [
        "pub fn item(value: u32) -> u64",
        "pub async fn item<T>(\nvalue: T,\n) -> T where T: Copy",
        "pub fn item() -> [u8; { 1 + 2 }] /* { not a body } */",
    ] {
        let source = format!("{header}\n{{ todo!() }}\n");
        let fixture = Fixture::new(case, source.as_bytes());
        let result = analyze(
            &analyzer,
            &request(&fixture.snapshot, &fixture.source, case, &limits),
            &ExtensionSupport::default(),
        );
        let function = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function)
            .unwrap();
        let signatures = result
            .document()
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
            .filter_map(|envelope| {
                let evidence = rootlight_ir::decode_lexical_evidence_envelope(envelope).unwrap();
                (evidence.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                    && evidence.subject() == rootlight_ir::FactRef::Entity(function.id))
                .then_some((envelope, evidence))
            })
            .collect::<Vec<_>>();
        assert_eq!(signatures.len(), 1);
        let (envelope, evidence) = &signatures[0];
        assert_eq!(evidence.text(), header);
        assert!(!evidence.is_truncated());
        let reference = envelope.evidence.source.as_ref().unwrap();
        let start = usize::try_from(reference.span().start_byte()).unwrap();
        let end = usize::try_from(reference.span().end_byte()).unwrap();
        assert_eq!(source.get(start..end), Some(header));
    }
}

#[test]
fn real_analyzer_distinguishes_trait_and_inherent_impl_owners() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let case = CASES[0];
    let source = "trait Foo { fn same(&self); }\nstruct Bar;\nstruct FooforBar;\nimpl Foo for Bar { fn same(&self) {} }\nimpl FooforBar { fn same(&self) {} }\n";
    let fixture = Fixture::new(case, source.as_bytes());
    let analyzer = analyzer(&provider, case);
    let analysis_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let output = analyze(&analyzer, &analysis_request, &extensions);
    let symbols = same_symbols_by_qualified_name(output.document());

    assert_eq!(
        symbols.keys().map(String::as_str).collect::<Vec<_>>(),
        ["<Bar as Foo>::same", "FooforBar::same"]
    );
    assert_ne!(
        symbols["<Bar as Foo>::same"].0, symbols["FooforBar::same"].0,
        "trait and inherent impl owners must remain distinct"
    );
}

#[test]
fn real_analyzer_preserves_rust_type_token_boundaries() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let case = CASES[0];
    let source = "trait Foo {}\nstruct dynFoo;\nimpl dyn Foo { fn same(&self) {} }\nimpl dynFoo { fn same(&self) {} }\n";
    let fixture = Fixture::new(case, source.as_bytes());
    let analyzer = analyzer(&provider, case);
    let analysis_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let output = analyze(&analyzer, &analysis_request, &extensions);
    let symbols = same_symbols_by_qualified_name(output.document());

    assert_eq!(
        symbols.keys().map(String::as_str).collect::<Vec<_>>(),
        ["dyn Foo::same", "dynFoo::same"]
    );
    assert_ne!(
        symbols["dyn Foo::same"].0, symbols["dynFoo::same"].0,
        "distinct valid Rust token streams must not collapse after trivia normalization"
    );
}

#[test]
fn real_analyzer_ignores_comments_inside_generic_impl_targets() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let case = CASES[0];
    let before = "struct Generic<T>(T);\nimpl<T> Generic<T> { fn same(&self) {} }\n";
    let commented =
        "struct Generic<T>(T);\nimpl<T> Generic</* identity trivia */ T> { fn same(&self) {} }\n";
    let fixture = Fixture::new(case, before.as_bytes());
    let analyzer = analyzer(&provider, case);
    let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let initial = analyze(&analyzer, &initial_request, &extensions);
    let initial_symbol = same_symbols_by_qualified_name(initial.document());
    assert!(initial_symbol.contains_key("Generic<T>::same"));

    let variant = fixture.rewrite(commented.as_bytes());
    let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
    let reparsed = analyze(&analyzer, &variant_request, &extensions);
    assert_eq!(
        same_symbols_by_qualified_name(reparsed.document()),
        initial_symbol,
        "comments inside a captured impl target must not change semantic identity"
    );
}

#[test]
fn real_analyzer_keeps_unique_symbol_identity_after_anonymous_scope_insertion() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let case = CASES[2];
    let before = "function outer() {\n  { const keep = 1; }\n}\n";
    let inserted = "function outer() {\n  { const unrelated = 0; }\n  { const keep = 1; }\n}\n";
    let fixture = Fixture::new(case, before.as_bytes());
    let analyzer = analyzer(&provider, case);
    let initial_request = request(&fixture.snapshot, &fixture.source, case, &limits);
    let initial = analyze(&analyzer, &initial_request, &extensions);
    let initial_id = symbol_id_named(initial.document(), "keep");

    let variant = fixture.rewrite(inserted.as_bytes());
    let variant_request = request(&variant.snapshot, &variant.source, case, &limits);
    let reparsed = analyze(&analyzer, &variant_request, &extensions);
    assert_eq!(
        symbol_id_named(reparsed.document(), "keep"),
        initial_id,
        "an unrelated earlier anonymous block must not perturb an unchanged symbol ID"
    );
}

#[test]
fn real_analyzer_bounds_ambiguous_anonymous_scope_identity_without_source_material() {
    const SECRET: &str = "scope-secret-marker";
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();
    let cases = [
        (
            CASES[1],
            format!(
                "# {SECRET}\nif True:\n    def same():\n        return 1\nif False:\n    def same():\n        return 2\n"
            ),
        ),
        (
            CASES[2],
            format!(
                "// {SECRET}\nfunction outer() {{\n  {{ const same = 1; }}\n  {{ const same = 2; }}\n}}\n"
            ),
        ),
        (
            CASES[3],
            format!(
                "// {SECRET}\nclass Outer {{ void run() {{ {{ int same = 1; }} {{ int same = 2; }} }} }}\n"
            ),
        ),
    ];

    for (case, source) in cases {
        let fixture = Fixture::new(case, source.as_bytes());
        let analyzer = analyzer(&provider, case);
        let analysis_request = request(&fixture.snapshot, &fixture.source, case, &limits);
        let output = execute_analysis(
            &analyzer,
            &analysis_request,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("ambiguous anonymous scope identity must degrade to bounded coverage");
        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
        assert!(output.document().skipped_regions.iter().any(|region| {
            region.domain == FactDomain::Entities
                && region.detail == "stable-scope-identity-unavailable"
        }));
        assert!(
            output
                .document()
                .entities
                .iter()
                .all(|entity| entity.canonical_name != "same")
        );
    }
}

fn assert_descriptor(analyzer: &TreeSitterAnalyzer, case: LanguageCase) {
    let descriptor = analyzer.descriptor();
    assert_eq!(descriptor.identity(), &producer_identity());
    assert_eq!(descriptor.kind(), ProducerKind::Parser);
    assert_eq!(descriptor.language().as_str(), case.name);
    assert_eq!(descriptor.tier(), AnalysisTier::TierD);
    assert_eq!(
        descriptor.memory_enforcement(),
        MemoryEnforcement::Unavailable
    );
    assert!(descriptor.supports_noncritical_extensions());
}

fn assert_contract(
    output: &AnalysisOutput,
    fixture: &Fixture,
    case: LanguageCase,
    limits: &AnalysisLimits,
    extensions: &ExtensionSupport,
) {
    let document = output.document();
    validate_ir_document(document, limits.ir(), extensions)
        .unwrap_or_else(|error| panic!("{} normalized IR must validate: {error}", case.name));
    let encoded = serde_json::to_vec(document).expect("normalized IR encodes");
    let decoded =
        decode_ir_document(&encoded, limits.ir(), extensions).expect("bounded IR decode succeeds");
    assert_eq!(decoded, IrDocument::NormalizedV1_1(document.clone()));

    assert_eq!(
        output.memory_admission(),
        MemoryAdmissionStatus::UnavailableEnforcementFallback
    );
    assert_eq!(document.repository, fixture.source.repository());
    assert_eq!(document.generation, fixture.source.generation());
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.id, fixture.snapshot.file());
    assert_eq!(file.repository, fixture.source.repository());
    assert_eq!(file.generation, fixture.source.generation());
    assert_eq!(file.path, case.path);
    assert_eq!(file.content_hash, fixture.source.content_hash());
    assert_eq!(file.byte_length, fixture.source.span().end_byte());
    assert_eq!(file.language, case.name);
    assert_eq!(file.encoding, "utf-8");
    assert_eq!(file.generated, case.generated);
    assert_eq!(file.evidence, direct_evidence(&fixture.source));

    assert_eq!(document.provenance.len(), 1);
    let provenance = &document.provenance[0];
    assert_eq!(file.provenance, provenance.id);
    assert_eq!(provenance.repository, fixture.source.repository());
    assert_eq!(provenance.generation, fixture.source.generation());
    assert_eq!(provenance.producer_kind, ProducerKind::Parser);
    assert_eq!(provenance.producer, producer_identity());
    assert_eq!(provenance.binary_digest, content_hash(BINARY_SEED));
    assert_eq!(provenance.frontend_version.as_deref(), Some(case.frontend));
    assert_eq!(provenance.language, case.name);
    assert_eq!(provenance.tier, AnalysisTier::TierD);
    assert_eq!(provenance.build_context, build_context());
    assert_eq!(provenance.input_sources, vec![fixture.source.clone()]);
    assert_eq!(provenance.evidence_sources, vec![fixture.source.clone()]);
    assert!(provenance.derivation_parents.is_empty());
    assert_eq!(provenance.rule, None);

    let coverage = output.report().coverage();
    assert_eq!(coverage.tier(), AnalysisTier::TierD);
    let expected_status = if matches!(case.name, "ruby" | "bash") {
        CoverageStatus::Complete
    } else {
        CoverageStatus::Bounded
    };
    assert_eq!(coverage.status(), expected_status);
    assert_eq!(
        coverage.total_source_bytes(),
        fixture.snapshot.content().len()
    );
    assert_eq!(
        coverage.covered_source_bytes(),
        fixture.snapshot.content().len()
    );
    assert_eq!(coverage.skipped_regions(), document.skipped_regions.len());
    assert_eq!(
        coverage
            .domains()
            .iter()
            .map(rootlight_adapter_sdk::DomainCoverage::domain)
            .collect::<Vec<_>>(),
        EXPECTED_DOMAINS
    );
    let records_by_domain: BTreeMap<_, _> = document
        .coverage_records
        .iter()
        .map(|record| (record.domain, record))
        .collect();
    assert_eq!(
        records_by_domain.keys().copied().collect::<Vec<_>>(),
        EXPECTED_DOMAINS
    );
    for reported in coverage.domains() {
        let record = records_by_domain
            .get(&reported.domain())
            .expect("every reported domain has a normalized coverage record");
        let skipped_in_domain = document
            .skipped_regions
            .iter()
            .filter(|region| region.domain == reported.domain())
            .count();
        let indexed_in_domain = match reported.domain() {
            FactDomain::Files => document.files.len(),
            FactDomain::Entities => document.entities.len(),
            FactDomain::Occurrences => document.occurrences.len(),
            FactDomain::Relations => document.relations.len(),
            FactDomain::Provenance => document.provenance.len(),
            FactDomain::SourceMappings => 0,
            FactDomain::Diagnostics => document.diagnostics.len(),
            FactDomain::Extensions => document.extensions.len(),
        };
        assert_eq!(record.scope, CoverageScope::File(file.id));
        assert_eq!(record.domain, reported.domain());
        assert_eq!(record.tier, AnalysisTier::TierD);
        assert_eq!(record.status, reported.status());
        assert_eq!(reported.skipped(), skipped_in_domain);
        assert_eq!(reported.indexed(), indexed_in_domain);
        assert_eq!(
            reported.discovered(),
            indexed_in_domain
                .checked_add(skipped_in_domain)
                .expect("domain accounting fits")
        );
        if skipped_in_domain > 0 {
            assert_eq!(reported.status(), CoverageStatus::Bounded);
        }
        assert_eq!(
            (record.discovered, record.indexed, record.skipped),
            (
                u64::try_from(reported.discovered()).expect("discovered count fits"),
                u64::try_from(reported.indexed()).expect("indexed count fits"),
                u64::try_from(reported.skipped()).expect("skipped count fits"),
            )
        );
        assert_eq!(record.provenance, provenance.id);
        assert_eq!(record.evidence, direct_evidence(&fixture.source));
    }
    let unresolved_import = document.skipped_regions.iter().any(|region| {
        region.domain == FactDomain::Relations && region.detail == "unresolved-import-target"
    });
    // These languages load modules through calls rather than grammar imports.
    assert_eq!(
        unresolved_import,
        !matches!(case.name, "lua" | "ruby" | "bash")
    );
    assert!(document.entities.iter().all(|entity| {
        entity.tier == AnalysisTier::TierD
            && entity.provenance == provenance.id
            && entity.evidence.source.is_some()
    }));
    for relation in &document.relations {
        if relation.predicate == RelationPredicate::Contains {
            continue;
        }
        assert_eq!(case.name, "lua");
        assert_eq!(relation.predicate, RelationPredicate::RefersTo);
        let RelationEndpoint::Occurrence(id) = relation.subject else {
            panic!("lexical relationship retains an occurrence endpoint");
        };
        let occurrence = document
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id == id)
            .expect("lexical relationship occurrence exists");
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            panic!("lexical relationship has an exact binding target");
        };
        assert_eq!(occurrence.role, OccurrenceRole::Reference);
        assert_eq!(relation.object, RelationEndpoint::Entity(symbol));
        assert_eq!(relation.evidence.source.as_ref(), Some(&occurrence.source));
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    if matches!(case.name, "python" | "javascript" | "typescript" | "ruby") {
        let file_module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == case.path)
            .expect("implicit file module is emitted");
        assert_eq!(file_module.flags, vec![EntityFlag::Synthetic]);
        assert!(document.occurrences.iter().all(|occurrence| {
            occurrence.role != OccurrenceRole::Definition
                || !matches!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol } if symbol == file_module.id
                )
        }));
    }
}

fn same_symbols_by_qualified_name(
    document: &rootlight_ir::NormalizedIrDocument,
) -> BTreeMap<String, (SymbolId, EntityKind)> {
    document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same")
        .map(|entity| (entity.qualified_name.clone(), (entity.id, entity.kind)))
        .collect()
}

fn symbol_id_named(
    document: &rootlight_ir::NormalizedIrDocument,
    canonical_name: &str,
) -> SymbolId {
    let mut matching = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == canonical_name);
    let id = matching
        .next()
        .unwrap_or_else(|| panic!("missing entity named {canonical_name}"))
        .id;
    assert!(
        matching.next().is_none(),
        "fixture must produce one entity named {canonical_name}"
    );
    id
}

fn symbol_ids(
    document: &rootlight_ir::NormalizedIrDocument,
) -> BTreeMap<(EntityKind, String, String), SymbolId> {
    let symbols: BTreeMap<_, _> = document
        .entities
        .iter()
        .map(|entity| {
            (
                (
                    entity.kind,
                    entity.language.clone(),
                    entity.qualified_name.clone(),
                ),
                entity.id,
            )
        })
        .collect();
    assert!(
        !symbols.is_empty(),
        "fixture must produce semantic entities"
    );
    symbols
}

fn analyzer(provider: &Arc<TreeSitterProvider>, case: LanguageCase) -> TreeSitterAnalyzer {
    let parser: Arc<dyn ParseProvider> = provider.clone();
    TreeSitterAnalyzer::new(
        parser,
        producer_identity(),
        language(case),
        case.frontend,
        content_hash(BINARY_SEED),
    )
    .expect("analyzer configuration is valid")
}

fn analyze(
    analyzer: &TreeSitterAnalyzer,
    request: &AnalysisRequest<'_>,
    extensions: &ExtensionSupport,
) -> AnalysisOutput {
    execute_analysis(
        analyzer,
        request,
        extensions.clone(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("real analyzer commits canonical IR")
}

fn request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    case: LanguageCase,
    limits: &'a AnalysisLimits,
) -> AnalysisRequest<'a> {
    AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds to source"),
        language(case),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        limits,
    )
    .expect("analysis request is valid")
    .with_generated_status(case.generated)
}

fn provider() -> TreeSitterProvider {
    let settings = ParserSettings::new(4096).expect("parser settings are valid");
    let config = RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        64,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .expect("runtime configuration is valid");
    TreeSitterProvider::new(config).expect("audited provider initializes")
}

fn limits() -> AnalysisLimits {
    limits_with_syntax_records(16_384)
}

fn limits_with_syntax_records(max_records: usize) -> AnalysisLimits {
    let batch = BatchThresholds::new(128.min(max_records), 1024 * 1024, 32, 128 * 1024)
        .expect("batch limits are valid");
    let syntax_stream = StreamLimits::new(
        128,
        max_records,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    let ir_stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).expect("IR batch limits are valid"),
    )
    .expect("IR stream limits are valid");
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        16 * 1024 * 1024,
        syntax_stream,
        ir_stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
}

fn producer_identity() -> ProducerIdentity {
    ProducerIdentity::new(
        "rootlight-treesitter-e2e",
        "1.0",
        content_hash(CONFIGURATION_SEED),
    )
    .expect("producer identity is valid")
}

fn build_context() -> BuildContextIdentity {
    BuildContextIdentity::new(content_hash(BUILD_CONTEXT_SEED))
}

fn language(case: LanguageCase) -> LanguageId {
    LanguageId::new(case.name).expect("language identity is valid")
}

fn direct_evidence(source: &SourceRef) -> FactEvidence {
    FactEvidence {
        source: Some(source.clone()),
        derivation: Vec::new(),
    }
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline is representable"),
    )
}

struct Fixture {
    temporary: Arc<TempDir>,
    relative: RelativePath,
    repository: RepositoryId,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(case: LanguageCase, bytes: &[u8]) -> Self {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary =
            Arc::new(tempdir_in(current).expect("local temporary directory is available"));
        let relative = RelativePath::parse(Path::new(case.path)).expect("fixture path is valid");
        if let Some(parent) = Path::new(case.path).parent() {
            fs::create_dir_all(temporary.path().join(parent))
                .expect("fixture source directory is created");
        }
        fs::write(temporary.path().join(case.path), bytes).expect("fixture source is written");
        let repository = derive_repository(case.name.as_bytes()).id();
        let (snapshot, source) = capture(
            &temporary,
            repository,
            GenerationId::from_bytes([17; 20]),
            &relative,
        );
        Self {
            temporary,
            relative,
            repository,
            snapshot,
            source,
        }
    }

    fn rewrite(&self, bytes: &[u8]) -> Self {
        fs::write(self.temporary.path().join(self.relative.as_str()), bytes)
            .expect("updated fixture source is written");
        let (snapshot, source) = capture(
            &self.temporary,
            self.repository,
            GenerationId::from_bytes([18; 20]),
            &self.relative,
        );
        Self {
            temporary: Arc::clone(&self.temporary),
            relative: self.relative.clone(),
            repository: self.repository,
            snapshot,
            source,
        }
    }

    fn next_generation(&self) -> Self {
        let (snapshot, source) = capture(
            &self.temporary,
            self.repository,
            GenerationId::from_bytes([19; 20]),
            &self.relative,
        );
        Self {
            temporary: Arc::clone(&self.temporary),
            relative: self.relative.clone(),
            repository: self.repository,
            snapshot,
            source,
        }
    }
}

fn capture(
    temporary: &TempDir,
    repository_id: RepositoryId,
    generation: GenerationId,
    relative: &RelativePath,
) -> (SourceSnapshot, SourceRef) {
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let snapshot = repository
        .snapshot(
            relative,
            u64::try_from(MAX_SOURCE_BYTES).expect("snapshot limit fits"),
        )
        .expect("fixture snapshot is stable");
    let byte_length = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    let source = SourceRef::new(
        repository_id,
        generation,
        SourceSpan::new(snapshot.file(), 0, byte_length).expect("full span is ordered"),
        snapshot.content_hash(),
        None,
    );
    (snapshot, source)
}
