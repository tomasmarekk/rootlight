//! Finite generic projection controls; none selects or certifies a language.
//! Explicit boundary tables keep source identity and pair interiors observable.

use std::cell::Cell;

use rootlight_cancel::CancellationReason;
use rootlight_ids::FileId;

use super::*;

fn view<'a>(raw: &'a [u8], encoding: &str) -> NativeSourceView<'a> {
    NativeSourceView::new_with_policy(
        raw,
        encoding,
        LineEndingPolicy::CrlfFileText,
        usize::MAX,
        usize::MAX,
        &Cancellation::new(),
    )
    .unwrap()
}

fn utf16(text: &str, encoding: SourceTextEncoding) -> Vec<u8> {
    text.encode_utf16()
        .flat_map(|unit| match encoding {
            SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
            SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
        })
        .collect()
}

fn exact_boundaries(view: &NativeSourceView<'_>, pairs: &[(usize, usize)]) {
    for physical in 0..=view.physical_bytes() + 1 {
        match pairs.iter().find(|(raw, _)| *raw == physical) {
            Some((_, projected)) => {
                assert_eq!(view.to_projected_offset(physical).unwrap(), *projected)
            }
            None => assert!(
                view.to_projected_offset(physical).is_err(),
                "physical {physical}"
            ),
        }
    }
    for projected in 0..=view.as_bytes().len() + 1 {
        match pairs.iter().find(|(_, text)| *text == projected) {
            Some((physical, _)) => {
                assert_eq!(view.to_physical_offset(projected).unwrap(), *physical)
            }
            None => assert!(
                view.to_physical_offset(projected).is_err(),
                "projected {projected}"
            ),
        }
    }
    let file = FileId::from_bytes([41; 20]);
    for (start, (physical_start, projected_start)) in pairs.iter().enumerate() {
        for (physical_end, projected_end) in pairs.iter().skip(start) {
            let raw = SourceSpan::new(
                file,
                u64::try_from(*physical_start).unwrap(),
                u64::try_from(*physical_end).unwrap(),
            )
            .unwrap();
            let text = SourceSpan::new(
                file,
                u64::try_from(*projected_start).unwrap(),
                u64::try_from(*projected_end).unwrap(),
            )
            .unwrap();
            assert_eq!(view.projected_span(raw).unwrap(), text);
            assert_eq!(view.physical_span(text).unwrap(), raw);
        }
    }
}

#[test]
fn file_text_preserve_keeps_default_bytes_boundaries_and_capacity() {
    let text = "\u{feff}a\r\né\r中🧭\n";
    let le = utf16(text, SourceTextEncoding::Utf16Le);
    let be = utf16(text, SourceTextEncoding::Utf16Be);
    for (raw, encoding) in [
        (text.as_bytes(), "utf-8"),
        (le.as_slice(), "utf-16le"),
        (be.as_slice(), "utf-16be"),
    ] {
        let default =
            NativeSourceView::new(raw, encoding, usize::MAX, usize::MAX, &Cancellation::new())
                .unwrap();
        let explicit = NativeSourceView::new_with_policy(
            raw,
            encoding,
            LineEndingPolicy::Preserve,
            usize::MAX,
            usize::MAX,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(default.as_str(), text);
        assert_eq!(explicit.as_bytes(), default.as_bytes());
        assert_eq!(explicit.owned_bytes(), default.owned_bytes());
        assert_eq!(explicit.is_identity(), default.is_identity());
        assert_eq!(explicit.line_ending_policy(), LineEndingPolicy::Preserve);
        for offset in 0..=raw.len() + 1 {
            assert_eq!(
                default.to_projected_offset(offset).ok(),
                explicit.to_projected_offset(offset).ok()
            );
        }
        for offset in 0..=text.len() + 1 {
            assert_eq!(
                default.to_physical_offset(offset).ok(),
                explicit.to_physical_offset(offset).ok()
            );
        }
    }
    assert_eq!(size_of::<ScalarWidthRun>(), 4 * size_of::<usize>());
}

#[test]
fn file_text_utf8_maps_pairs_unicode_lone_cr_and_every_span_exactly() {
    let raw = "\u{feff}a\r\né\rb\n🧭\r\n";
    let view = view(raw.as_bytes(), "utf-8");
    assert_eq!(view.as_str(), "\u{feff}a\né\rb\n🧭\n");
    assert_eq!(view.encoding(), None);
    assert!(!view.is_identity());
    assert_eq!(view.line_ending_policy(), LineEndingPolicy::CrlfFileText);
    exact_boundaries(
        &view,
        &[
            (0, 0),
            (3, 3),
            (4, 4),
            (6, 5),
            (8, 7),
            (9, 8),
            (10, 9),
            (11, 10),
            (15, 14),
            (17, 15),
        ],
    );
}

#[test]
fn file_text_utf16_boms_surrogates_and_pair_interiors_map_exactly() {
    let text = "\u{feff}a\r\né\r🧭\n";
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let raw = utf16(text, encoding);
        let view = view(&raw, encoding.as_str());
        assert_eq!(view.as_str(), "\u{feff}a\né\r🧭\n");
        assert_eq!(view.encoding(), Some(encoding));
        assert!(!view.is_identity());
        exact_boundaries(
            &view,
            &[
                (0, 0),
                (2, 3),
                (4, 4),
                (8, 5),
                (10, 7),
                (12, 8),
                (16, 12),
                (18, 13),
            ],
        );
    }
}

