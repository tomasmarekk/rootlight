//! Stable property coverage for bounded parser extraction.
//!
//! CI-safe generated inputs span invalid UTF-8, Unicode/CRLF, incomplete
//! nesting, and token storms without requiring nightly fuzzing toolchains.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use proptest::prelude::*;
use proptest::test_runner::{RngAlgorithm, RngSeed};
use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseRequest, ResourceKind, SinkError, StreamLimits, execute_parse,
};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, TreeSitterProvider};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ir::{CoverageStatus, IrLimits, SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir_in;

const MAX_SOURCE_BYTES: usize = 4096;
const FUZZ_CASES: u32 = 24;
// CI replays one reviewed corpus; broader random campaigns use a separate runner config.
const FUZZ_SEED: u64 = 202_607_170_404;
const FUZZ_ROUTES: [(&str, &str); 20] = [
    ("fuzz.dart", "dart"),
    ("fuzz.toml", "toml"),
    ("fuzz.json", "json"),
    ("fuzz.rs", "rust"),
    ("fuzz.py", "python"),
    ("fuzz.js", "javascript"),
    ("Fuzz.java", "java"),
    ("fuzz.go", "go"),
    ("fuzz.ts", "typescript"),
    ("fuzz.tsx", "typescript"),
    ("fuzz.rb", "ruby"),
    ("fuzz.c", "c"),
    ("fuzz.cpp", "cpp"),
    ("Fuzz.cs", "csharp"),
    ("fuzz.kt", "kotlin"),
    ("fuzz.php", "php"),
    ("fuzz.lua", "lua"),
    ("fuzz.swift", "swift"),
    ("fuzz.css", "css"),
    ("fuzz.sh", "bash"),
];

#[test]
fn html_embedded_hostile_sources_share_bounds_and_release_the_parser() {
    let provider = provider();
    let nested = format!(
        "<script>let value = {}1{};</script>",
        "(".repeat(128),
        ")".repeat(128)
    );
    let storm = "<script>function entry() { return 1; }</script>".repeat(30);
    for source in [
        nested.as_bytes(),
        storm.as_bytes(),
        b"<script>function broken(</script><style>.ok{color:red}</style>",
        b"<style>\0\xff</style>",
        "<script>const text = '雪';\r\n</script>".as_bytes(),
    ] {
        for (nodes, depth) in [(1, 1), (16, 4), (64, 16), (256, 32)] {
            let fixture = Fixture::new("input.html", source);
            let budget = limits(nodes, depth);
            let input = request(&fixture, &budget, "html");
            match execute_parse(
                &provider,
                &input,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            ) {
                Ok(output) => {
                    assert!(output.report().resources().syntax_nodes() <= nodes);
                    assert!(output.report().resources().max_syntax_depth() <= depth);
                    assert!(output.facts().len() <= 8);
                    assert!(output.facts().iter().all(|fact| {
                        usize::try_from(fact.span().end_byte()).is_ok_and(|end| end <= source.len())
                    }));
                }
                Err(AdapterError::ProviderFailed { code }) => {
                    if code.as_str() == "invalid-utf8" {
                        assert!(std::str::from_utf8(source).is_err());
                    } else {
                        assert!(
                            matches!(
                                code.as_str(),
                                "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
                            ),
                            "{code:?}"
                        );
                    }
                }
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed,
                    limit,
                })) => {
                    assert_eq!(limit, 8);
                    assert!(observed > limit);
                    assert_eq!(
                        provider.required_syntax_fact_count(&input, &deadline()),
                        Ok(observed)
                    );
                }
                other => panic!("unexpected bounded HTML parse: {other:?}"),
            }
            let cancellation = deadline();
            assert!(cancellation.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                execute_parse(
                    &provider,
                    &input,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(provider.stats().checked_out_parsers, 0);
        }
    }
    let cleanup = Fixture::new("cleanup.html", b"<p>safe</p>");
    let budget = limits(256, 32);
    assert!(
        execute_parse(
            &provider,
            &request(&cleanup, &budget, "html"),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        )
        .is_ok()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}

#[test]
fn cleanup_fixtures_fit_the_bounded_parser_budget() {
    let provider = provider();
    let limits = limits(256, 32);
    for (name, language) in FUZZ_ROUTES {
        let fixture = Fixture::new(name, cleanup_source(name, language));
        let request = request(&fixture, &limits, language);
        let result = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        );
        assert!(result.is_ok(), "{name}: {result:?}");
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn powershell_hostile_sources_preserve_budgets_cancellation_and_parser_cleanup() {
    let provider = provider();
    let mut pre_parse_limits = 0;
    let nested = format!("$value = {}1{}", "(".repeat(256), ")".repeat(256));
    let storm = "function Read-Entry { param($Name); return $Name }\n".repeat(40);
    let casts = format!("{}$value = 1", "[int]".repeat(64));
    let targets = format!("{}$last = 1", "$value, ".repeat(40));
    let tables = "@{ Key = @{ Nested = 1 }; $key = @{ Value = 2 } }\n".repeat(40);
    let methods = format!("$items{}", ".Apply{ $_ }".repeat(64));
    let arguments = format!("Invoke-Entry {}", "pre$($name)post, ".repeat(64));
    let sources: &[&[u8]] = &[
        &[0xff, 0xfe, 0x80],
        b"function Broken { param(",
        b"$value = @'\nunfinished",
        "$value = '雪'\r\n$value = 2\r\n".as_bytes(),
        nested.as_bytes(),
        storm.as_bytes(),
        casts.as_bytes(),
        targets.as_bytes(),
        tables.as_bytes(),
        methods.as_bytes(),
        arguments.as_bytes(),
    ];
    for source in sources {
        for (max_nodes, max_depth) in [(1, 1), (16, 4), (256, 32)] {
            let fixture = Fixture::new("input.ps1", source);
            let budget = limits(max_nodes, max_depth);
            let request = request(&fixture, &budget, "powershell");
            match execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            ) {
                Ok(output) => {
                    assert!(std::str::from_utf8(source).is_ok());
                    assert!(output.report().resources().syntax_nodes() <= max_nodes);
                    assert!(output.report().resources().max_syntax_depth() <= max_depth);
                    assert!(output.facts().len() <= 8);
                    assert!(output.facts().iter().all(|fact| {
                        usize::try_from(fact.span().end_byte()).is_ok_and(|end| end <= source.len())
                    }));
                }
                Err(AdapterError::ProviderFailed { code })
                    if matches!(
                        code.as_str(),
                        "syntax-node-parse-work-limit" | "syntax-depth-parse-work-limit"
                    ) =>
                {
                    // Native work may exhaust admission before a tree exists; that is
                    // a typed rejection, never a successful partial publication.
                    assert!(std::str::from_utf8(source).is_ok());
                    pre_parse_limits += 1;
                }
                Err(AdapterError::ProviderFailed { code }) => {
                    assert_eq!(code.as_str(), "invalid-utf8");
                    assert!(std::str::from_utf8(source).is_err());
                }
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed,
                    limit,
                })) => {
                    assert_eq!(limit, 8);
                    assert!(observed > limit);
                    assert_eq!(
                        provider.required_syntax_fact_count(&request, &deadline()),
                        Ok(observed)
                    );
                }
                other => panic!("unexpected bounded PowerShell parse: {other:?}"),
            }
            let cancellation = deadline();
            assert!(cancellation.cancel(CancellationReason::ClientRequest));
            assert!(matches!(
                execute_parse(
                    &provider,
                    &request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancellation
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert_eq!(provider.stats().checked_out_parsers, 0);
        }
        let cleanup = Fixture::new("cleanup.ps1", b"function Cleanup {}\n");
        let budget = limits(256, 32);
        assert!(
            execute_parse(
                &provider,
                &request(&cleanup, &budget, "powershell"),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_ok()
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
    assert!(pre_parse_limits > 0);
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: FUZZ_CASES,
        max_shrink_iters: 256,
        failure_persistence: None,
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(FUZZ_SEED),
        ..ProptestConfig::default()
    })]

    #[test]
    fn bounded_parser_inputs_preserve_bounds_and_cleanup(
        input in adversarial_input(),
        max_nodes in 1usize..=256,
        max_depth in 1usize..=32,
    ) {
        for (name, language) in FUZZ_ROUTES {
            let provider = provider();
            let fixture = Fixture::new(name, &input);
            let fuzz_limits = limits(max_nodes, max_depth);
            let fuzz_request = request(&fixture, &fuzz_limits, language);
            let cancellation = deadline();
            let result = execute_parse(
                &provider,
                &fuzz_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            );

            match result {
                Ok(output) => {
                    prop_assert!(std::str::from_utf8(&input).is_ok());
                    prop_assert!(output.report().resources().syntax_nodes() <= max_nodes);
                    prop_assert!(output.report().resources().max_syntax_depth() <= max_depth);
                    prop_assert!(output.facts().len() <= 8);
                    prop_assert!(matches!(
                        output.report().coverage().status(),
                        CoverageStatus::Complete
                            | CoverageStatus::Bounded
                            | CoverageStatus::Unknown
                    ));
                    for fact in output.facts() {
                        prop_assert!(
                            usize::try_from(fact.span().end_byte())
                                .is_ok_and(|end| end <= input.len())
                        );
                        prop_assert!(fact.syntax_kind().as_str().len() <= 128);
                    }
                    for diagnostic in output.diagnostics() {
                        prop_assert!(diagnostic.code().as_str().len() <= 64);
                    }
                }
                Err(AdapterError::ProviderFailed { code }) => {
                    prop_assert_eq!(code.as_str(), "invalid-utf8");
                    prop_assert!(std::str::from_utf8(&input).is_err());
                }
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::DeadlineExceeded,
                }) => {}
                Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: ResourceKind::RequiredSyntaxFacts,
                    observed,
                    limit,
                })) => {
                    // Identity facts are atomic: overflow must fail closed, not
                    // publish an incomplete declaration under a success status.
                    prop_assert_eq!(limit, 8);
                    prop_assert!(observed > limit);
                    let required = provider.required_syntax_fact_count(
                        &fuzz_request,
                        &deadline(),
                    );
                    prop_assert_eq!(required, Ok(observed));
                }
                Err(error) => {
                    prop_assert!(false, "{name}: unexpected bounded parse error: {error:?}");
                }
            }

            let cancelled = deadline();
            prop_assert!(cancelled.cancel(CancellationReason::ClientRequest));
            let cancellation_observed = matches!(
                execute_parse(
                    &provider,
                    &fuzz_request,
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancelled,
                ),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            );
            prop_assert!(cancellation_observed);

            let cleanup_bytes = cleanup_source(name, language);
            let cleanup = Fixture::new(name, cleanup_bytes);
            let cleanup_limits = limits(256, 32);
            let cleanup_request = request(&cleanup, &cleanup_limits, language);
            let cleanup_result = execute_parse(
                &provider,
                &cleanup_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            prop_assert!(cleanup_result.is_ok(), "{}: {:?}", name, cleanup_result);
            prop_assert_eq!(provider.stats().checked_out_parsers, 0);
        }
    }
}

