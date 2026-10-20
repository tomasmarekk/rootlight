//! Keep native input callbacks bounded even inside one long lexer/scanner token.
//! EOF used to stop native work never becomes an accepted recovery tree.

use super::{AdapterError, CANCELLATION_BYTE_INTERVAL, provider_failure};
use rootlight_cancel::{Cancellation, Cancelled};
use std::cell::Cell;

#[derive(Clone, Copy)]
enum Stop {
    Cancelled(Cancelled),
    Work(&'static str),
    ChunkWidth,
    ScalarTooWide,
    Boundary,
}

pub(super) struct Control<'a> {
    cancellation: &'a Cancellation,
    remaining: Cell<usize>,
    failure: &'static str,
    stop: Cell<Option<Stop>>,
}

impl<'a> Control<'a> {
    pub(super) fn new(
        maximum: usize,
        failure: &'static str,
        cancellation: &'a Cancellation,
    ) -> Self {
        Self {
            cancellation,
            remaining: Cell::new(maximum),
            failure,
            stop: Cell::new(None),
        }
    }

    pub(super) fn checkpoint(&self, shared: &Cell<Option<usize>>) -> bool {
        if self.stop.get().is_some() {
            return false;
        }
        if let Err(cancelled) = self.cancellation.check() {
            self.stop.set(Some(Stop::Cancelled(cancelled)));
            return false;
        }
        let next = self.remaining.get().saturating_sub(1);
        self.remaining.set(next);
        let shared_exhausted = shared.get().is_some_and(|remaining| {
            let next = remaining.saturating_sub(1);
            shared.set(Some(next));
            next == 0
        });
        if next == 0 || shared_exhausted {
            self.stop.set(Some(Stop::Work(self.failure)));
            return false;
        }
        true
    }

