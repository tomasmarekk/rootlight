//! Public-boundary integration contracts for the real Tree-sitter analyzer.
//!
//! The audited grammars flow from VFS snapshots through parsing, lowering,
//! canonical normalized IR, and explicit validation without native parser types.

use std::{
    collections::BTreeMap,
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

const CASES: [LanguageCase; 13] = [
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
fn complete_structural_artifact_replays_under_a_smaller_fact_partition() {
    let case = CASES[0];
    let primary_provider = Arc::new(provider());
    let primary_analyzer = analyzer(&primary_provider, case);
    let initial_limits = limits();
    let extensions = ExtensionSupport::default();
    let fixture = Fixture::new(case, b"fn stable() {}\n");
    let initial_request = request(&fixture.snapshot, &fixture.source, case, &initial_limits);
    let (_, artifact) = primary_analyzer
        .analyze_and_capture(
            &initial_request,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("complete structural artifact is captured");
    let bounded_limits = limits_with_syntax_records(artifact.syntax_fact_count().max(1));
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
    let case = LUA_CASE;
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
    let expected_status = if case.name == "ruby" {
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
    // Lua and Ruby module loading use ordinary calls, not grammar import statements.
    assert_eq!(unresolved_import, !matches!(case.name, "lua" | "ruby"));
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
