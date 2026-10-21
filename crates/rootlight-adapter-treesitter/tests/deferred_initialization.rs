//! Public parsing and normalized-IR contracts for deferred built-in queries.
//! These compare observable behavior with the complete eager audit; fresh-registry
//! unit tests separately prove initialization counts and shared-query ownership.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, ParseOutput, ParseProvider,
    ParseRequest, RequestError, StreamLimits, execute_analysis, execute_parse,
};
use rootlight_adapter_treesitter::{
    GrammarRegistry, ParserSettings, ReuseInvalidation, ReuseStatus, RuntimeConfig,
    TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::content_hash;
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, ExtensionSupport, IrLimits,
    ProducerIdentity, SourceRef, SourceSpan,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MAX_NODES: usize = 16_384;
const MAX_DEPTH: usize = 128;
const MEMORY_POLICY: MemoryAdmissionPolicy =
    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;

const CASES: [(&str, &str, &[u8]); 100] = [
    ("written.idr", "idris", b"global : Type\nglobal = Type\nreader : Type\nreader = global\n"),
    ("written.sed", "sed", b":entry\r\nb entry\r\n"),
    ("written.wl", "wolfram", b"owned[value_String] := Module[{}, external[value]];\n"),
    ("written.pug", "pug", include_bytes!("fixtures/pug/hash.pug")),
    ("written.lean", "lean", b"def owned (value : Nat) := value\n"),
    ("written.coffee", "coffeescript", b"class Written\n  measure: ->\n    value = 1\n    value\n"),
    ("Dockerfile", "dockerfile", b"FROM scratch AS written\n"),
    ("meson.build", "meson", b"items = [1]\nforeach item : items\nendforeach\n"),
    ("written.gd", "gdscript", b"class_name Written\nfunc measure():\n    return 1\n"),
    ("written.chpl", "chapel", include_bytes!("fixtures/chapel/genhashkey32.chpl")),
    ("written.qs","qsharp",include_bytes!("fixtures/qsharp/Shorthand.qs")),
    ("written.sml", "standard-ml", include_bytes!("fixtures/standard_ml/deephandle.sml")),
    ("written.wat", "webassembly", include_bytes!("fixtures/webassembly/sieve.wat")),
    ("written.mojo", "mojo", include_bytes!("fixtures/mojo/llama2.mojo")),
    ("written.ring", "ring", include_bytes!("fixtures/ring/htmlfunctions.ring")),
    ("written.ha", "hare", include_bytes!("fixtures/hare/original-part1.ha")),
    ("written.hx", "haxe", b"class Written { static function measure() { var value = 1; consume(value); } }"),
    (
        "SettingsUiState.qml",
        "qml",
        include_bytes!("fixtures/qml/SettingsUiState.qml"),
    ),
    (
        "measurement.mo",
        "modelica",
        b"within Neutral;\nfunction measure\n input Real value;\n output Real result;\nalgorithm\n result := value;\nend measure;\n",
    ),
    (
        "measurement.schema.json",
        "json-schema",
        br##"{"$schema":"https://json-schema.org/draft/2020-12/schema","$defs":{"Measurement":{"type":"object","properties":{"value":{"type":"number"}}}},"properties":{"result":{"$ref":"#/$defs/Measurement"}}}"##,
    ),
    ("sample.adb", "ada", b"procedure Sample is begin null; end Sample;\n"),
    ("written.tf", "hcl", b"written = { nested = 1 }\n"),
    ("written.d", "d", b"module sample; void measure(int value) {}\n"),
    ("written.zig", "zig", b"pub fn written() void { var value: u8 = 1; _ = &value; }\n"),
    ("written.cr", "crystal", b"class Written\n  def value\n    1\n  end\nend\n"),
    ("written.cue", "cue", b"package sample\nparent: {child: string}\n"),
    ("written.pony", "pony", b"class Written\n  fun ref value(): U32 => 1\n"),
    ("written.fish", "fish", b"function measure; echo value; end\n"),
    ("written.nim", "nim", b"proc measure(value: int): int = value\n"),
    ("written.clj", "clojure", b"(ns written) (defn measure [value] value)\n"),
    ("written.rkt", "racket", b"#lang racket/base\n(define (measure value) value)\n"),
    ("written.vhdl", "vhdl", b"entity Written is port (Clock : in bit); end Written;\n"),
        ("schema.graphql", "graphql", b"type Query { value: String }\n"),
    ("sample.ex", "elixir", b"defmodule Sample do\n def value, do: 1\nend\n"),
    ("schema.proto", "protobuf", b"syntax = \"proto3\"; message Record { string value = 1; }\n"),
    ("schema.thrift", "thrift", b"struct Record { 1: string value; }\n"),
    ("schema.capnp", "capnp", b"struct Item { value @0 :Text; }\n"),
    ("written.v", "coq", b"(* marker *)\r\nDefinition written_value : nat := 1.\r\nLemma statement : True. Proof. exact I. Qed.\r\n"),
    ("source.v", "v", b"module written\nstruct Box { value int }\nfn measure(value int) int { return value }\n"),
    ("written.v", "verilog", b"module Written(input wire clk); wire value; endmodule\n"),
    ("written.sv", "systemverilog", b"interface Written(input logic clk); logic value; endinterface\n"),
    ("written.e", "eiffel", b"class BOX\nfeature\n value: INTEGER\nend\n"),
    ("written.cmake", "cmake", b"function(Written value)\nset(local ${value})\nendfunction()\n"),
    ("written.jl", "julia", b"module Written\nmeasure(value::Int) = value + 1\nend\n"),
    ("written.ml", "ocaml", b"let increment x = x + 1\n"),
    ("written.mli", "ocaml-interface", b"val increment : int -> int\n"),
    ("written.scm", "scheme", b"(define (measure value) (+ value 1))\n"),
    ("written.xml", "xml", b"<root><item value=\"plain\"/><item value='other'>text</item></root>"),
    ("written.dtd", "dtd", b"<!ELEMENT root EMPTY>"),
    ("written.wgsl", "wgsl", b"fn Written(Value: f32) -> f32 { return Value; }\n"),
    ("written.hlsl", "hlsl", b"float Written(float Value) { return Value; }\n"),
    ("written.glsl", "glsl", b"float Written(float Value) { return Value; }\n"),
    ("written.asm", "assembly", b"Written PROC\nnop\nWritten ENDP\n"),
    ("written.lisp", "common-lisp", b"(defun written (value) value)\n"),
    ("Sample.pas", "pascal", b"unit Sample; interface procedure Measure(Value: Integer); implementation procedure Measure(Value: Integer); begin end; end.\n"),
    ("sample.cls", "apex", b"public class Written { public Integer Read(Integer Value) { return Value; } }\n"),
    ("sample.sas", "sas", b"function Measure(Value); return Value; endsub;\n"),
    ("sample.pks", "plsql", b"create or replace package Sample as\nprocedure Adjust(Value in number);\nend Sample;\n/\n"),
    (
        "sample.f90",
        "fortran",
        b"module Sample\ncontains\nsubroutine Measure(Value)\ninteger :: Value\nend subroutine Measure\nend module Sample\n",
    ),
    (
        "Sample.fs",
        "fsharp",
        b"module Sample\nlet measure value = value\n",
    ),
    (
        "Sample.fsi",
        "fsharp-signature",
        b"module Sample\nval measure: int -> int\n",
    ),
    (
        "sample-fixed.cbl",
        "cobol-fixed",
        b"000100 IDENTIFICATION DIVISION.\n000200 PROGRAM-ID. SAMPLE.\n000300 PROCEDURE DIVISION.\n000400     STOP RUN.\n",
    ),
    (
        "sample-free.cbl",
        "cobol-free",
        b"IDENTIFICATION DIVISION.\nPROGRAM-ID. SAMPLE.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
    ),
    (
        "Sample.hs",
        "haskell",
        b"module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\n",
    ),
    (
        "measure.bas",
        "vba",
        b"Public Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\n",
    ),
    (
        "sample.groovy",
        "groovy",
        b"class Sample { int read(int value) { value } }\n",
    ),
    (
        "sample.pm",
        "perl",
        b"package Sample; sub sample ($value) { return $value; }\n",
    ),
    (
        "sample.bas",
        "visual-basic-classic",
        b"Attribute VB_Name = \"Sample\"\nSub Run()\nEnd Sub\n",
    ),
    (
        "measure.m",
        "matlab",
        b"function result = measure(value)\nresult = value;\nend\n",
    ),
    (
        "sample.nix",
        "nix",
        b"let identity = value: value; in identity 1",
    ),
    (
        "sample.m",
        "objective-c",
        b"@interface Sample\n- (int)read:(int)value;\n@end\n",
    ),
    (
        "view.astro",
        "astro",
        b"---\nconst heading: string = 'Guide';\n---\n<main>{heading}</main>",
    ),
    (
        "sample.md",
        "markdown",
        b"# Guide\n\n[entry]: module.rs\n\n## Usage\nRead [entry].\n",
    ),
    (
        "sample.psm1",
        "powershell",
        b"function Read-Entry { param([string]$Name); return $Name }",
    ),
    (
        "sample.dart",
        "dart",
        b"class Store { int read(int value) => value; }",
    ),
    (
        "sample.scala",
        "scala",
        b"object Store { def read(value: Int) = value }",
    ),
    (
        "sample.sol",
        "solidity",
        b"contract Sample { event Changed(uint value); }",
    ),
    ("sample.R", "r", b"sample <- function(value) value\n"),
    ("sample.sql", "sql", b"CREATE TABLE sample (id INT);"),
    ("sample.html", "html", b"<main id='sample'>text</main>"),
    ("sample.yaml", "yaml", b"name: sample\n"),
    ("sample.toml", "toml", b"name = 'sample'\n"),
    ("sample.json", "json", br#"{"name":"sample"}"#),
    ("sample.jsonc", "jsonc", br#"{/*comment*/"name":"sample",}"#),
    ("sample.sh", "bash", b"sample() { :; }\n"),
    (
        "sample.lua",
        "lua",
        b"local function sample(value) return value end\n",
    ),
    ("sample.rs", "rust", b"fn sample() {}\n"),
    ("sample.rb", "ruby", b"def sample\n nil\nend\n"),
    ("sample.swift", "swift", b"func sample() {}\n"),
    ("sample.css", "css", b".sample { color: red; }\n"),
    ("sample.py", "python", b"def sample():\n    return None\n"),
    (
        "sample.js",
        "javascript",
        b"function sample() { return null; }\n",
    ),
    (
        "Sample.java",
        "java",
        b"class Sample { void sample() {} }\n",
    ),
    ("sample.go", "go", b"package sample\nfunc Sample() {}\n"),
    ("sample.ts", "typescript", b"function sample(): void {}\n"),
    ("sample.c", "c", b"int sample(void) { return 0; }\n"),
    (
        "sample.cpp",
        "cpp",
        b"namespace sample { int value() { return 0; } }\n",
    ),
    ("Sample.cs", "csharp", b"class Sample { void Run() {} }\n"),
    (
        "sample.kt",
        "kotlin",
        b"class Sample { fun run(): Unit {} }\n",
    ),
    (
        "sample.php",
        "php",
        b"<?php class Sample { public function run(): void {} }\n",
    ),
];

#[test]
fn deferred_queries_preserve_every_audited_language_and_normalized_ir() {
    let eager = Arc::new(TreeSitterProvider::new(config()).expect("complete query audit succeeds"));
    let deferred = Arc::new(
        TreeSitterProvider::new_deferred(config()).expect("deferred provider metadata validates"),
    );
    let registry = GrammarRegistry::audited().expect("audited grammar metadata validates");
    assert_eq!(eager.capabilities(), deferred.capabilities());
    assert_eq!(
        CASES
            .iter()
            .map(|(_, language, _)| *language)
            .collect::<BTreeSet<_>>(),
        registry
            .descriptors()
            .iter()
            .map(|entry| entry.language().as_str())
            .collect()
    );
    for (name, language, source) in CASES {
        let (parsed, analyzed) = assert_parity(&eager, &deferred, name, language, source);
        if language == "idris" {
            for name in ["global", "reader"] {
                assert_entity(&analyzed, language, name);
                assert_written_definition(&parsed, source, name.as_bytes());
            }
        } else if language == "pug" {
            for name in ["input", "type", "disabled"] {
                assert_entity(&analyzed, language, name);
                assert_written_definition(&parsed, source, name.as_bytes());
            }
        } else if language == "modelica" {
            assert_entity(&analyzed, language, "measure");
            assert_entity(&analyzed, language, "value");
            assert!(
                parsed
                    .facts()
                    .iter()
                    .any(|fact| fact.syntax_kind().as_str()
                        == "modelica.input_parameter.declaration")
            );
            assert_written_definition(&parsed, source, b"measure");
            assert_written_definition(&parsed, source, b"value");
        } else if language == "json-schema" {
            for name in ["Measurement", "value", "result"] {
                assert_entity(&analyzed, language, name);
                assert_written_definition(&parsed, source, name.as_bytes());
            }
            assert!(
                parsed
                    .facts()
                    .iter()
                    .any(|fact| fact.syntax_kind().as_str() == "json_schema.field.declaration")
            );
            let document = analyzed.document();
            let model = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Measurement")
                .unwrap();
            let field = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "value")
                .unwrap();
            let owner = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "result")
                .unwrap();
            for (predicate, subject, object) in [
                (
                    rootlight_ir::RelationPredicate::Contains,
                    model.id,
                    field.id,
                ),
                (
                    rootlight_ir::RelationPredicate::UsesType,
                    owner.id,
                    model.id,
                ),
            ] {
                assert!(
                    document
                        .relations
                        .iter()
                        .any(|edge| edge.predicate == predicate
                            && edge.subject == rootlight_ir::RelationEndpoint::Entity(subject)
                            && edge.object == rootlight_ir::RelationEndpoint::Entity(object))
                );
            }
        }
    }
    let (parsed, analyzed) = assert_parity(
        &eager,
        &deferred,
        "view.tsx",
        "typescript",
        b"export function View() { return <main>ready</main>; }\n",
    );
    assert!(
        parsed
            .facts()
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "typescript.function.declaration"),
        "TSX syntax labels: {:?}",
        parsed
            .facts()
            .iter()
            .map(|fact| fact.syntax_kind().as_str())
            .collect::<BTreeSet<_>>()
    );
    assert!(
        analyzed
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "View")
    );
}