    pub(super) fn end(&self, source: &[u8], offset: usize, width: usize) -> Option<usize> {
        if self.stop.get().is_some() {
            return None;
        }
        if offset >= source.len() {
            return Some(source.len());
        }
        if width == 0 {
            self.stop.set(Some(Stop::ChunkWidth));
            return None;
        }
        // The native lexer retries from the scalar's original byte offset;
        // it does not concatenate an incomplete scalar across chunks.
        let scalar_width = match source[offset] {
            0x00..=0x7f => 1,
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                self.stop.set(Some(Stop::Boundary));
                return None;
            }
        };
        let maximum = width.min(CANCELLATION_BYTE_INTERVAL);
        if scalar_width > maximum {
            self.stop.set(Some(Stop::ScalarTooWide));
            return None;
        }
        let mut end = offset.saturating_add(maximum).min(source.len());
        for _ in 0..3 {
            if end == source.len() || source[end] & 0xc0 != 0x80 {
                break;
            }
            end -= 1;
        }
        if end < offset + scalar_width || (end < source.len() && source[end] & 0xc0 == 0x80) {
            self.stop.set(Some(Stop::Boundary));
            return None;
        }
        Some(end)
    }

    pub(super) fn error(&self) -> Option<AdapterError> {
        self.stop.get().map(|stop| match stop {
            Stop::Cancelled(cancelled) => cancelled.into(),
            Stop::Work(code) => provider_failure(code),
            Stop::ChunkWidth => provider_failure("native-input-chunk-width"),
            Stop::ScalarTooWide => provider_failure("native-input-chunk-too-small"),
            Stop::Boundary => provider_failure("native-input-scalar-boundary"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    #[test]
    fn cancellation_after_real_input_credit_remains_typed_and_terminal() {
        let cancellation = Cancellation::new();
        let control = Control::new(8, "syntax-node-parse-work-limit", &cancellation);
        let shared = Cell::new(None);
        assert!(control.checkpoint(&shared));
        assert_eq!(control.end("aλ😀z".as_bytes(), 0, 2), Some(1));
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(!control.checkpoint(&shared));
        assert!(matches!(
            control.error(),
            Some(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert_eq!(control.end(b"source", 0, 2), None);
        assert!(!control.checkpoint(&shared));
    }

    #[test]
    fn local_and_shared_native_work_are_consumed_without_replenishing_credit() {
        let cancellation = Cancellation::new();
        let control = Control::new(4, "syntax-node-parse-work-limit", &cancellation);
        let shared = Cell::new(Some(8));
        for expected in [7, 6, 5] {
            assert!(control.checkpoint(&shared));
            assert_eq!(shared.get(), Some(expected));
        }
        assert!(!control.checkpoint(&shared));
        assert_eq!(shared.get(), Some(4));
        assert!(
            matches!(control.error(), Some(AdapterError::ProviderFailed { code }) if code.as_str() == "syntax-node-parse-work-limit")
        );
        assert!(!control.checkpoint(&shared));
        assert_eq!(shared.get(), Some(4));
        let control = Control::new(16, "syntax-depth-parse-work-limit", &cancellation);
        let shared = Cell::new(Some(2));
        assert!(control.checkpoint(&shared));
        assert!(!control.checkpoint(&shared));
        assert_eq!(shared.get(), Some(0));
        assert!(
            matches!(control.error(), Some(AdapterError::ProviderFailed { code }) if code.as_str() == "syntax-depth-parse-work-limit")
        );
    }

    #[test]
    fn fitting_native_unicode_identifier_scalars_keep_exact_lookahead_and_chunk_maximum() {
        use std::sync::{Arc, Mutex};
        for (width, name) in [
            (1, "name"),
            (2, "name"),
            (3, "name"),
            (2, "λ"),
            (3, "λ"),
            (3, "界"),
            (4, "𐐀"),
            (64 * 1024, "𐐀"),
        ] {
            let text = format!("fn {name}() {{}}");
            let cancellation = Cancellation::new();
            let control = Control::new(16_384, "syntax-node-parse-work-limit", &cancellation);
            let shared = Cell::new(None);
            let logs = Arc::new(Mutex::new(Vec::new()));
            let native_logs = logs.clone();
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&crate::registry::language_for(crate::GrammarFamily::Rust))
                .unwrap();
            parser.set_logger(Some(Box::new(move |kind, message| {
                if kind == tree_sitter::LogType::Lex {
                    native_logs.lock().unwrap().push(message.to_owned());
                }
            })));
            let mut input = |offset: usize, _: tree_sitter::Point| {
                if !control.checkpoint(&shared) {
                    return b"".as_slice();
                }
                let Some(end) = control.end(text.as_bytes(), offset, width) else {
                    return b"".as_slice();
                };
                let chunk = text.as_bytes().get(offset..end).unwrap_or_default();
                assert!(chunk.len() <= width);
                assert!(std::str::from_utf8(chunk).is_ok());
                chunk
            };
            let tree = parser.parse_with_options(&mut input, None, None).unwrap();
            assert!(control.error().is_none(), "width={width}, {name}");
            assert!(
                !tree.root_node().has_error(),
                "width={width}, {name}: {}",
                tree.root_node().to_sexp()
            );
            let node = tree
                .root_node()
                .named_descendant_for_byte_range(3, 3 + name.len())
                .unwrap();
            assert_eq!(node.kind(), "identifier");
            assert_eq!(node.byte_range(), 3..3 + name.len());
            assert_eq!(&text[node.byte_range()], name);
            for scalar in name.chars().filter(|scalar| !scalar.is_ascii()) {
                let expected = format!("consume character:{}", u32::from(scalar));
                assert!(
                    logs.lock().unwrap().contains(&expected),
                    "missing exact native scalar {expected}, width={width}"
                );
            }
        }
        let cancellation = Cancellation::new();
        let control = Control::new(8, "syntax-node-parse-work-limit", &cancellation);
        let wide = vec![b'x'; 2 * CANCELLATION_BYTE_INTERVAL];
        assert_eq!(
            control.end(&wide, 0, wide.len()),
            Some(CANCELLATION_BYTE_INTERVAL)
        );
    }

    #[test]
    fn too_small_scalar_chunks_refuse_typed_before_accepting_native_recovery() {
        for (width, name) in [(1, "λ"), (1, "界"), (2, "界"), (1, "𐐀"), (2, "𐐀"), (3, "𐐀")]
        {
            let text = format!("fn {name}() {{}}");
            let cancellation = Cancellation::new();
            let control = Control::new(16_384, "syntax-node-parse-work-limit", &cancellation);
            let shared = Cell::new(None);
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&crate::registry::language_for(crate::GrammarFamily::Rust))
                .unwrap();
            let mut input = |offset: usize, _: tree_sitter::Point| {
                if !control.checkpoint(&shared) {
                    return b"".as_slice();
                }
                let Some(end) = control.end(text.as_bytes(), offset, width) else {
                    return b"".as_slice();
                };
                let chunk = text.as_bytes().get(offset..end).unwrap_or_default();
                assert!(chunk.len() <= width);
                assert!(std::str::from_utf8(chunk).is_ok());
                chunk
            };
            let partial = parser.parse_with_options(&mut input, None, None);
            assert!(
                matches!(control.error(), Some(AdapterError::ProviderFailed { code }) if code.as_str() == "native-input-chunk-too-small"),
                "width={width}, {name}"
            );
            // Stop EOF may produce a recovery tree. It is never usable evidence.
            drop(partial);
            parser.reset();
            assert!(
                !parser
                    .parse("fn clean() {}", None)
                    .unwrap()
                    .root_node()
                    .has_error()
            );
        }
    }

    #[test]
    fn sdk_scalar_width_refusal_keeps_pool_and_cache_empty_then_accepts_fitting_source() {
        use rootlight_adapter_sdk::{
            AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, LanguageId,
            MemoryAdmissionPolicy, ParseRequest, StreamLimits, execute_parse,
        };
        use rootlight_ids::{GenerationId, derive_repository};
        use rootlight_ir::{IrLimits, SourceRef, SourceSpan};
        use rootlight_vfs::{RelativePath, RepositoryRoot};
        use std::{fs, path::Path};
        for (width, name, refused) in [
            (1, "ascii", false),
            (2, "λ", false),
            (3, "界", false),
            (1, "λ", true),
            (2, "界", true),
            (3, "𐐀", true),
            (4, "𐐀", false),
        ] {
            let directory = tempfile::tempdir().unwrap();
            let text = format!("fn {name}() {{}}");
            fs::write(directory.path().join("source.rs"), text.as_bytes()).unwrap();
            let repository = derive_repository(b"native-input-scalar-width-contract").id();
            let root = RepositoryRoot::open(repository, directory.path()).unwrap();
            let snapshot = root
                .snapshot(
                    &RelativePath::parse(Path::new("source.rs")).unwrap(),
                    1024 * 1024,
                )
                .unwrap();
            let source = SourceRef::new(
                repository,
                GenerationId::from_bytes([15; 20]),
                SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
                snapshot.content_hash(),
                None,
            );
            let stream = StreamLimits::new(
                128,
                4096,
                16 * 1024 * 1024,
                128,
                128 * 1024,
                1024 * 1024,
                BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
            )
            .unwrap();
            let limits = AnalysisLimits::new(
                1024 * 1024,
                16_384,
                128,
                16,
                16 * 1024 * 1024,
                stream.clone(),
                stream,
                IrLimits::default(),
            )
            .unwrap();
            let request = ParseRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
                LanguageId::new("rust").unwrap(),
                EncodingId::utf8(),
                Vec::new(),
                &limits,
            )
            .unwrap();
            let provider = crate::TreeSitterProvider::new(
                crate::RuntimeConfig::new(
                    1024 * 1024,
                    16_384,
                    128,
                    16,
                    64,
                    1,
                    16 * 1024 * 1024,
                    crate::ParserSettings::new(width).unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            let output = execute_parse(
                &provider,
                &request,
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &Cancellation::new(),
            );
            if refused {
                assert!(
                    matches!(output, Err(AdapterError::ProviderFailed { code }) if code.as_str() == "native-input-chunk-too-small"),
                    "width={width}, {name}"
                );
                assert_eq!(provider.stats().cache.entries, 0);
            } else {
                let output = output.unwrap();
                assert!(!output.facts().is_empty());
                assert!(
                    !output
                        .diagnostics()
                        .iter()
                        .any(|diagnostic| diagnostic.code().as_str() == "syntax-error")
                );
            }
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().available_parsers, 1);
        }
        let cancellation = Cancellation::new();
        let control = Control::new(8, "syntax-node-parse-work-limit", &cancellation);
        assert_eq!(control.end("λ".as_bytes(), 1, 1), None);
        assert!(
            matches!(control.error(), Some(AdapterError::ProviderFailed { code }) if code.as_str() == "native-input-scalar-boundary")
        );
    }
}
