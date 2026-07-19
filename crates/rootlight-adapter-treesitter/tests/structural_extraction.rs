//! Golden contracts for generic, bounded structural fact extraction.
//!
//! The fixtures exercise reviewed grammar packs through the public SDK boundary,
//! including CRLF, Unicode, recovery, included ranges, and incremental reuse.

use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, IncludedRange,
    LanguageId, MemoryAdmissionPolicy, ParseOutput, ParseRequest, StreamLimits, SyntaxFact,
    SyntaxFactKind, execute_parse,
};
use rootlight_adapter_treesitter::{
    ParserSettings, ReuseStatus, RuntimeConfig, SourceEdit, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ir::{CoverageStatus, IrLimits, SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;

struct LanguageCase {
    name: &'static str,
    language: &'static str,
    source: &'static str,
}

const CASES: [LanguageCase; 6] = [
    LanguageCase {
        name: "structural.rs",
        language: "rust",
        source: include_str!("fixtures/structural/rust.rs"),
    },
    LanguageCase {
        name: "structural.py",
        language: "python",
        source: include_str!("fixtures/structural/python.py"),
    },
    LanguageCase {
        name: "structural.js",
        language: "javascript",
        source: include_str!("fixtures/structural/javascript.js"),
    },
    LanguageCase {
        name: "Structural.java",
        language: "java",
        source: include_str!("fixtures/structural/java.java"),
    },
    LanguageCase {
        name: "structural.go",
        language: "go",
        source: include_str!("fixtures/structural/go.go"),
    },
    LanguageCase {
        name: "structural.ts",
        language: "typescript",
        source: include_str!("fixtures/structural/typescript.ts"),
    },
];

#[test]
fn six_language_crlf_unicode_fixtures_match_structural_goldens() {
    let provider = provider();
    let limits = limits(4096, 128);

    for case in CASES {
        let bytes = crlf_bytes(case.source);
        let fixture = Fixture::new(case.name, &bytes);
        let request = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            case.language,
            Vec::new(),
        );
        let output = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("reviewed structural fixture parses");

        assert_eq!(
            output.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(output.diagnostics().is_empty());
        assert_eq!(label_counts(&output), golden_label_counts(case.language));
        assert_required_roles(&output);
        assert_parent_contract(output.facts());
        assert_role_precedence(output.facts());
        assert!(bytes.windows(2).any(|window| window == b"\r\n"));
        assert!(bytes.iter().any(|byte| !byte.is_ascii()));

        if case.language == "python" {
            assert_python_non_doc_string(&output, &bytes);
        }
        if case.language == "java" {
            assert_java_annotation_element(&output, &bytes);
        }
    }
}

#[test]
fn rust_impl_scopes_parent_same_named_methods() {
    const SOURCE: &[u8] =
        b"struct A;\nstruct B;\nimpl A { fn same(&self) {} }\nimpl B { fn same(&self) {} }\n";
    let fixture = Fixture::new("impl-scopes.rs", SOURCE);
    let limits = limits(4096, 128);
    let provider = provider();
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("impl-scope fixture parses");
    let facts = output.facts();
    assert_eq!(
        facts
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "rust.impl.scope")
            .count(),
        2
    );
    let methods = facts
        .iter()
        .filter(|fact| {
            if fact.syntax_kind().as_str() != "rust.function.declaration" {
                return false;
            }
            let start = usize::try_from(fact.span().start_byte()).expect("span start fits");
            let end = usize::try_from(fact.span().end_byte()).expect("span end fits");
            SOURCE
                .get(start..end)
                .is_some_and(|text| text.starts_with(b"fn same"))
        })
        .collect::<Vec<_>>();
    assert_eq!(methods.len(), 2);
    for method in methods {
        let parent = method
            .parent()
            .and_then(|parent| facts.iter().find(|fact| fact.local_id() == parent))
            .expect("same-named method has a captured parent");
        assert_eq!(parent.syntax_kind().as_str(), "rust.impl.scope");
    }
}

#[test]
fn clean_and_incremental_extraction_are_logically_identical_for_every_family() {
    let limits = limits(4096, 128);
    let settings = ParserSettings::new(256).expect("test parser settings are valid");

    for case in CASES {
        let incremental_provider = provider();
        let initial_bytes = crlf_bytes(case.source);
        let fixture = Fixture::new(case.name, &initial_bytes);
        let initial_request = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            case.language,
            Vec::new(),
        );
        let initial = incremental_provider
            .execute_with_previous(
                &initial_request,
                None,
                &[],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .expect("initial parse succeeds");
        let previous = initial
            .previous()
            .expect("fixture tree fits the cache")
            .clone();

        let mut updated_bytes = initial_bytes.clone();
        updated_bytes.extend_from_slice(b"\r\n");
        let updated = fixture.rewrite(&updated_bytes);
        let updated_request = request(
            &updated.snapshot,
            &updated.source,
            &limits,
            case.language,
            Vec::new(),
        );
        let edit = SourceEdit::new(initial_bytes.len(), initial_bytes.len(), "\r\n")
            .expect("append edit is valid");
        let incremental = incremental_provider
            .execute_with_previous(
                &updated_request,
                Some(&previous),
                &[edit],
                settings,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .expect("incremental extraction succeeds");
        let clean_provider = provider();
        let clean = execute_parse(
            &clean_provider,
            &updated_request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("clean extraction succeeds");

        assert!(matches!(
            incremental.reuse_status(),
            ReuseStatus::Reused { .. }
        ));
        assert_eq!(incremental.output().facts(), clean.facts());
        assert_eq!(incremental.output().diagnostics(), clean.diagnostics());
    }
}

#[test]
fn malformed_input_preserves_facts_outside_recovery() {
    let source = b"fn good() {}\r\nfn broken( {\r\nfn later() {}\r\n";
    let fixture = Fixture::new("recovery.rs", source);
    let limits = limits(4096, 128);
    let provider = provider();
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("recovery output commits");

    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert!(
        output
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
    );
    assert!(fact_texts(&output, source, "rust.identifier.definition").contains(&"good"));
}

#[test]
fn disjoint_included_ranges_never_emit_gap_facts() {
    let source = b"fn first() {}\r\nnot rust at all\r\nfn second() {}\r\n";
    let fixture = Fixture::new("ranges.rs", source);
    let first_end = b"fn first() {}\r\n".len();
    let second_start = source
        .windows(b"fn second".len())
        .position(|window| window == b"fn second")
        .expect("second function exists");
    let language = LanguageId::new("rust").expect("language is valid");
    let included = vec![
        IncludedRange::new(byte_span(&fixture, 0, first_end), language.clone()),
        IncludedRange::new(byte_span(&fixture, second_start, source.len()), language),
    ];
    let limits = limits(4096, 128);
    let provider = provider();
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        included.clone(),
    );
    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("multi-range extraction succeeds");

    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert_eq!(
        output
            .facts()
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Root)
            .count(),
        2
    );
    assert!(output.facts().iter().all(|fact| {
        included.iter().any(|range| {
            fact.span().start_byte() >= range.span().start_byte()
                && fact.span().end_byte() <= range.span().end_byte()
        })
    }));
    assert_eq!(
        fact_texts(&output, source, "rust.identifier.definition"),
        vec!["first", "second"]
    );
}

#[test]
fn sink_fact_pressure_is_explicit_and_bounded() {
    let bytes = crlf_bytes(CASES[0].source);
    let fixture = Fixture::new("limited.rs", &bytes);
    let limits = fact_limited_limits();
    let provider = provider();
    let request = request(
        &fixture.snapshot,
        &fixture.source,
        &limits,
        "rust",
        Vec::new(),
    );
    let output = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("fact-limited extraction commits a diagnostic");

    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
    assert!(
        output
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == "syntax-extraction-limit")
    );
    assert!(output.facts().len() <= 4);
}