#[test]
fn file_text_no_pair_utf8_borrows_with_zero_owned_capacity_but_keeps_policy() {
    for raw in [b"".as_slice(), b"a\rb\nc\0", "\u{feff}é中🧭".as_bytes()] {
        let view = NativeSourceView::new_with_policy(
            raw,
            "utf-8",
            LineEndingPolicy::CrlfFileText,
            raw.len(),
            0,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(view.as_bytes(), raw);
        assert_eq!(view.as_bytes().as_ptr(), raw.as_ptr());
        assert_eq!(view.owned_bytes(), 0);
        assert!(view.runs.is_empty() && view.is_identity());
        assert_eq!(view.line_ending_policy(), LineEndingPolicy::CrlfFileText);
    }
}

#[test]
fn file_text_planned_and_returned_capacities_include_both_widths() {
    let raw = b"a\r\nb\n";
    let full = view(raw, "utf-8");
    assert_eq!(full.runs.len(), 3);
    assert_eq!(
        full.runs
            .iter()
            .map(|run| (
                run.physical_width(),
                run.projected_width(),
                run.scalar_count
            ))
            .collect::<Vec<_>>(),
        vec![(1, 1, 1), (2, 1, 1), (1, 1, 2)]
    );
    let text_capacity = match &full.text {
        Cow::Owned(text) => text.capacity(),
        Cow::Borrowed(_) => 0,
    };
    assert_eq!(
        full.owned_bytes(),
        text_capacity + full.runs.capacity() * size_of::<ScalarWidthRun>()
    );
    let charge = full.owned_bytes();
    assert!(matches!(
        NativeSourceView::new_with_policy(
            raw,
            "utf-8",
            LineEndingPolicy::CrlfFileText,
            4,
            charge - 1,
            &Cancellation::new()
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::ReportedMemoryBytes,
            ..
        }))
    ));
    assert_eq!(
        NativeSourceView::new_with_policy(
            raw,
            "utf-8",
            LineEndingPolicy::CrlfFileText,
            4,
            charge,
            &Cancellation::new()
        )
        .unwrap()
        .owned_bytes(),
        charge
    );
    assert!(matches!(
        NativeSourceView::new_with_policy(
            raw,
            "utf-8",
            LineEndingPolicy::CrlfFileText,
            3,
            usize::MAX,
            &Cancellation::new()
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::SourceBytes,
            ..
        }))
    ));
    assert!(projection_owned_bytes(usize::MAX, 1).is_err());
    assert!(projection_owned_bytes(0, usize::MAX).is_err());
}

