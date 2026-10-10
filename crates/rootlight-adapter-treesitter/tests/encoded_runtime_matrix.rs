//! Encoding equivalence across every audited native grammar/profile.
//! File-backed structural goldens and existing smoke representatives use the
//! same BOM-bearing UTF-8 text; raw UTF-16 evidence is checked independently.

use std::{
    collections::BTreeSet,
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseOutput, ParseRequest, StreamLimits, SyntaxFactKind, execute_parse,
};
use rootlight_adapter_treesitter::{
    GrammarRegistry, ParserSettings, RuntimeConfig, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{IrLimits, SourceRef, SourceSpan, SourceTextEncoding};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const MAX_SOURCE_BYTES: usize = 1024 * 1024;
const MEMORY_POLICY: MemoryAdmissionPolicy =
    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback;
const ENCODINGS: [SourceTextEncoding; 2] =
    [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be];

struct LanguageCase {
    name: &'static str,
    language: &'static str,
    source: &'static str,
}

const CASES: [LanguageCase; 48] = [
    LanguageCase {
        name: "written.wgsl",
        language: "wgsl",
        source: "fn Written(Value: f32) -> f32 { return Value; }\n",
    },
    LanguageCase {
        name: "written.hlsl",
        language: "hlsl",
        source: "float Written(float Value) { return Value; }\n",
    },
    LanguageCase {
        name: "written.glsl",
        language: "glsl",
        source: "float Written(float Value) { return Value; }\n",
    },
    LanguageCase {
        name: "written.asm",
        language: "assembly",
        source: "Written PROC\nnop\nWritten ENDP\n",
    },
    LanguageCase {
        name: "written.lisp",
        language: "common-lisp",
        source: "(defun written (value) value)\n",
    },
    LanguageCase {
        name: "sample.cls",
        language: "apex",
        source: "public class Written { public Integer Read(Integer Value) { return Value; } }\n",
    },
    LanguageCase {
        name: "sample.pks",
        language: "plsql",
        source: "create or replace package Sample as\nprocedure Adjust(Value in number);\nend Sample;\n/\n",
    },
    LanguageCase {
        name: "sample.f90",
        language: "fortran",
        source: "module Sample\ncontains\nsubroutine Measure(Value)\ninteger :: Value\nend subroutine Measure\nend module Sample\n",
    },
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
    LanguageCase {
        name: "structural.c",
        language: "c",
        source: include_str!("fixtures/structural/c.c"),
    },
    LanguageCase {
        name: "structural.cpp",
        language: "cpp",
        source: include_str!("fixtures/structural/cpp.cpp"),
    },
    LanguageCase {
        name: "Structural.cs",
        language: "csharp",
        source: include_str!("fixtures/structural/csharp.cs"),
    },
    LanguageCase {
        name: "structural.kt",
        language: "kotlin",
        source: include_str!("fixtures/structural/kotlin.kt"),
    },
    LanguageCase {
        name: "structural.php",
        language: "php",
        source: include_str!("fixtures/structural/php.php"),
    },
    LanguageCase {
        name: "structural.lua",
        language: "lua",
        source: include_str!("fixtures/structural/lua_golden.lua"),
    },
    LanguageCase {
        name: "structural.rb",
        language: "ruby",
        source: include_str!("fixtures/structural/ruby.rb"),
    },
    LanguageCase {
        name: "structural.swift",
        language: "swift",
        source: include_str!("fixtures/structural/swift.swift"),
    },
    LanguageCase {
        name: "structural.sh",
        language: "bash",
        source: include_str!("fixtures/structural/bash.sh"),
    },
    LanguageCase {
        name: "structural.css",
        language: "css",
        source: include_str!("fixtures/css.css"),
    },
    LanguageCase {
        name: "Sample.fs",
        language: "fsharp",
        source: include_str!("fixtures/fsharp/structural.fs"),
    },
    LanguageCase {
        name: "Sample.fsi",
        language: "fsharp-signature",
        source: include_str!("fixtures/fsharp/structural.fsi"),
    },
    LanguageCase {
        name: "sample-fixed.cbl",
        language: "cobol-fixed",
        source: "000100 IDENTIFICATION DIVISION.\n000200 PROGRAM-ID. SAMPLE.\n000300 PROCEDURE DIVISION.\n000400     STOP RUN.\n",
    },
    LanguageCase {
        name: "sample-free.cbl",
        language: "cobol-free",
        source: "IDENTIFICATION DIVISION.\nPROGRAM-ID. SAMPLE.\nPROCEDURE DIVISION.\n    STOP RUN.\n",
    },
    LanguageCase {
        name: "Sample.hs",
        language: "haskell",
        source: "module Sample where\npick :: Int -> Int\npick 0 = 1\npick value = value\n",
    },
    LanguageCase {
        name: "measure.bas",
        language: "vba",
        source: "Public Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\n",
    },
    LanguageCase {
        name: "sample.groovy",
        language: "groovy",
        source: "class Sample { int read(int value) { value } }\n",
    },
    LanguageCase {
        name: "sample.pm",
        language: "perl",
        source: "package Sample; sub sample ($value) { return $value; }\n",
    },
    LanguageCase {
        name: "sample.bas",
        language: "visual-basic-classic",
        source: "Attribute VB_Name = \"Sample\"\nSub Run()\nEnd Sub\n",
    },
    LanguageCase {
        name: "measure.m",
        language: "matlab",
        source: "function result = measure(value)\nresult = value;\nend\n",
    },
    LanguageCase {
        name: "sample.nix",
        language: "nix",
        source: "let identity = value: value; in identity 1",
    },
    LanguageCase {
        name: "sample.m",
        language: "objective-c",
        source: "@interface Sample\n- (int)read:(int)value;\n@end\n",
    },
    LanguageCase {
        name: "view.astro",
        language: "astro",
        source: "---\nconst heading: string = 'Guide';\n---\n<main>{heading}</main>",
    },
    LanguageCase {
        name: "sample.md",
        language: "markdown",
        source: "# Guide\n\n[entry]: module.rs\n\n## Usage\nRead [entry].\n",
    },
    LanguageCase {
        name: "sample.psm1",
        language: "powershell",
        source: "function Read-Entry { param([string]$Name); return $Name }",
    },
    LanguageCase {
        name: "sample.dart",
        language: "dart",
        source: "class Store { int read(int value) => value; }",
    },
    LanguageCase {
        name: "sample.scala",
        language: "scala",
        source: "object Store { def read(value: Int) = value }",
    },
    LanguageCase {
        name: "sample.sol",
        language: "solidity",
        source: "contract Sample { event Changed(uint value); }",
    },
    LanguageCase {
        name: "sample.R",
        language: "r",
        source: "sample <- function(value) value\n",
    },
    LanguageCase {
        name: "sample.sql",
        language: "sql",
        source: "CREATE TABLE sample (id INT);",
    },
    LanguageCase {
        name: "sample.html",
        language: "html",
        source: "<main id='sample'>text</main>",
    },
    LanguageCase {
        name: "sample.yaml",
        language: "yaml",
        source: "name: sample\n",
    },
    LanguageCase {
        name: "sample.toml",
        language: "toml",
        source: "name = 'sample'\n",
    },
    LanguageCase {
        name: "sample.json",
        language: "json",
        source: r#"{"name":"sample"}"#,
    },
];

#[test]
fn every_audited_language_preserves_projected_captures_and_original_encoded_evidence() {
    let registry = GrammarRegistry::audited().expect("audited registry initializes");
    let inventory: BTreeSet<_> = CASES.iter().map(|case| case.language).collect();
    assert_eq!(
        inventory.len(),
        CASES.len(),
        "matrix language profiles are unique"
    );
    assert_eq!(
        inventory,
        registry
            .descriptors()
            .iter()
            .map(|descriptor| descriptor.language().as_str())
            .collect::<BTreeSet<_>>(),
        "every audited language/profile needs an explicit neutral fixture"
    );

    let provider = provider();
    let budget = limits();
    for case in &CASES {
        let body = case
            .source
            .strip_prefix('\u{feff}')
            .unwrap_or(case.source)
            .replace("\r\n", "\n")
            .replace('\n', "\r\n");
        let text = format!("\u{feff}{body}");
        let control = Fixture::new(case.name, text.as_bytes());
        let control_output = parse(&provider, &control, &budget, case.language, "utf-8");
        assert_raw_report(&control_output, &control, case.language);
        assert_no_encoding_or_limit_diagnostics(&control_output, case.language);
        assert_eq!(provider.stats().checked_out_parsers, 0);

        for encoding in ENCODINGS {
            let raw = encode(&text, encoding);
            let fixture = Fixture::new(case.name, &raw);
            let output = parse(
                &provider,
                &fixture,
                &budget,
                case.language,
                encoding.as_str(),
            );
            assert_raw_report(&output, &fixture, case.language);
            assert_no_encoding_or_limit_diagnostics(&output, case.language);
            assert_eq!(fixture.snapshot.content(), raw);
            assert_eq!(fixture.source.content_hash(), content_hash(&raw));
            assert_capture_equivalence(
                &control_output,
                &output,
                &fixture,
                &text,
                encoding,
                case.language,
            );
            assert_diagnostic_equivalence(&control_output, &output, &fixture, &text, case.language);
            assert_eq!(
                output.report().coverage().status(),
                control_output.report().coverage().status(),
                "{} {encoding:?}",
                case.language
            );
            assert_eq!(
                output.report().coverage().skipped_regions(),
                control_output.report().coverage().skipped_regions(),
                "{} {encoding:?}",
                case.language
            );
            assert_eq!(
                output.report().resources().syntax_nodes(),
                control_output.report().resources().syntax_nodes(),
                "{} {encoding:?}",
                case.language
            );
            assert_eq!(
                output.report().resources().max_syntax_depth(),
                control_output.report().resources().max_syntax_depth(),
                "{} {encoding:?}",
                case.language
            );
            assert_eq!(provider.stats().checked_out_parsers, 0);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Capture<'a> {
    local_id: u64,
    parent: Option<u64>,
    kind: SyntaxFactKind,
    depth: usize,
    syntax: &'a str,
    start: usize,
    end: usize,
}

fn assert_capture_equivalence(
    control: &ParseOutput,
    encoded: &ParseOutput,
    fixture: &Fixture,
    text: &str,
    encoding: SourceTextEncoding,
    language: &str,
) {
    assert!(
        !control.facts().is_empty(),
        "{language} fixture has structural captures"
    );
    let expected: Vec<_> = control
        .facts()
        .iter()
        .map(|fact| Capture {
            local_id: fact.local_id(),
            parent: fact.parent(),
            kind: fact.kind(),
            depth: fact.depth(),
            syntax: fact.syntax_kind().as_str(),
            start: physical_offset(text, fact.span().start_byte()),
            end: physical_offset(text, fact.span().end_byte()),
        })
        .collect();
    let actual: Vec<_> = encoded
        .facts()
        .iter()
        .map(|fact| Capture {
            local_id: fact.local_id(),
            parent: fact.parent(),
            kind: fact.kind(),
            depth: fact.depth(),
            syntax: fact.syntax_kind().as_str(),
            start: usize::try_from(fact.span().start_byte()).unwrap(),
            end: usize::try_from(fact.span().end_byte()).unwrap(),
        })
        .collect();
    assert_eq!(actual, expected, "{language} {encoding:?}");
    for (control_fact, encoded_fact) in control.facts().iter().zip(encoded.facts()) {
        let span = encoded_fact.span();
        assert_eq!(span.file(), fixture.snapshot.file());
        assert!(span.start_byte().is_multiple_of(2));
        assert!(span.end_byte().is_multiple_of(2));
        let raw = fixture
            .snapshot
            .content()
            .get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            )
            .unwrap();
        let decoded: String =
            char::decode_utf16(raw.as_chunks::<2>().0.iter().map(|unit| match encoding {
                SourceTextEncoding::Utf16Le => u16::from_le_bytes(*unit),
                SourceTextEncoding::Utf16Be => u16::from_be_bytes(*unit),
            }))
            .collect::<Result<_, _>>()
            .expect("capture endpoints never split a surrogate pair");
        let control_text = text
            .get(
                usize::try_from(control_fact.span().start_byte()).unwrap()
                    ..usize::try_from(control_fact.span().end_byte()).unwrap(),
            )
            .unwrap();
        assert_eq!(
            decoded, control_text,
            "{language} {encoding:?} exact capture text"
        );
    }
}

fn assert_diagnostic_equivalence(
    control: &ParseOutput,
    encoded: &ParseOutput,
    fixture: &Fixture,
    text: &str,
    language: &str,
) {
    assert_eq!(
        encoded.diagnostics().len(),
        control.diagnostics().len(),
        "{language}"
    );
    for (expected, actual) in control.diagnostics().iter().zip(encoded.diagnostics()) {
        assert_eq!(actual.code(), expected.code(), "{language}");
        assert_eq!(actual.severity(), expected.severity(), "{language}");
        assert_eq!(
            actual.coverage_effect(),
            expected.coverage_effect(),
            "{language}"
        );
        match (expected.source(), actual.source()) {
            (Some(expected), Some(actual)) => {
                assert_eq!(actual.repository(), fixture.source.repository());
                assert_eq!(actual.generation(), fixture.source.generation());
                assert_eq!(actual.content_hash(), fixture.snapshot.content_hash());
                assert_eq!(actual.span().file(), fixture.snapshot.file());
                assert_eq!(
                    usize::try_from(actual.span().start_byte()).unwrap(),
                    physical_offset(text, expected.span().start_byte()),
                    "{language}"
                );
                assert_eq!(
                    usize::try_from(actual.span().end_byte()).unwrap(),
                    physical_offset(text, expected.span().end_byte()),
                    "{language}"
                );
            }
            (None, None) => {}
            _ => panic!("{language} diagnostic source presence changed"),
        }
    }
}

fn assert_raw_report(output: &ParseOutput, fixture: &Fixture, language: &str) {
    let raw = fixture.snapshot.content();
    assert_eq!(fixture.source.content_hash(), content_hash(raw));
    assert_eq!(
        fixture.source.span().end_byte(),
        u64::try_from(raw.len()).unwrap()
    );
    assert_eq!(
        output.report().coverage().total_source_bytes(),
        raw.len(),
        "{language}"
    );
    assert_eq!(
        output.report().coverage().covered_source_bytes(),
        raw.len(),
        "{language} full traversal accounts original bytes without claiming complete semantics"
    );
    assert_eq!(
        output.report().resources().source_bytes(),
        raw.len(),
        "{language}"
    );
}

fn assert_no_encoding_or_limit_diagnostics(output: &ParseOutput, language: &str) {
    for diagnostic in output.diagnostics() {
        let code = diagnostic.code().as_str();
        assert!(
            !code.contains("encoding")
                && !code.contains("utf")
                && !code.contains("fallback")
                && !code.contains("limit"),
            "{language} neutral fixture unexpectedly reports {code}"
        );
    }
}

fn physical_offset(text: &str, projected: u64) -> usize {
    let projected = usize::try_from(projected).unwrap();
    text.get(..projected)
        .expect("UTF-8 control endpoint is an exact scalar boundary")
        .encode_utf16()
        .count()
        * 2
}

fn encode(text: &str, encoding: SourceTextEncoding) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(|unit| match encoding {
            SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
            SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
        })
        .collect()
}

fn parse(
    provider: &TreeSitterProvider,
    fixture: &Fixture,
    budget: &AnalysisLimits,
    language: &str,
    encoding: &str,
) -> ParseOutput {
    let source = GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap();
    assert_eq!(source.bytes(), fixture.snapshot.content());
    assert_eq!(
        source.source_ref().content_hash(),
        fixture.snapshot.content_hash()
    );
    let request = ParseRequest::new(
        source,
        LanguageId::new(language).unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        budget,
    )
    .unwrap();
    execute_parse(provider, &request, MEMORY_POLICY, &deadline())
        .unwrap_or_else(|error| panic!("{language} {encoding} native parse failed: {error}"))
}

fn provider() -> TreeSitterProvider {
    TreeSitterProvider::new(
        RuntimeConfig::new(
            MAX_SOURCE_BYTES,
            16_384,
            128,
            16,
            64,
            1,
            2 * MAX_SOURCE_BYTES,
            ParserSettings::new(128).unwrap(),
        )
        .unwrap(),
    )
    .unwrap()
}

fn limits() -> AnalysisLimits {
    let batch = BatchThresholds::new(128, 256 * 1024, 8, 4096).unwrap();
    let stream =
        StreamLimits::new(64, 4096, 4 * 1024 * 1024, 64, 64 * 1024, 128 * 1024, batch).unwrap();
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        16_384,
        128,
        16,
        8 * MAX_SOURCE_BYTES,
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
    fn new(name: &str, raw: &[u8]) -> Self {
        let temporary = tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let relative = RelativePath::parse(Path::new(name)).unwrap();
        fs::write(temporary.path().join(relative.as_str()), raw).unwrap();
        let repository_id = "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v".parse().unwrap();
        let repository = RepositoryRoot::open(repository_id, temporary.path()).unwrap();
        let snapshot = repository
            .snapshot(&relative, u64::try_from(MAX_SOURCE_BYTES).unwrap())
            .unwrap();
        let source = SourceRef::new(
            repository_id,
            "gen1_is6sduoy6mt3wwxnzuibgq6rb6zs2jtal4aj2by"
                .parse()
                .unwrap(),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        Self {
            _temporary: temporary,
            snapshot,
            source,
        }
    }
}