#[test]
fn java_package_and_named_module_have_bounded_name_evidence() {
    let limits = limits(4096, 128);
    let cases: [(&str, &[u8], &str, &str); 2] = [
        (
            "Package.java",
            b"package com.example.rootlight;\r\nclass Package {}\r\n",
            "java.package.module",
            "com.example.rootlight",
        ),
        (
            "module-info.java",
            b"module com.example.rootlight {}\r\n",
            "java.module.module",
            "com.example.rootlight",
        ),
    ];

    for (name, source, module_label, expected_name) in cases {
        let fixture = Fixture::new(name, source);
        let provider = provider();
        let request = request(
            &fixture.snapshot,
            &fixture.source,
            &limits,
            "java",
            Vec::new(),
        );
        let output = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .expect("named Java module syntax parses");
        let module = output
            .facts()
            .iter()
            .find(|fact| fact.syntax_kind().as_str() == module_label)
            .expect("module container is extracted");
        let name_fact = output
            .facts()
            .iter()
            .find(|fact| {
                fact.parent() == Some(module.local_id())
                    && fact.syntax_kind().as_str() == "java.qualified_identifier.definition"
            })
            .expect("module name evidence is extracted");
        assert_eq!(source_text(source, name_fact), expected_name);
    }
}

#[test]
fn deep_and_wide_inputs_stop_at_bounds_and_leave_the_provider_reusable() {
    let provider = provider();
    let mut wide = String::new();
    for index in 0..2000 {
        wide.push_str(&format!("fn item_{index}() {{ item_{index}(); }}\n"));
    }
    let wide_fixture = Fixture::new("wide.rs", wide.as_bytes());
    let wide_limits = limits(256, 128);
    let wide_request = request(
        &wide_fixture.snapshot,
        &wide_fixture.source,
        &wide_limits,
        "rust",
        Vec::new(),
    );
    let wide_output = execute_parse(
        &provider,
        &wide_request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("wide node-limited input commits");
    assert_eq!(
        wide_output.report().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(wide_output.facts().is_empty());
    assert!(
        wide_output
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == "syntax-node-limit")
    );

    let mut deep = "fn deep() {".to_owned();
    deep.push_str(&"(".repeat(2000));
    deep.push('1');
    deep.push_str(&")".repeat(2000));
    deep.push_str("}\n");
    let deep_fixture = Fixture::new("deep.rs", deep.as_bytes());
    let deep_limits = limits(16_384, 16);
    let deep_request = request(
        &deep_fixture.snapshot,
        &deep_fixture.source,
        &deep_limits,
        "rust",
        Vec::new(),
    );
    let deep_output = execute_parse(
        &provider,
        &deep_request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("deep depth-limited input commits");
    assert_eq!(
        deep_output.report().coverage().status(),
        CoverageStatus::Bounded
    );
    assert!(deep_output.facts().is_empty());
    assert!(
        deep_output
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == "syntax-depth-limit")
    );

    let cleanup = Fixture::new("cleanup.rs", b"fn cleanup() {}\n");
    let cleanup_limits = limits(4096, 128);
    let cleanup_request = request(
        &cleanup.snapshot,
        &cleanup.source,
        &cleanup_limits,
        "rust",
        Vec::new(),
    );
    execute_parse(
        &provider,
        &cleanup_request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("provider remains reusable after bounded stress");
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

fn golden_label_counts(language: &str) -> BTreeMap<String, usize> {
    let entries: &[(&str, usize)] = match language {
        "rust" => &[
            ("rust.block.scope", 1),
            ("rust.call.call", 1),
            ("rust.file.root", 1),
            ("rust.function.declaration", 1),
            ("rust.identifier.definition", 2),
            ("rust.identifier.reference", 8),
            ("rust.line_comment.documentation", 2),
            ("rust.module.module", 1),
            ("rust.parameters.signature", 1),
            ("rust.string.string", 1),
            ("rust.use.import", 1),
        ],
        "python" => &[
            ("python.block.scope", 2),
            ("python.call.call", 1),
            ("python.class.declaration", 1),
            ("python.file.module", 1),
            ("python.function.declaration", 1),
            ("python.identifier.definition", 2),
            ("python.identifier.reference", 7),
            ("python.import.import", 1),
            ("python.module.root", 1),
            ("python.parameters.signature", 1),
            ("python.string.documentation", 3),
            ("python.string.string", 5),
        ],
        "javascript" => &[
            ("javascript.block.scope", 1),
            ("javascript.call.call", 1),
            ("javascript.class.declaration", 1),
            ("javascript.comment.documentation", 2),
            ("javascript.file.module", 1),
            ("javascript.identifier.definition", 2),
            ("javascript.identifier.reference", 5),
            ("javascript.import.import", 1),
            ("javascript.method.declaration", 1),
            ("javascript.parameters.signature", 1),
            ("javascript.program.root", 1),
            ("javascript.property_identifier.definition", 1),
            ("javascript.property_identifier.reference", 1),
            ("javascript.string.string", 2),
            ("javascript.variable.declaration", 1),
        ],
        "java" => &[
            ("java.annotation.declaration", 1),
            ("java.annotation_element.declaration", 1),
            ("java.block.scope", 1),
            ("java.block_comment.documentation", 2),
            ("java.class.declaration", 1),
            ("java.identifier.definition", 6),
            ("java.identifier.reference", 6),
            ("java.import.import", 1),
            ("java.local_variable.declaration", 1),
            ("java.method.declaration", 1),
            ("java.package.module", 1),
            ("java.parameters.signature", 2),
            ("java.program.root", 1),
            ("java.string.string", 1),
        ],
        "go" => &[
            ("go.block.scope", 1),
            ("go.call.call", 1),
            ("go.comment.comment", 1),
            ("go.comment.documentation", 2),
            ("go.constant.declaration", 1),
            ("go.field_identifier.definition", 1),
            ("go.field_identifier.reference", 3),
            ("go.file.root", 1),
            ("go.identifier.definition", 2),
            ("go.identifier.reference", 7),
            ("go.import.import", 1),
            ("go.method.declaration", 1),
            ("go.package.module", 1),
            ("go.package_identifier.definition", 1),
            ("go.parameters.signature", 1),
            ("go.raw_string.string", 1),
            ("go.string.string", 2),
            ("go.type.declaration", 1),
            ("go.type_identifier.definition", 1),
            ("go.type_identifier.reference", 4),
            ("go.variable.declaration", 1),
        ],
        "typescript" => &[
            ("typescript.block.scope", 1),
            ("typescript.call.call", 1),
            ("typescript.class.declaration", 1),
            ("typescript.comment.documentation", 1),
            ("typescript.file.module", 1),
            ("typescript.identifier.definition", 1),
            ("typescript.identifier.reference", 7),
            ("typescript.import.import", 1),
            ("typescript.interface.declaration", 1),
            ("typescript.method.declaration", 1),
            ("typescript.method_signature.declaration", 1),
            ("typescript.parameters.signature", 2),
            ("typescript.program.root", 1),
            ("typescript.property_identifier.definition", 2),
            ("typescript.property_identifier.reference", 1),
            ("typescript.string.string", 2),
            ("typescript.template.string", 1),
            ("typescript.type_alias.declaration", 1),
            ("typescript.type_identifier.definition", 3),
            ("typescript.type_identifier.reference", 2),
            ("typescript.variable.declaration", 1),
        ],
        _ => panic!("unexpected fixture language"),
    };
    entries
        .iter()
        .map(|(label, count)| ((*label).to_owned(), *count))
        .collect()
}

fn label_counts(output: &ParseOutput) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for fact in output.facts() {
        *counts
            .entry(fact.syntax_kind().as_str().to_owned())
            .or_insert(0) += 1;
    }
    counts
}

fn assert_required_roles(output: &ParseOutput) {
    for kind in [
        SyntaxFactKind::Root,
        SyntaxFactKind::Module,
        SyntaxFactKind::Declaration,
        SyntaxFactKind::Signature,
        SyntaxFactKind::Import,
        SyntaxFactKind::Scope,
        SyntaxFactKind::Occurrence,
        SyntaxFactKind::Comment,
        SyntaxFactKind::StringLiteral,
    ] {
        assert!(
            output.facts().iter().any(|fact| fact.kind() == kind),
            "fixture omitted required {kind:?} evidence"
        );
    }
}

fn assert_parent_contract(facts: &[SyntaxFact]) {
    let by_id = facts
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    for fact in facts {
        if let Some(parent_id) = fact.parent() {
            let parent = by_id.get(&parent_id).expect("parent identity resolves");
            assert!(
                matches!(
                    parent.kind(),
                    SyntaxFactKind::Root
                        | SyntaxFactKind::Module
                        | SyntaxFactKind::Declaration
                        | SyntaxFactKind::Scope
                ),
                "parent must be a selected container"
            );
            assert!(parent.span().start_byte() <= fact.span().start_byte());
            assert!(parent.span().end_byte() >= fact.span().end_byte());
            assert_eq!(fact.depth(), parent.depth() + 1);
        } else {
            assert_eq!(fact.kind(), SyntaxFactKind::Root);
            assert_eq!(fact.depth(), 0);
        }
    }
    for fact in facts.iter().filter(|fact| {
        fact.kind() == SyntaxFactKind::Signature
            || fact.syntax_kind().as_str().ends_with(".definition")
    }) {
        let parent = by_id
            .get(&fact.parent().expect("name/signature has a parent"))
            .expect("name/signature parent resolves");
        if fact.kind() == SyntaxFactKind::Signature {
            assert_eq!(parent.kind(), SyntaxFactKind::Declaration);
        } else {
            assert!(matches!(
                parent.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Module
            ));
        }
    }
}

fn assert_role_precedence(facts: &[SyntaxFact]) {
    for definition in facts
        .iter()
        .filter(|fact| fact.syntax_kind().as_str().ends_with(".definition"))
    {
        assert!(!facts.iter().any(|candidate| {
            candidate.span() == definition.span()
                && candidate.syntax_kind().as_str().ends_with(".reference")
        }));
    }
    for documentation in facts
        .iter()
        .filter(|fact| fact.syntax_kind().as_str().ends_with(".documentation"))
    {
        assert!(!facts.iter().any(|candidate| {
            candidate.span() == documentation.span()
                && candidate.syntax_kind().as_str().ends_with(".comment")
        }));
    }
}

fn assert_python_non_doc_string(output: &ParseOutput, source: &[u8]) {
    let target = "\"a standalone string is not documentation\"";
    let fact = output
        .facts()
        .iter()
        .find(|fact| source_text(source, fact) == target)
        .expect("standalone string is extracted");
    assert_eq!(fact.kind(), SyntaxFactKind::StringLiteral);
    assert!(!fact.syntax_kind().as_str().ends_with(".documentation"));
}

fn assert_java_annotation_element(output: &ParseOutput, source: &[u8]) {
    let element = output
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "java.annotation_element.declaration")
        .expect("annotation element declaration is extracted");
    let by_id = output
        .facts()
        .iter()
        .map(|fact| (fact.local_id(), fact))
        .collect::<BTreeMap<_, _>>();
    let children = output
        .facts()
        .iter()
        .filter(|fact| fact.parent() == Some(element.local_id()))
        .collect::<Vec<_>>();
    assert!(children.iter().any(|fact| {
        fact.syntax_kind().as_str() == "java.identifier.definition"
            && source_text(source, fact) == "value"
    }));
    assert!(children.iter().any(|fact| {
        fact.syntax_kind().as_str() == "java.parameters.signature"
            && source_text(source, fact) == "("
    }));
    assert!(by_id.contains_key(&element.local_id()));
}

fn fact_texts<'a>(output: &ParseOutput, source: &'a [u8], label: &str) -> Vec<&'a str> {
    output
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == label)
        .map(|fact| source_text(source, fact))
        .collect()
}