fn adversarial_input() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        4 => proptest::collection::vec(any::<u8>(), 0..=2048),
        3 => proptest::collection::vec(any::<char>(), 0..=512)
            .prop_map(|characters| characters.into_iter().collect::<String>().into_bytes()),
        2 => (0usize..=1024).prop_map(|count| {
            let mut source = "α\r\n".repeat(count / 16).into_bytes();
            source.extend(std::iter::repeat_n(b'(', count));
            source
        }),
        2 => proptest::collection::vec(
            prop_oneof![
                Just("identifier"),
                Just("{"),
                Just("}"),
                Just("("),
                Just(")"),
                Just("\""),
                Just("//"),
                Just("\r\n"),
                Just("🦀"),
            ],
            0..=256,
        )
        .prop_map(|tokens| tokens.concat().into_bytes()),
    ]
}

fn provider() -> TreeSitterProvider {
    let settings = ParserSettings::new(256).expect("fuzz parser settings are valid");
    let config = RuntimeConfig::new(
        MAX_SOURCE_BYTES,
        1024,
        64,
        8,
        8,
        1,
        2 * 1024 * 1024,
        settings,
    )
    .expect("fuzz runtime config is valid");
    TreeSitterProvider::new(config).expect("audited provider initializes")
}

fn limits(max_nodes: usize, max_depth: usize) -> AnalysisLimits {
    let syntax_batch =
        BatchThresholds::new(4, 4096, 4, 1024).expect("syntax batch limits are valid");
    let syntax = StreamLimits::new(8, 8, 16 * 1024, 8, 4096, 4096, syntax_batch)
        .expect("syntax stream limits are valid");
    let ir_batch = BatchThresholds::new(8, 4096, 4, 1024).expect("IR batch limits are valid");
    let ir = StreamLimits::new(8, 32, 32 * 1024, 8, 4096, 4096, ir_batch)
        .expect("IR stream limits are valid");
    AnalysisLimits::new(
        MAX_SOURCE_BYTES,
        max_nodes,
        max_depth,
        8,
        2 * 1024 * 1024,
        syntax,
        ir,
        IrLimits::default(),
    )
    .expect("fuzz analysis limits are valid")
}