#[test]
fn deferred_embedded_queries_preserve_child_languages_and_host_evidence() {
    let eager = Arc::new(TreeSitterProvider::new(config()).expect("complete query audit succeeds"));
    let deferred = Arc::new(TreeSitterProvider::new_deferred(config()).unwrap());
    let (_, html) = assert_parity(
        &eager,
        &deferred,
        "view.html",
        "html",
        b"<main><script>function client(value) { return value; }</script><style>.card { color: red; }</style></main>",
    );
    assert_entity(&html, "javascript", "client");
    assert_entity(&html, "css", ".card");
    let (_, astro) = assert_parity(
        &eager,
        &deferred,
        "view.astro",
        "astro",
        b"---\nfunction server(value: string) { return value; }\n---\n<style>.card { color: red; }</style><main>{(() => { const local = 1; return local; })()}</main>",
    );
    assert_entity(&astro, "typescript", "server");
    assert_entity(&astro, "typescript", "local");
    assert_entity(&astro, "css", ".card");
    let (markdown, analysis) = assert_parity(
        &eager,
        &deferred,
        "guide.md",
        "markdown",
        b"# Guide *emphasis*\n\n[entry](guide.md)\n\n```python\ndef fenced(value):\n    return value\n```\n",
    );
    assert!(markdown.facts().iter().any(|fact| {
        fact.syntax_kind()
            .as_str()
            .starts_with("markdown.inline_parsed.")
    }));
    assert_entity(&analysis, "python", "fenced");
    for output in [&html, &astro, &analysis] {
        assert_eq!(output.document().files.len(), 1);
        for entity in &output.document().entities {
            if let Some(source) = &entity.evidence.source {
                assert_eq!(source.span().file(), output.document().files[0].id);
            }
        }
    }
}

