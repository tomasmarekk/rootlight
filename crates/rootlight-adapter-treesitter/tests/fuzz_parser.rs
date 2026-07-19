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
    MemoryAdmissionPolicy, ParseRequest, StreamLimits, execute_parse,
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
        for (name, language) in [
            ("fuzz.rs", "rust"),
            ("fuzz.py", "python"),
            ("fuzz.js", "javascript"),
            ("Fuzz.java", "java"),
            ("fuzz.go", "go"),
            ("fuzz.ts", "typescript"),
        ] {
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
                Err(error) => {
                    prop_assert!(false, "unexpected bounded parse error: {error:?}");
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

            let cleanup = Fixture::new("cleanup.txt", cleanup_source(language));
            let cleanup_limits = limits(256, 32);
            let cleanup_request = request(&cleanup, &cleanup_limits, language);
            prop_assert!(execute_parse(
                &provider,
                &cleanup_request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .is_ok());
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

fn cleanup_source(language: &str) -> &'static [u8] {
    match language {
        "rust" => b"fn cleanup() {}\n",
        "python" => b"def cleanup():\n    pass\n",
        "javascript" => b"function cleanup() {}\n",
        "java" => b"class Cleanup {}\n",
        "go" => b"package cleanup\nfunc cleanup() {}\n",
        "typescript" => b"function cleanup(): void {}\n",
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