fn source_text<'a>(source: &'a [u8], fact: &SyntaxFact) -> &'a str {
    let start = usize::try_from(fact.span().start_byte()).expect("fixture offset fits");
    let end = usize::try_from(fact.span().end_byte()).expect("fixture offset fits");
    std::str::from_utf8(&source[start..end]).expect("fixture span is UTF-8")
}

fn crlf_bytes(source: &str) -> Vec<u8> {
    source.replace('\n', "\r\n").into_bytes()
}

fn provider() -> TreeSitterProvider {
    let settings = ParserSettings::new(256).expect("settings are bounded");
    let config = RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        16_384,
        128,
        32,
        64,
        1,
        16 * 1024 * 1024,
        settings,
    )
    .expect("runtime config is valid");
    TreeSitterProvider::new(config).expect("audited provider initializes")
}

fn limits(max_nodes: usize, max_depth: usize) -> AnalysisLimits {
    let batch = BatchThresholds::new(64, 64 * 1024, 8, 4096).expect("batch limits are valid");
    let stream = StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch)
        .expect("stream limits are valid");
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        max_nodes,
        max_depth,
        32,
        8 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
}

fn fact_limited_limits() -> AnalysisLimits {
    let syntax_batch =
        BatchThresholds::new(2, 4096, 2, 1024).expect("syntax batch limits are valid");
    let syntax = StreamLimits::new(4, 4, 16 * 1024, 4, 4096, 4096, syntax_batch)
        .expect("syntax stream limits are valid");
    let ir_batch = BatchThresholds::new(8, 4096, 2, 1024).expect("IR batch limits are valid");
    let ir = StreamLimits::new(8, 32, 32 * 1024, 8, 8192, 8192, ir_batch)
        .expect("IR stream limits are valid");
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        4096,
        128,
        32,
        8 * 1024 * 1024,
        syntax,
        ir,
        IrLimits::default(),
    )
    .expect("analysis limits are valid")
}

