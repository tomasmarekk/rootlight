//! Authored byte fixtures verify declared encodings and exact scalar provenance.
//! The complete single-byte alphabet catches accidental Latin-1 substitution.

use super::*;
use rootlight_cancel::CancellationReason;

fn decode(bytes: &[u8], page: u16) -> Result<DecodedText<'_>, TextError> {
    DecodedText::decode(bytes, page, TextLimits::default(), &Cancellation::new())
}

#[test]
fn windows_alphabet_is_exact_and_undefined_slots_fail_closed() {
    let exceptions = [
        Some('€'),
        None,
        Some('‚'),
        Some('ƒ'),
        Some('„'),
        Some('…'),
        Some('†'),
        Some('‡'),
        Some('ˆ'),
        Some('‰'),
        Some('Š'),
        Some('‹'),
        Some('Œ'),
        None,
        Some('Ž'),
        None,
        None,
        Some('‘'),
        Some('’'),
        Some('“'),
        Some('”'),
        Some('•'),
        Some('–'),
        Some('—'),
        Some('˜'),
        Some('™'),
        Some('š'),
        Some('›'),
        Some('œ'),
        None,
        Some('ž'),
        Some('Ÿ'),
    ];
    for byte in 0..=u8::MAX {
        let input = [byte];
        let expected = if (0x80..=0x9f).contains(&byte) {
            exceptions[usize::from(byte - 0x80)]
        } else {
            Some(char::from(byte))
        };
        match expected {
            Some(character) => {
                let view = decode(&input, 1252).unwrap();
                assert_eq!(view.text(), character.to_string());
                assert_eq!(view.original_bytes(), input);
                assert_eq!(view.original_range(0..view.text().len()), Some(0..1));
            }
            None => assert!(matches!(
                decode(&input, 1252),
                Err(TextError::Invalid {
                    code_page: 1252,
                    offset: 0
                })
            )),
        }
    }
    assert!(matches!(
        decode(b"prefix\x81", 1252),
        Err(TextError::Invalid { offset: 6, .. })
    ));
}

#[test]
fn sparse_mapping_preserves_every_boundary_without_rounding() {
    for length in [1, 255, 256, 257, 511, 512, 513, 1025] {
        let input: Vec<_> = [b'a', 0xe9, 0x80, b'\r', b'\n', 0]
            .into_iter()
            .cycle()
            .take(length)
            .collect();
        let view = decode(&input, 1252).unwrap();
        let boundaries: Vec<_> = view
            .text()
            .char_indices()
            .map(|(at, _)| at)
            .chain([view.text().len()])
            .collect();
        assert_eq!(boundaries.len(), input.len() + 1);
        for (original, pair) in boundaries.windows(2).enumerate() {
            assert_eq!(
                view.original_range(pair[0]..pair[1]),
                Some(original..original + 1)
            );
            assert_eq!(
                view.original_range(pair[0]..pair[0]),
                Some(original..original)
            );
            for split in pair[0] + 1..pair[1] {
                assert_eq!(view.original_range(pair[0]..split), None);
                assert_eq!(view.original_range(split..pair[1]), None);
            }
        }
        assert_eq!(
            view.original_range(view.text().len()..view.text().len()),
            Some(length..length)
        );
        assert_eq!(view.original_range(0..view.text().len()), Some(0..length));
        assert_eq!(view.original_range(Range { start: 1, end: 0 }), None);
        assert_eq!(view.original_range(0..view.text().len() + 1), None);
        assert_eq!(view.original_range(usize::MAX..usize::MAX), None);
        assert_eq!(
            view.mapping_points(),
            if length == 1 { 0 } else { length.div_ceil(256) }
        );
    }
}