#[test]
fn deferred_cancellation_preserves_reasons_and_commits_no_provider_state() {
    let fixture = Fixture::new("cancelled.py", b"def retained(value):\n    return value\n");
    let budget = limits();
    let request = fixture.request("python", &budget);
    let provider = TreeSitterProvider::new_deferred(config()).unwrap();
    let initial = provider.stats();
    for reason in [
        CancellationReason::ClientRequest,
        CancellationReason::ParentCancelled,
        CancellationReason::DeadlineExceeded,
        CancellationReason::Shutdown,
        CancellationReason::ResourceLimit,
    ] {
        let cancellation = deadline();
        cancellation.cancel(reason);
        assert_eq!(
            execute_parse(&provider, &request, MEMORY_POLICY, &cancellation),
            Err(AdapterError::Cancelled { reason })
        );
        assert_eq!(
            provider.required_syntax_fact_count(&request, &cancellation),
            Err(AdapterError::Cancelled { reason })
        );
        assert_eq!(
            provider
                .execute_with_previous(
                    &request,
                    None,
                    &[],
                    settings(),
                    MEMORY_POLICY,
                    &cancellation,
                )
                .unwrap_err(),
            AdapterError::Cancelled { reason }
        );
        assert_eq!(provider.stats(), initial);
    }
    assert_eq!(
        execute_parse(
            &provider,
            &request,
            MEMORY_POLICY,
            &Cancellation::with_deadline(Instant::now()),
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::DeadlineExceeded
        })
    );
    assert_eq!(provider.stats(), initial);
    let recovered = execute_parse(&provider, &request, MEMORY_POLICY, &deadline()).unwrap();
    assert_eq!(
        recovered.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!recovered.facts().is_empty());
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn deferred_admission_rejects_invalid_requests_before_retaining_state() {
    let fixture = Fixture::new("admission.rs", b"fn admitted() {}\n");
    let budget = limits();
    let request = fixture.request("rust", &budget);
    let unsupported = fixture.request("unregistered", &budget);
    let provider = TreeSitterProvider::new_deferred(config()).unwrap();
    let initial = provider.stats();
    for (request, memory_policy, cancellation, error) in [
        (
            &request,
            MemoryAdmissionPolicy::RequireHardOrAccounted,
            deadline(),
            RequestError::MemoryEnforcementUnavailable,
        ),
        (
            &unsupported,
            MEMORY_POLICY,
            deadline(),
            RequestError::UnsupportedLanguage,
        ),
    ] {
        assert_eq!(
            execute_parse(&provider, request, memory_policy, &cancellation),
            Err(AdapterError::RejectedRequest(error))
        );
        assert_eq!(provider.stats(), initial);
    }
    assert!(
        !execute_parse(&provider, &request, MEMORY_POLICY, &Cancellation::new())
            .unwrap()
            .facts()
            .is_empty()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn shared_deferred_queries_do_not_share_provider_cache_or_incremental_identity() {
    let fixture = Fixture::new("identity.rs", b"fn owned() {}\n");
    let budget = limits();
    let request = fixture.request("rust", &budget);
    let first = TreeSitterProvider::new_deferred(config()).unwrap();
    let second = TreeSitterProvider::new_deferred(config()).unwrap();
    let initial = first
        .execute_with_previous(&request, None, &[], settings(), MEMORY_POLICY, &deadline())
        .unwrap();
    let previous = initial.previous().expect("first provider retains its tree");
    let first_stats = first.stats();
    assert_eq!(first_stats.cache.entries, 1);
    assert_eq!(second.stats().cache.entries, 0);
    assert_eq!(second.stats().pooled_parsers, 0);
    let unrelated = second
        .execute_with_previous(
            &request,
            Some(previous),
            &[],
            settings(),
            MEMORY_POLICY,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        unrelated.reuse_status(),
        ReuseStatus::Invalidated(ReuseInvalidation::Provider)
    );
    assert_eq!(unrelated.output(), initial.output());
    assert_eq!(first.stats(), first_stats);
    assert_eq!(second.stats().cache.entries, 1);
    let reused = first
        .execute_with_previous(
            &request,
            Some(previous),
            &[],
            settings(),
            MEMORY_POLICY,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        reused.reuse_status(),
        ReuseStatus::Reused { changed_ranges: 0 }
    );
    assert_eq!(reused.output(), initial.output());
    assert_eq!(first.stats().checked_out_parsers, 0);
    assert_eq!(second.stats().checked_out_parsers, 0);
}

fn assert_parity(
    eager: &Arc<TreeSitterProvider>,
    deferred: &Arc<TreeSitterProvider>,
    name: &str,
    language: &str,
    source: &[u8],
) -> (ParseOutput, AnalysisOutput) {
    let fixture = Fixture::new(name, source);
    let budget = limits();
    let request = fixture.request(language, &budget);
    let parsed = execute_parse(deferred.as_ref(), &request, MEMORY_POLICY, &deadline())
        .unwrap_or_else(|error| panic!("deferred {language}: {error}"));
    let eager_parsed = execute_parse(eager.as_ref(), &request, MEMORY_POLICY, &deadline())
        .unwrap_or_else(|error| panic!("eager {language}: {error}"));
    assert_eq!(parsed, eager_parsed, "parser parity for {name}");
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete,
        "{name}"
    );
    assert!(!parsed.facts().is_empty(), "{name}");
    assert_eq!(
        deferred
            .required_syntax_fact_count(&request, &deadline())
            .unwrap(),
        eager
            .required_syntax_fact_count(&request, &deadline())
            .unwrap(),
        "preflight parity for {name}",
    );
    let analyzed = analyze(deferred, &fixture, language, &budget);
    let eager_analyzed = analyze(eager, &fixture, language, &budget);
    assert_eq!(analyzed, eager_analyzed, "normalized IR parity for {name}");
    (parsed, analyzed)
}

fn assert_written_definition(parsed: &ParseOutput, source: &[u8], name: &[u8]) {
    assert!(
        parsed.facts().iter().any(|fact| {
            fact.syntax_kind().as_str().ends_with(".definition")
                && source.get(
                    usize::try_from(fact.span().start_byte()).unwrap()
                        ..usize::try_from(fact.span().end_byte()).unwrap(),
                ) == Some(name)
        }),
        "written definition {:?}",
        std::str::from_utf8(name).unwrap()
    );
}

fn analyze(
    provider: &Arc<TreeSitterProvider>,
    fixture: &Fixture,
    language: &str,
    limits: &AnalysisLimits,
) -> AnalysisOutput {
    let parser: Arc<dyn ParseProvider> = provider.clone();
    let analyzer = TreeSitterAnalyzer::new(
        parser,
        ProducerIdentity::new("deferred-contract", "1.0", content_hash(b"same-config")).unwrap(),
        LanguageId::new(language).unwrap(),
        "reviewed-query-contract",
        content_hash(b"same-binary"),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        LanguageId::new(language).unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"same-build")),
        limits,
    )
    .unwrap()
    .with_generated_status(false);
    execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MEMORY_POLICY,
        &deadline(),
    )
    .unwrap_or_else(|error| panic!("normalized {language}: {error}"))
}