fn request<'a>(
    fixture: &'a Fixture,
    limits: &'a AnalysisLimits,
    language: &str,
) -> ParseRequest<'a> {
    ParseRequest::new(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source)
            .expect("fuzz snapshot binds"),
        LanguageId::new(language).expect("fuzz language is valid"),
        EncodingId::new("utf-8").expect("fuzz encoding is valid"),
        Vec::new(),
        limits,
    )
    .expect("fuzz request is valid")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("fuzz deadline is representable"),
    )
}

// Cleanup probes must fit the unchanged eight-fact identity budget themselves;
// otherwise shrinking the unrelated adversarial input cannot explain failure.
fn cleanup_source(name: &str, language: &str) -> &'static [u8] {
    if name.ends_with(".tsx") {
        return b"function Cleanup() { return <span />; }\n";
    }
    match language {
        "dart" => b"void cleanup() {}\n",
        "rust" => b"fn cleanup() {}\n",
        "python" => b"def cleanup():\n    pass\n",
        "javascript" => b"function cleanup() {}\n",
        "java" => b"class Cleanup {}\n",
        "go" => b"package cleanup\nfunc cleanup() {}\n",
        "typescript" => b"function cleanup(): void {}\n",
        "c" => b"int cleanup(void) { return 0; }\n",
        "cpp" => b"int cleanup() { return 0; }\n",
        "csharp" => b"class Cleanup {}\n",
        "kotlin" => b"class Cleanup\n",
        "php" => b"<?php class Cleanup {}\n",
        "lua" => b"local function cleanup() return 1 end\n",
        "ruby" => b"def cleanup()\nend\n",
        "swift" => b"func cleanup() {}\n",
        "css" => b".cleanup { color: red; }\n",
        "bash" => b"cleanup() { :; }\n",
        "json" => br#"{"cleanup":true}"#,
        "toml" => b"cleanup = true\n",
        _ => b"",
    }
}

struct Fixture {
    _temporary: tempfile::TempDir,
    snapshot: rootlight_vfs::SourceSnapshot,
    source: SourceRef,
}

impl Fixture {
    fn new(name: &str, bytes: &[u8]) -> Self {
        let current = std::env::current_dir().expect("current directory exists");
        let temporary = tempdir_in(current).expect("local temporary directory is available");
        fs::write(temporary.path().join(name), bytes).expect("fuzz source is written");
        let repository_id = rootlight_ids::RepositoryId::from_bytes([51; 16]);
        let repository =
            RepositoryRoot::open(repository_id, temporary.path()).expect("repository opens");
        let relative = RelativePath::parse(Path::new(name)).expect("fuzz path is valid");
        let snapshot = repository
            .snapshot(&relative, MAX_SOURCE_BYTES as u64)
            .expect("fuzz snapshot is stable");
        let end = u64::try_from(snapshot.content().len()).expect("fuzz length fits");
        let source = SourceRef::new(
            repository_id,
            rootlight_ids::GenerationId::from_bytes([52; 20]),
            SourceSpan::new(snapshot.file(), 0, end).expect("fuzz span is ordered"),
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
