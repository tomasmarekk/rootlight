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
    fs::write(directory.path().join("source.ring"), text.as_bytes()).unwrap();
    let repository = derive_repository(b"ring-native-input-qualification").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.ring")).unwrap(),
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
        LanguageId::new("ring").unwrap(),
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

fn active_ring_regions(chunk: usize) {
    const REGION: usize = 128 * 1024;
    const CANCEL_AFTER: usize = 4096;
    for (name, source, message) in [
        (
            "body-identifier",
            format!(
                "class Written\nfunc measure value {{ local = {} }}\n",
                "q".repeat(REGION)
            ),
            " character:'q'",
        ),
        (
            "block-comment",
            format!(
                "class Written\nfunc measure value {{ /*{}*/ consume(value) }}\n",
                "q".repeat(REGION)
            ),
            " character:'q'",
        ),
        (
            "line-comment",
            format!(
                "class Written\nfunc measure value {{ //{}\n consume(value) }}\n",
                "q".repeat(REGION)
            ),
            " character:'q'",
        ),
        (
            "string",
            format!(
                "class Written\nfunc measure value {{ local = \"{}\" }}\n",
                "q".repeat(REGION)
            ),
            " character:'q'",
        ),
        (
            "external-formal-word",
            format!(
                "class Written\nfunc measure {} {{ return 1 }}\n",
                "q".repeat(REGION)
            ),
            " character:'q'",
        ),
        (
            "external-whitespace",
            format!(
                "class Written\nfunc measure value {{ {} }}\n",
                "\t".repeat(REGION)
            ),
            " character:9",
        ),
    ] {
        let provider = TreeSitterProvider::new(
            RuntimeConfig::new(
                8 * 1024 * 1024,
                1_048_576,
                256,
                16,
                128,
                1,
                64 * 1024 * 1024,
                ParserSettings::new(chunk).unwrap(),
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
            "real native lexical trigger must execute: {name}"
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
            "full region consumed after cancellation: {name}"
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
            "class Clean\nfunc measure value { consume(value) }\n",
            &Cancellation::new(),
        )
        .unwrap();
        assert!(
            !clean
                .diagnostics()
                .iter()
                .any(|item| item.code().as_str() == "syntax-error")
        );
        assert!(
            clean
                .facts()
                .iter()
                .any(|item| item.syntax_kind().as_str() == "ring.method.declaration")
        );
        assert_eq!(provider.stats().checked_out_parsers, 0);
    }
}

#[test]
fn active_ring_native_lexer_cancel_stops_before_long_token_and_reuses_clean_parser() {
    active_ring_regions(256);
}

#[test]
fn active_ring_wide_native_chunks_still_stop_before_long_token_and_reuse_clean_parser() {
    active_ring_regions(CANCELLATION_BYTE_INTERVAL * 4);
}