#[test]
fn file_text_cancellation_is_checked_in_both_projection_passes() {
    let text = "a\r\n".repeat(700);
    for cancel_pass in [0, 1] {
        let cancellation = Cancellation::new();
        let pass = Cell::new(0);
        let consumed = Cell::new(0usize);
        let consumed_ref = &consumed;
        let cancellation_ref = &cancellation;
        let result = build(
            text.as_bytes(),
            Some(&text),
            None,
            || {
                let current = pass.get();
                pass.set(current + 1);
                text.chars().map(Ok).inspect(move |_| {
                    if current == cancel_pass {
                        consumed_ref.set(consumed_ref.get() + 1);
                        if consumed_ref.get() == 300 {
                            cancellation_ref.cancel(CancellationReason::ClientRequest);
                        }
                    }
                })
            },
            usize::MAX,
            usize::MAX,
            &cancellation,
        );
        assert!(matches!(
            result,
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(
            consumed.get() >= 300 && consumed.get() <= 300 + 2 * CANCELLATION_SCALAR_INTERVAL + 1
        );
        assert_eq!(pass.get(), cancel_pass + 1);
    }
}

#[test]
fn file_text_prefix_never_promotes_a_cut_pair_or_changes_lone_cr_eof() {
    let cancellation = Cancellation::new();
    let view = NativeSourceView::bounded_prefix_with_policy(
        b"a\r\nb",
        "utf-8",
        LineEndingPolicy::CrlfFileText,
        2,
        usize::MAX,
        &cancellation,
    )
    .unwrap();
    assert_eq!(view.as_bytes(), b"a");
    assert_eq!(view.physical_bytes(), 1);
    let lone = NativeSourceView::bounded_prefix_with_policy(
        b"a\r",
        "utf-8",
        LineEndingPolicy::CrlfFileText,
        2,
        0,
        &cancellation,
    )
    .unwrap();
    assert_eq!(lone.as_bytes(), b"a\r");
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        let raw = utf16("\u{feff}a\r\nb", encoding);
        let prefix = NativeSourceView::bounded_prefix_with_policy(
            &raw,
            encoding.as_str(),
            LineEndingPolicy::CrlfFileText,
            6,
            usize::MAX,
            &cancellation,
        )
        .unwrap();
        assert_eq!(prefix.as_str(), "\u{feff}a");
        assert_eq!(prefix.physical_bytes(), 4);
        assert!(prefix.physical_bytes() < raw.len());
    }
}

#[test]
fn file_text_invalid_eof_bom_encoding_and_cancellation_remain_strict() {
    for bytes in [b"\xe2".as_slice(), b"a\r\n\xff"] {
        assert!(
            NativeSourceView::new_with_policy(
                bytes,
                "utf-8",
                LineEndingPolicy::CrlfFileText,
                usize::MAX,
                usize::MAX,
                &Cancellation::new()
            )
            .is_err()
        );
    }
    for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
        for text in ["a\r\n", "\u{feff}a\0\r\n"] {
            let raw = utf16(text, encoding);
            assert!(
                NativeSourceView::new_with_policy(
                    &raw,
                    encoding.as_str(),
                    LineEndingPolicy::CrlfFileText,
                    usize::MAX,
                    usize::MAX,
                    &Cancellation::new()
                )
                .is_err()
            );
        }
        let mut raw = utf16("\u{feff}a\r\n", encoding);
        raw.push(0);
        assert!(
            NativeSourceView::bounded_prefix_with_policy(
                &raw,
                encoding.as_str(),
                LineEndingPolicy::CrlfFileText,
                raw.len(),
                usize::MAX,
                &Cancellation::new()
            )
            .is_err()
        );
    }
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        NativeSourceView::new_with_policy(
            b"a\r\n",
            "utf-8",
            LineEndingPolicy::CrlfFileText,
            usize::MAX,
            usize::MAX,
            &cancelled
        ),
        Err(AdapterError::Cancelled { .. })
    ));
    assert!(matches!(
        NativeSourceView::new_with_policy(
            b"a",
            "utf-32le",
            LineEndingPolicy::CrlfFileText,
            usize::MAX,
            usize::MAX,
            &Cancellation::new()
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::UnsupportedEncoding
        ))
    ));
}
