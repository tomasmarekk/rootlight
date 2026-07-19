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
    OccurrenceTarget, ProducerIdentity, ProducerKind, RelationPredicate, SourceRef, SourceSpan,
    decode_ir_document, validate_ir_document,
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

const CASES: [LanguageCase; 6] = [
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
        frontend: "tree-sitter-javascript-0.25.0",
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
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(),
        )
        .expect("initial structural artifact is captured");
    let successor = fixture.next_generation();
    let successor_request = request(&successor.snapshot, &successor.source, case, &limits);

    let reused = primary_analyzer
        .analyze_from_artifact(
            &successor_request,
            &artifact,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
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
                MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
                &deadline(),
            )
            .is_err(),
        "identical public metadata must not authorize cross-provider reuse"
    );
}

#[test]
fn reviewed_queries_preserve_explicit_call_sites() {
    let provider = Arc::new(provider());
    let limits = limits();
    let extensions = ExtensionSupport::default();

    for case in CASES.into_iter().filter(|case| {
        matches!(
            case.name,
            "rust" | "python" | "javascript" | "go" | "typescript"
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
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
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
fn real_analyzer_rejects_ambiguous_anonymous_scope_identity_without_source_material() {
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
        let error = execute_analysis(
            &analyzer,
            &analysis_request,
            extensions.clone(),
            MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
            &deadline(),
        )
        .expect_err("ambiguous anonymous scope identity must fail conservatively");
        assert!(matches!(
            &error,
            AdapterError::ProviderFailed { code }
                if code.as_str() == "treesitter-lowering-symbol-collision"
        ));
        let rendered = format!("{error:?}\n{error}");
        assert!(!rendered.contains(SECRET));
        assert!(!rendered.contains(case.path));
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
        MemoryAdmissionStatus::UnavailableM05Fallback
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
    assert_eq!(coverage.status(), CoverageStatus::Bounded);
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
    assert!(document.skipped_regions.iter().any(|region| {
        region.domain == FactDomain::Relations && region.detail == "unresolved-import-target"
    }));
    assert!(document.entities.iter().all(|entity| {
        entity.tier == AnalysisTier::TierD
            && entity.provenance == provenance.id
            && entity.evidence.source.is_some()
    }));
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    if matches!(case.name, "python" | "javascript" | "typescript") {
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
        MemoryAdmissionPolicy::AllowUnavailableM05Fallback,
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
    let batch =
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).expect("batch limits are valid");
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_SYNTAX_NODES,
        MAX_SYNTAX_DEPTH,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
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