#[test]
fn declared_utf8_and_ascii_borrow_without_normalization() {
    for (input, page) in [
        ("\u{feff}é e\u{301} Δ 🦀\r\n\0".as_bytes(), 65001),
        (b"Sub Run()\r\n\0".as_slice(), 1252),
        (b"".as_slice(), 1252),
        (b"".as_slice(), 65001),
    ] {
        let view = decode(input, page).unwrap();
        assert!(matches!(view.text, Cow::Borrowed(_)));
        assert_eq!(view.text().as_ptr(), input.as_ptr());
        assert_eq!(view.text().as_bytes(), input);
        assert_eq!(view.code_page(), page);
        assert_eq!(view.mapping_points(), 0);
        for position in 0..=input.len() {
            let expected = view
                .text()
                .is_char_boundary(position)
                .then_some(position..position);
            assert_eq!(view.original_range(position..position), expected);
        }
    }
    assert_eq!(decode(b"\xef\xbb\xbf", 1252).unwrap().text(), "ï»¿");
}

#[test]
fn unknown_pages_and_invalid_utf8_never_use_a_fallback() {
    for page in [0, 932, 1250, 1200, u16::MAX] {
        for input in [b"".as_slice(), b"ASCII", b"\x80"] {
            assert!(
                matches!(decode(input, page), Err(TextError::Unsupported { code_page }) if code_page == page)
            );
        }
    }
    for tail in [
        b"\x80".as_slice(),
        b"\xc0\xaf",
        b"\xed\xa0\x80",
        b"\xf4\x90\x80\x80",
        b"\xe2\x82",
    ] {
        let mut input = b"ok".to_vec();
        input.extend_from_slice(tail);
        assert!(matches!(
            decode(&input, 65001),
            Err(TextError::Invalid {
                code_page: 65001,
                offset: 2
            })
        ));
    }
    assert_ne!(
        decode(b"\xc3\xa9", 1252).unwrap().text(),
        decode(b"\xc3\xa9", 65001).unwrap().text()
    );
}

#[test]
fn input_output_mapping_limits_and_cancellation_are_independent() {
    let cancel = Cancellation::new();
    let defaults = TextLimits::default();
    let input = [0x80; 257];
    for (limits, resource) in [
        (
            TextLimits {
                input_bytes: 256,
                ..defaults
            },
            "input bytes",
        ),
        (
            TextLimits {
                output_bytes: 770,
                ..defaults
            },
            "output bytes",
        ),
        (
            TextLimits {
                mapping_points: 1,
                ..defaults
            },
            "mapping points",
        ),
    ] {
        assert!(
            matches!(DecodedText::decode(&input, 1252, limits, &cancel), Err(TextError::Limit { resource: actual, .. }) if actual == resource)
        );
    }
    let exact = TextLimits {
        input_bytes: 257,
        output_bytes: 771,
        mapping_points: 2,
    };
    assert_eq!(
        DecodedText::decode(&input, 1252, exact, &cancel)
            .unwrap()
            .text()
            .len(),
        771
    );
    let zero = TextLimits {
        input_bytes: 0,
        output_bytes: 0,
        mapping_points: 0,
    };
    assert!(DecodedText::decode(b"", 1252, zero, &cancel).is_ok());
    assert!(
        DecodedText::decode(
            b"plain",
            1252,
            TextLimits {
                mapping_points: 0,
                ..defaults
            },
            &cancel
        )
        .is_ok()
    );
    for limits in [
        TextLimits {
            input_bytes: 64 * 1024 * 1024 + 1,
            ..defaults
        },
        TextLimits {
            output_bytes: 64 * 1024 * 1024 + 1,
            ..defaults
        },
        TextLimits {
            mapping_points: MAX_POINTS + 1,
            ..defaults
        },
    ] {
        assert!(matches!(
            DecodedText::decode(b"", 1252, limits, &cancel),
            Err(TextError::Limits)
        ));
    }
    cancel.cancel(CancellationReason::ClientRequest);
    for page in [1252, 65001, 0] {
        assert!(matches!(
            DecodedText::decode(&input, page, defaults, &cancel),
            Err(TextError::Cancelled(_))
        ));
    }
}

#[test]
fn debug_and_errors_do_not_disclose_source_content() {
    let view = decode(b"PrivatePayload\x80", 1252).unwrap();
    assert!(!format!("{view:?}").contains("PrivatePayload"));
    let error = decode(b"PrivatePayload\x81", 1252).unwrap_err();
    assert!(!format!("{error:?} {error}").contains("PrivatePayload"));
}