fn request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
    language: &str,
    included_ranges: Vec<IncludedRange>,
) -> ParseRequest<'a> {
    ParseRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
        LanguageId::new(language).expect("language is valid"),
        EncodingId::new("utf-8").expect("encoding is valid"),
        included_ranges,
        limits,
    )
    .expect("parse request is valid")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("deadline is representable"),
    )
}

fn byte_span(fixture: &Fixture, start: usize, end: usize) -> SourceSpan {
    SourceSpan::new(
        fixture.snapshot.file(),
        u64::try_from(start).expect("fixture offset fits"),
        u64::try_from(end).expect("fixture offset fits"),
    )
    .expect("fixture span is ordered")
}

struct Fixture {
    temporary: Arc<TempDir>,
    relative: RelativePath,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let current = std::env::current_dir().expect("current directory exists");
        let temporary =
            Arc::new(tempdir_in(current).expect("local temporary directory is available"));
        fs::write(temporary.path().join(name), bytes).expect("fixture source is written");
        let relative = RelativePath::parse(Path::new(name)).expect("fixture path is valid");
        let (snapshot, source) = capture(&temporary, &relative);
        Self {
            temporary,
            relative,
            snapshot,
            source,
        }
    }

    fn rewrite(&self, bytes: &[u8]) -> Self {
        fs::write(self.temporary.path().join(self.relative.as_str()), bytes)
            .expect("updated fixture source is written");
        let (snapshot, source) = capture(&self.temporary, &self.relative);
        Self {
            temporary: Arc::clone(&self.temporary),
            relative: self.relative.clone(),
            snapshot,
            source,
        }
    }
}

fn capture(temporary: &TempDir, relative: &RelativePath) -> (SourceSnapshot, SourceRef) {
    let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        .parse()
        .expect("repository identity parses");
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let snapshot = repository
        .snapshot(relative, MAX_SOURCE_BYTES as u64)
        .expect("fixture snapshot is stable");
    let end = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    let source = SourceRef::new(
        repository_id,
        "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
            .parse()
            .expect("generation identity parses"),
        SourceSpan::new(snapshot.file(), 0, end).expect("full span is ordered"),
        snapshot.content_hash(),
        None,
    );
    (snapshot, source)
}
