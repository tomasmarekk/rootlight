//! Stream-backed behavior is compared with existing whole-snapshot oracles.
//! The test capability authenticates complete raw bytes and can fail after its
//! final callback, independently of the consumer's text and staging decisions.

use super::*;
use rootlight_vfs::TextEncoding;

struct Backing<'a> {
    generation: &'a GenerationSnapshot,
    bytes: &'a [u8],
    chunk: usize,
    calls: Cell<usize>,
    offset_error: bool,
    length_error: bool,
    final_error: bool,
    cancel_after_callback: bool,
    retry_rejected_callback: bool,
}

impl std::fmt::Debug for Backing<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Backing")
            .field("generation", &self.generation.metadata().generation())
            .finish_non_exhaustive()
    }
}

impl<'a> Backing<'a> {
    fn new(generation: &'a GenerationSnapshot, bytes: &'a [u8], chunk: usize) -> Self {
        Self {
            generation,
            bytes,
            chunk,
            calls: Cell::new(0),
            offset_error: false,
            length_error: false,
            final_error: false,
            cancel_after_callback: false,
            retry_rejected_callback: false,
        }
    }
}

impl VerifiedSourceBacking for Backing<'_> {
    fn repository(&self) -> RepositoryId {
        self.generation.metadata().repository()
    }
    fn generation(&self) -> GenerationId {
        self.generation.metadata().generation()
    }
    fn visit_verified(
        &self,
        file: &FileRecord,
        maximum: u64,
        cancellation: &Cancellation,
        visitor: &mut dyn FnMut(u64, &[u8]) -> Result<(), SourceError>,
    ) -> Result<u64, SourceError> {
        cancellation
            .check()
            .map_err(|error| SourceError::Cancelled(error.reason()))?;
        self.calls.set(self.calls.get() + 1);
        if self.generation.find_file(file.id) != Some(file) {
            return Err(SourceError::FileNotFound);
        }
        if file.byte_length > maximum {
            return Err(SourceError::VerificationBudgetExceeded);
        }
        let mut consumed = 0u64;
        for bytes in self.bytes.chunks(self.chunk) {
            if self.retry_rejected_callback {
                let _ = visitor(consumed + 1, bytes);
            }
            visitor(consumed + u64::from(self.offset_error), bytes)?;
            consumed += u64::try_from(bytes.len()).unwrap();
            if self.cancel_after_callback {
                let _ = cancellation.cancel(CancellationReason::ParentCancelled);
            }
        }
        if self.final_error || content_hash(self.bytes) != file.content_hash {
            return Err(SourceError::StaleSource);
        }
        Ok(consumed - u64::from(self.length_error))
    }
}