fn assert_entity(output: &AnalysisOutput, language: &str, name: &str) {
    assert!(
        output
            .document()
            .entities
            .iter()
            .any(|entity| { entity.language == language && entity.canonical_name == name }),
        "missing {language} entity {name}"
    );
}

fn settings() -> ParserSettings {
    ParserSettings::new(1024).unwrap()
}

fn config() -> RuntimeConfig {
    RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        MAX_NODES,
        MAX_DEPTH,
        32,
        64,
        1,
        2 * 1024 * 1024,
        settings(),
    )
    .unwrap()
}

fn limits() -> AnalysisLimits {
    let batch = BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        MAX_NODES,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        batch,
    )
    .unwrap();
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        MAX_NODES,
        MAX_DEPTH,
        32,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

struct Fixture {
    _temporary: TempDir,
    snapshot: SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(temporary.path().join(name), bytes).unwrap();
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
        let snapshot = repository
            .snapshot(
                &RelativePath::parse(Path::new(name)).unwrap(),
                u64::try_from(MAX_SOURCE_BYTES).unwrap(),
            )
            .unwrap();
        let source = SourceRef::new(
            repository_id,
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .unwrap(),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _temporary: temporary,
            snapshot,
            source,
        }
    }

    fn request<'a>(&'a self, language: &str, limits: &'a AnalysisLimits) -> ParseRequest<'a> {
        ParseRequest::new(
            GenerationBoundSnapshot::new(&self.snapshot, &self.source).unwrap(),
            LanguageId::new(language).unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            limits,
        )
        .unwrap()
    }
}
