//! Exercise actual provider cancellation after native lexical work has begun.
//! The existing parser logger supplies the deterministic observation point only.

use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseRequest, StreamLimits, execute_parse,
};
use rootlight_cancel::CancellationReason;
use rootlight_ids::{GenerationId, derive_repository};
use rootlight_ir::{IrLimits, SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tempfile::tempdir;

fn parse_text(
    provider: &TreeSitterProvider,
    text: &str,
    cancellation: &Cancellation,
) -> Result<rootlight_adapter_sdk::ParseOutput, AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.hx"), text.as_bytes()).unwrap();
    let repository = derive_repository(b"haxe-native-input-qualification").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.hx")).unwrap(),
            8 * 1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([14; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let stream = StreamLimits::new(
        1024,
        65_536,
        64 * 1024 * 1024,
        1024,
        1024 * 1024,
        8 * 1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        8 * 1024 * 1024,
        1_048_576,
        256,
        16,
        64 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = ParseRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("haxe").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        &limits,
    )
    .unwrap();
    execute_parse(
        provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancellation,
    )
}

#[test]
fn active_haxe_lexer_cancellation_stops_actual_provider_before_long_region_is_consumed() {
    for (name, open, fill, close, message) in [
        ("identifier", "var ", "q", " = 1;", " character:'q'"),
        ("block-comment", "/*", "q", "*/", " character:'q'"),
        ("line-comment", "//", "q", "\n", " character:'q'"),
        ("string", "var text = \"", "q", "\";", " character:'q'"),
        (
            "external-closing-brace-whitespace",
            "",
            "\t",
            "",
            " character:9",
        ),
    ] {
        const REGION: usize = 128 * 1024;
        const CANCEL_AFTER: usize = 4096;
        let source = format!(
            "class Written {{ static function measure() {{ {open}{}{close} }} }}",
            fill.repeat(REGION)
        );
        let provider = TreeSitterProvider::new(
            RuntimeConfig::new(
                8 * 1024 * 1024,
                1_048_576,
                256,
                16,
                128,
                1,
                64 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let consumed = Arc::new(AtomicUsize::new(0));
        let native_cancel = cancellation.clone();
        let native_consumed = consumed.clone();
        {
            let mut lease = provider.pool.acquire(&Cancellation::new()).unwrap();
            lease
                .parser_mut()
                .unwrap()
                .set_logger(Some(Box::new(move |kind, text| {
                    if kind == tree_sitter::LogType::Lex && text.ends_with(message) {
                        let count = native_consumed.fetch_add(1, Ordering::SeqCst) + 1;
                        if count == CANCEL_AFTER {
                            assert!(native_cancel.cancel(CancellationReason::ClientRequest));
                        }
                    }
                })));
        }
        let result = parse_text(&provider, &source, &cancellation);
        assert!(
            consumed.load(Ordering::SeqCst) >= CANCEL_AFTER,
            "native lexical point must execute before cancellation: {name}"
        );
        assert!(
            matches!(
                result,
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ),
            "{name}: {result:?}"
        );
        assert!(
            consumed.load(Ordering::SeqCst) < REGION,
            "actual provider consumed the complete long region after active cancellation: {name}, consumed={}",
            consumed.load(Ordering::SeqCst)
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().available_parsers, 1);
        assert_eq!(provider.stats().cache.entries, 0);
        {
            let mut lease = provider.pool.acquire(&Cancellation::new()).unwrap();
            lease.parser_mut().unwrap().set_logger(None);
        }
        let clean = parse_text(
            &provider,
            "class Clean { static function measure() { var value = 1; consume(value); } }",
            &Cancellation::new(),
        )
        .unwrap();
        assert!(
            !clean
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code().as_str() == "syntax-error")
        );
        assert!(
            clean
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "haxe.method.declaration")
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn active_haxe_wide_input_maximum_still_stops_before_the_long_region_is_consumed() {
    for (name, open, fill, close, message) in [
        ("identifier", "var ", "q", " = 1;", " character:'q'"),
        ("block-comment", "/*", "q", "*/", " character:'q'"),
        ("line-comment", "//", "q", "\n", " character:'q'"),
        ("string", "var text = \"", "q", "\";", " character:'q'"),
        (
            "external-closing-brace-whitespace",
            "",
            "\t",
            "",
            " character:9",
        ),
    ] {
        const REGION: usize = 128 * 1024;
        const CANCEL_AFTER: usize = 4096;
        let source = format!(
            "class Written {{ static function measure() {{ {open}{}{close} }} }}",
            fill.repeat(REGION)
        );
        let provider = TreeSitterProvider::new(
            RuntimeConfig::new(
                8 * 1024 * 1024,
                1_048_576,
                256,
                16,
                128,
                1,
                64 * 1024 * 1024,
                ParserSettings::new(CANCELLATION_BYTE_INTERVAL * 4).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let cancellation = Cancellation::new();
        let consumed = Arc::new(AtomicUsize::new(0));
        let native_cancel = cancellation.clone();
        let native_consumed = consumed.clone();
        {
            let mut lease = provider.pool.acquire(&Cancellation::new()).unwrap();
            lease
                .parser_mut()
                .unwrap()
                .set_logger(Some(Box::new(move |kind, text| {
                    if kind == tree_sitter::LogType::Lex && text.ends_with(message) {
                        let count = native_consumed.fetch_add(1, Ordering::SeqCst) + 1;
                        if count == CANCEL_AFTER {
                            assert!(native_cancel.cancel(CancellationReason::ClientRequest));
                        }
                    }
                })));
        }
        let result = parse_text(&provider, &source, &cancellation);
        assert!(
            consumed.load(Ordering::SeqCst) >= CANCEL_AFTER,
            "native lexical point must execute before cancellation: {name}"
        );
        assert!(
            matches!(
                result,
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ),
            "{name}: {result:?}"
        );
        assert!(
            consumed.load(Ordering::SeqCst) < REGION,
            "actual provider consumed the complete long region after active cancellation: {name}, consumed={}",
            consumed.load(Ordering::SeqCst)
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().available_parsers, 1);
        assert_eq!(provider.stats().cache.entries, 0);
        {
            let mut lease = provider.pool.acquire(&Cancellation::new()).unwrap();
            lease.parser_mut().unwrap().set_logger(None);
        }
        let clean = parse_text(
            &provider,
            "class Clean { static function measure() { var value = 1; consume(value); } }",
            &Cancellation::new(),
        )
        .unwrap();
        assert!(
            !clean
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code().as_str() == "syntax-error")
        );
        assert!(
            clean
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == "haxe.method.declaration")
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}