fn encode(text: &str, encoding: TextEncoding, utf8_bom: bool) -> Vec<u8> {
    match encoding {
        TextEncoding::Utf8 => {
            if utf8_bom {
                [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()
            } else {
                text.as_bytes().to_vec()
            }
        }
        TextEncoding::Utf16Le | TextEncoding::Utf16Be => std::iter::once(0xfeff)
            .chain(text.encode_utf16())
            .flat_map(|unit| {
                if encoding == TextEncoding::Utf16Le {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect(),
    }
}

fn offset(text: &str, encoding: TextEncoding, utf8_bom: bool) -> u64 {
    u64::try_from(encode(text, encoding, utf8_bom).len()).unwrap()
}

fn raw_options() -> SourceReadOptions {
    SourceReadOptions::new()
        .with_context_lines_before(0)
        .with_context_lines_after(0)
        .with_encoding(SourceEncoding::Bytes)
}

#[test]
fn streamed_text_context_and_prefixes_match_whole_source_for_every_chunk_split() {
    let text = "head\r\nλ😀tail\nthird\nlast";
    for (encoding, bom) in [
        (TextEncoding::Utf8, false),
        (TextEncoding::Utf8, true),
        (TextEncoding::Utf16Le, false),
        (TextEncoding::Utf16Be, false),
    ] {
        let bytes = encode(text, encoding, bom);
        let start = offset("head\r\n", encoding, bom);
        let end = offset("head\r\nλ😀", encoding, bom);
        let fixture = fixture(&bytes, encoding.as_str(), (start, end));
        let ordinary = SourceService::new(&fixture.root, &fixture.generation).unwrap();
        for chunk in 1..=8 {
            let backing = Backing::new(&fixture.generation, &bytes, chunk);
            let streamed =
                SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
            for (before, after) in [(0, 0), (1, 1), (2, 0), (0, 2)] {
                let options = LosslessTextReadOptions::new()
                    .with_context_lines_before(before)
                    .with_context_lines_after(after);
                let references = [fixture.reference.clone(), fixture.reference.clone()];
                let a = ordinary
                    .read_lossless_text(
                        &references,
                        options,
                        SourceBudget::new(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                let previous = backing.calls.get();
                let b = streamed
                    .read_lossless_text(
                        &references,
                        options,
                        SourceBudget::new(),
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert_eq!(
                    backing.calls.get() - previous,
                    1,
                    "duplicate selectors share one authenticated visit"
                );
                assert_eq!(
                    a.chunks(),
                    b.chunks(),
                    "{encoding:?}/{bom}/{chunk}/{before}/{after}"
                );
                assert_eq!(a.total_text_bytes(), b.total_text_bytes());
                assert_eq!(a.total_source_bytes(), b.total_source_bytes());
                assert!(
                    b.total_response_memory_bytes()
                        <= SourceBudget::new().max_response_memory_bytes
                );
                assert!(matches!(
                    streamed.read_lossless_text_prefix_with_options(
                        &references,
                        options,
                        0,
                        SourceBudget::new(),
                        &Cancellation::new()
                    ),
                    Err(SourceError::InvalidBudget)
                ));
                for maximum in [1, 5, 31] {
                    let a = ordinary
                        .read_lossless_text_prefix_with_options(
                            &references,
                            options,
                            maximum,
                            SourceBudget::new(),
                            &Cancellation::new(),
                        )
                        .unwrap();
                    let b = streamed
                        .read_lossless_text_prefix_with_options(
                            &references,
                            options,
                            maximum,
                            SourceBudget::new(),
                            &Cancellation::new(),
                        )
                        .unwrap();
                    assert_eq!(
                        a.chunks(),
                        b.chunks(),
                        "prefix {encoding:?}/{chunk}/{before}/{after}/{maximum}"
                    );
                    assert_eq!(a.truncated(), b.truncated());
                }
            }
            let a = ordinary
                .read_lossless_text_prefix(
                    std::slice::from_ref(&fixture.reference),
                    5,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
            let b = streamed
                .read_lossless_text_prefix(
                    std::slice::from_ref(&fixture.reference),
                    5,
                    SourceBudget::new(),
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(a.chunks(), b.chunks());
        }
    }
}

#[test]
fn streamed_prefix_keeps_the_beginning_of_a_long_prior_context_line() {
    let text = format!("{}λ😀\nselected\nend", "a".repeat(100_000));
    for encoding in [
        TextEncoding::Utf8,
        TextEncoding::Utf16Le,
        TextEncoding::Utf16Be,
    ] {
        let bytes = encode(&text, encoding, false);
        let start = offset(&text[..text.find("selected").unwrap()], encoding, false);
        let fixture = fixture(&bytes, encoding.as_str(), (start, start + 2));
        let backing = Backing::new(&fixture.generation, &bytes, 4093);
        let streamed = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
        let options = LosslessTextReadOptions::new().with_context_lines_before(1);
        let ordinary = SourceService::new(&fixture.root, &fixture.generation).unwrap();
        let a = ordinary
            .read_lossless_text_prefix_with_options(
                std::slice::from_ref(&fixture.reference),
                options,
                17,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
        let b = streamed
            .read_lossless_text_prefix_with_options(
                std::slice::from_ref(&fixture.reference),
                options,
                17,
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(a.chunks(), b.chunks());
        assert_eq!(a.truncated(), b.truncated());
        assert_eq!(b.chunks()[0].raw().start_byte, 0);
    }
}

#[test]
fn streamed_literal_pages_match_overlaps_and_restore_every_physical_encoding() {
    let text = "head λ😀λ😀λ tail\nλ😀λ";
    for (encoding, bom) in [
        (TextEncoding::Utf8, false),
        (TextEncoding::Utf8, true),
        (TextEncoding::Utf16Le, false),
        (TextEncoding::Utf16Be, false),
    ] {
        let bytes = encode(text, encoding, bom);
        let fixture = fixture(
            &bytes,
            encoding.as_str(),
            (0, u64::try_from(bytes.len()).unwrap()),
        );
        let backing = Backing::new(&fixture.generation, &bytes, 3);
        let streamed = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
        let ordinary = SourceService::new(&fixture.root, &fixture.generation).unwrap();
        for scan in [7, 15, 1024] {
            let budget = SourceFindBudget {
                matcher: LiteralSearchBudget {
                    max_scan_bytes: scan,
                    max_matches: 1,
                    ..SourceFindBudget::new().matcher
                },
                ..SourceFindBudget::new()
            };
            let mut cursor = None;
            for _ in 0..bytes.len() {
                let a = ordinary
                    .find_literal(
                        fixture.reference.span().file(),
                        "λ😀λ",
                        cursor,
                        budget,
                        &Cancellation::new(),
                    )
                    .unwrap();
                let b = streamed
                    .find_literal(
                        fixture.reference.span().file(),
                        "λ😀λ",
                        cursor,
                        budget,
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert_eq!(a.file, b.file);
                assert_eq!(a.matches, b.matches);
                assert_eq!(a.scanned, b.scanned);
                assert_eq!(a.stop, b.stop);
                assert_eq!(a.continuation, b.continuation);
                assert_eq!(a.restoration_bytes, b.restoration_bytes);
                cursor = b.continuation;
                if cursor.is_none() {
                    break;
                }
            }
            assert!(cursor.is_none());
        }
        let forged = SourceFindCursor {
            next_byte: usize::try_from(offset("head ", encoding, bom)).unwrap(),
            matched_prefix: 1,
        };
        assert!(matches!(
            streamed.find_literal(
                fixture.reference.span().file(),
                "λ😀λ",
                Some(forged),
                SourceFindBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceFindError::Literal(LiteralSearchError::InvalidCursor))
        ));
    }
}

#[test]
fn streamed_raw_bytes_preserve_unknown_and_malformed_text_without_false_text_success() {
    let bytes = [0xff, 0x00, 0x81, b'x', b'y'];
    for encoding in ["unknown", "utf-8"] {
        let fixture = fixture(&bytes, encoding, (1, 4));
        let backing = Backing::new(&fixture.generation, &bytes, 1);
        let streamed = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
        let result = streamed
            .read(
                std::slice::from_ref(&fixture.reference),
                raw_options(),
                SourceBudget::new(),
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(result.chunks[0].bytes, bytes[1..4]);
        assert!(matches!(
            streamed.read_lossless_text(
                std::slice::from_ref(&fixture.reference),
                LosslessTextReadOptions::new(),
                SourceBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceError::EncodingUnsupported)
        ));
        assert!(matches!(
            streamed.find_literal(
                fixture.reference.span().file(),
                "x",
                None,
                SourceFindBudget::new(),
                &Cancellation::new()
            ),
            Err(SourceFindError::Source(SourceError::EncodingUnsupported))
        ));
    }
}

#[test]
fn streamed_verification_and_provider_failures_never_release_a_selected_prefix() {
    let bytes = b"selected\nvalid tail";
    let fixture = fixture(bytes, "utf-8", (0, 4));
    for kind in 0..4 {
        let mut backing = Backing::new(&fixture.generation, bytes, 3);
        backing.offset_error = kind == 0;
        backing.length_error = kind == 1;
        backing.final_error = kind == 2;
        backing.cancel_after_callback = kind == 3;
        let streamed = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
        assert!(
            streamed
                .read(
                    std::slice::from_ref(&fixture.reference),
                    raw_options(),
                    SourceBudget::new(),
                    &Cancellation::new()
                )
                .is_err()
        );
        assert_eq!(
            backing.calls.get(),
            1,
            "provider failure must actually be exercised"
        );
    }
    let malformed = b"selected\ninvalid \xf0\x9f";
    let fixture = super::fixture(malformed, "utf-8", (0, 4));
    let backing = Backing::new(&fixture.generation, malformed, 3);
    let streamed = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
    assert!(matches!(
        streamed.read_lossless_text_prefix(
            std::slice::from_ref(&fixture.reference),
            1,
            SourceBudget::new(),
            &Cancellation::new()
        ),
        Err(SourceError::EncodingUnsupported)
    ));
}

#[test]
fn streamed_defaults_and_explicit_duration_provenance_remain_distinct() {
    let bytes = b"text";
    let fixture = fixture(bytes, "utf-8", (0, 4));
    let backing = Backing::new(&fixture.generation, bytes, 1);
    let service = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
    let cancellation = Cancellation::new();
    let budget = SourceBudget::new();
    assert_eq!(budget.max_verified_bytes, u64::MAX);
    assert_eq!(budget.max_snapshot_bytes, HARD_MAX_SNAPSHOT_BYTES);
    assert!(
        SourceControl::for_service(&service, &cancellation, budget)
            .deadline
            .is_none()
    );
    assert!(
        SourceControl::for_service(
            &service,
            &cancellation,
            budget.with_max_duration(budget.max_duration)
        )
        .deadline
        .is_some()
    );
    assert!(
        SourceBudget::new()
            .with_max_verification_duration(Duration::from_secs(60))
            .validate()
            .is_ok()
    );
    let short = budget.with_max_verification_duration(Duration::from_nanos(1));
    assert!(matches!(
        service.read(
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::new(),
            short,
            &cancellation
        ),
        Err(SourceError::Cancelled(CancellationReason::DeadlineExceeded))
    ));
    assert_eq!(backing.calls.get(), 0);
    let _ = cancellation.cancel(CancellationReason::ParentCancelled);
    assert!(matches!(
        service.read(
            std::slice::from_ref(&fixture.reference),
            SourceReadOptions::new(),
            budget,
            &cancellation
        ),
        Err(SourceError::Cancelled(CancellationReason::ParentCancelled))
    ));
    let ordinary = SourceService::new(&fixture.root, &fixture.generation).unwrap();
    assert!(
        SourceControl::for_service(&ordinary, &Cancellation::new(), budget)
            .deadline
            .is_some()
    );
}

#[test]
fn streamed_large_tiny_range_uses_independent_verification_and_memory_limits() {
    let mut bytes = vec![b'a'; usize::try_from(HARD_MAX_SNAPSHOT_BYTES).unwrap() + 1];
    let start = bytes.len() - 4;
    bytes[start..].copy_from_slice(b"tail");
    let fixture = fixture(
        &bytes,
        "utf-8",
        (
            u64::try_from(start).unwrap(),
            u64::try_from(bytes.len()).unwrap(),
        ),
    );
    let backing = Backing::new(&fixture.generation, &bytes, 65536);
    let streamed = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
    let options = raw_options();
    let result = streamed
        .read(
            std::slice::from_ref(&fixture.reference),
            options,
            SourceBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(result.chunks[0].bytes, b"tail");
    assert_eq!(result.chunks[0].start_byte, u64::try_from(start).unwrap());
    let text = streamed
        .read_lossless_text(
            std::slice::from_ref(&fixture.reference),
            LosslessTextReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            SourceBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(text.chunks()[0].text(), "tail");
    let found = streamed
        .find_literal(
            fixture.reference.span().file(),
            "tail",
            Some(SourceFindCursor {
                next_byte: start,
                matched_prefix: 0,
            }),
            SourceFindBudget::new(),
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(found.matches, vec![fixture.reference.clone()]);
    assert_eq!(found.stop, SourceFindStop::EndOfSource);
    let before = backing.calls.get();
    let explicit = SourceBudget::new().with_max_verified_bytes(HARD_MAX_SNAPSHOT_BYTES);
    assert_eq!(
        streamed
            .read(
                std::slice::from_ref(&fixture.reference),
                options,
                explicit,
                &Cancellation::new()
            )
            .unwrap_err(),
        SourceError::VerificationBudgetExceeded
    );
    assert_eq!(backing.calls.get(), before);
    let ordinary = SourceService::new(&fixture.root, &fixture.generation).unwrap();
    assert_eq!(
        ordinary
            .read(
                std::slice::from_ref(&fixture.reference),
                options,
                SourceBudget::new(),
                &Cancellation::new()
            )
            .unwrap_err(),
        SourceError::SnapshotBudgetExceeded
    );
}

#[test]
fn a_provider_cannot_erase_a_rejected_callback_by_retrying_the_same_bytes() {
    let bytes = b"selected";
    let fixture = fixture(bytes, "utf-8", (0, 4));
    let mut backing = Backing::new(&fixture.generation, bytes, 3);
    backing.retry_rejected_callback = true;
    let service = SourceService::from_verified_backing(&backing, &fixture.generation).unwrap();
    assert_eq!(
        service
            .read(
                std::slice::from_ref(&fixture.reference),
                raw_options(),
                SourceBudget::new(),
                &Cancellation::new()
            )
            .unwrap_err(),
        SourceError::StaleSource
    );
    assert_eq!(backing.calls.get(), 1);
}
