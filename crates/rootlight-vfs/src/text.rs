//! Borrowed Unicode views over immutable original source bytes.
//!
//! Validation never replaces source storage. UTF-16 is admitted only with a BOM
//! and strict whole-input decoding; consumers retain their own bounded projections.

use std::{char::DecodeUtf16, fmt, iter, slice, str};

use rootlight_cancel::{Cancellation, Cancelled};

/// Byte encoding verified for a complete borrowed source view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextEncoding {
    /// UTF-8, without normalization or removal of a leading BOM.
    Utf8,
    /// BOM-backed UTF-16 with little-endian code units.
    Utf16Le,
    /// BOM-backed UTF-16 with big-endian code units.
    Utf16Be,
}

impl TextEncoding {
    /// Returns the encoding label stored with original file metadata.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }

    /// Returns the original encoded byte width of one decoded scalar value.
    #[must_use]
    pub const fn character_bytes(self, character: char) -> usize {
        match self {
            Self::Utf8 => character.len_utf8(),
            Self::Utf16Le | Self::Utf16Be => character.len_utf16() * 2,
        }
    }
}

#[derive(Clone, Copy)]
enum TextBody<'a> {
    Utf8(&'a str),
    Utf16(&'a [[u8; 2]]),
}

/// Validated, allocation-free text view over original source bytes.
///
/// UTF-16 iteration omits the encoding BOM. UTF-8 iteration preserves every
/// scalar, including a BOM. Debug output excludes repository-controlled content.
/// Character positions are not source offsets; consumers must account for the
/// original encoding's byte widths and any omitted BOM when deriving spans.
#[derive(Clone, Copy)]
pub struct SourceText<'a> {
    body: TextBody<'a>,
    encoding: TextEncoding,
}

impl<'a> SourceText<'a> {
    /// Borrows an already validated UTF-8 string without scanning or copying it.
    #[must_use]
    pub const fn from_utf8(text: &'a str) -> Self {
        Self {
            body: TextBody::Utf8(text),
            encoding: TextEncoding::Utf8,
        }
    }

    /// Validates UTF-8 or BOM-backed UTF-16 across the entire immutable input.
    ///
    /// Returns `None` for unknown encodings, malformed UTF-16, or decoded UTF-16
    /// NUL. The latter prevents UTF-32LE's shared BOM prefix from being admitted
    /// as UTF-16. UTF-8 binary classification remains the discovery caller's job.
    /// No source-sized allocation is performed.
    ///
    /// # Errors
    /// Returns [`Cancelled`] when cancellation or its deadline wins.
    pub fn parse(bytes: &'a [u8], cancellation: &Cancellation) -> Result<Option<Self>, Cancelled> {
        Self::parse_with_check(bytes, || cancellation.check())
    }

    /// Validates a source view while enforcing the caller's work controls.
    ///
    /// Uses the same whole-input rules as [`Self::parse`]. Calls `check` before
    /// and after UTF-8 validation and at most every 1024 UTF-16 scalars, allowing
    /// query-specific budgets without changing a shared cancellation token.
    ///
    /// # Errors
    /// Propagates the first error returned by `check` without exposing a partial view.
    pub fn parse_with_check<E>(
        bytes: &'a [u8],
        mut check: impl FnMut() -> Result<(), E>,
    ) -> Result<Option<Self>, E> {
        check()?;
        let utf8 = str::from_utf8(bytes);
        check()?;
        if let Ok(text) = utf8 {
            return Ok(Some(Self::from_utf8(text)));
        }
        let (encoding, body) = match bytes {
            [0xff, 0xfe, body @ ..] => (TextEncoding::Utf16Le, body),
            [0xfe, 0xff, body @ ..] => (TextEncoding::Utf16Be, body),
            _ => return Ok(None),
        };
        let (units, remainder) = body.as_chunks::<2>();
        if !remainder.is_empty() {
            return Ok(None);
        }
        for (index, decoded) in decode_units(units, encoding).enumerate() {
            if index % 1024 == 0 {
                check()?;
            }
            if !matches!(decoded, Ok(character) if character != '\0') {
                check()?;
                return Ok(None);
            }
        }
        check()?;
        Ok(Some(Self {
            body: TextBody::Utf16(units),
            encoding,
        }))
    }

    /// Returns the verified original byte encoding.
    #[must_use]
    pub const fn encoding(self) -> TextEncoding {
        self.encoding
    }

    /// Returns the original byte length, including an encoding BOM when present.
    #[must_use]
    pub const fn byte_len(self) -> usize {
        match self.body {
            TextBody::Utf8(text) => text.len(),
            TextBody::Utf16(units) => units.len() * 2 + 2,
        }
    }

    /// Iterates from an original byte offset without decoding the preceding text.
    ///
    /// Returns `None` for offsets beyond the source or inside a scalar. UTF-16
    /// offset zero aliases the first scalar after the two-byte BOM; offset one,
    /// odd offsets, and the low half of a surrogate pair are rejected. UTF-8
    /// retains its BOM as a scalar, consistently with [`Self::chars`].
    pub fn chars_from_byte(self, offset: usize) -> Option<impl Iterator<Item = char> + 'a> {
        match self.body {
            TextBody::Utf8(text) => Some(SourceChars::Utf8(text.get(offset..)?.chars())),
            TextBody::Utf16(units) => {
                let offset = if offset == 0 { 2 } else { offset };
                if offset < 2 || !offset.is_multiple_of(2) {
                    return None;
                }
                let suffix = units.get((offset - 2) / 2..)?;
                if let Some(first) = suffix.first() {
                    let unit = match self.encoding {
                        TextEncoding::Utf16Be => u16::from_be_bytes(*first),
                        TextEncoding::Utf8 | TextEncoding::Utf16Le => u16::from_le_bytes(*first),
                    };
                    if (0xdc00..=0xdfff).contains(&unit) {
                        return None;
                    }
                }
                Some(SourceChars::Utf16(decode_units(suffix, self.encoding)))
            }
        }
    }

    /// Borrows the original text without copying when its encoding is UTF-8.
    #[must_use]
    pub const fn utf8(self) -> Option<&'a str> {
        match self.body {
            TextBody::Utf8(text) => Some(text),
            TextBody::Utf16(_) => None,
        }
    }

    /// Iterates Unicode scalars without allocating a decoded source buffer.
    ///
    /// Callers scanning large inputs must check their cancellation/work budgets
    /// during iteration. This view has already validated the immutable input.
    pub fn chars(self) -> impl Iterator<Item = char> + 'a {
        match self.body {
            TextBody::Utf8(text) => SourceChars::Utf8(text.chars()),
            TextBody::Utf16(units) => SourceChars::Utf16(decode_units(units, self.encoding)),
        }
    }
}

impl fmt::Debug for SourceText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceText")
            .field("encoding", &self.encoding)
            .finish_non_exhaustive()
    }
}

type UnitDecoder<'a> =
    DecodeUtf16<iter::Map<iter::Copied<slice::Iter<'a, [u8; 2]>>, fn([u8; 2]) -> u16>>;

fn decode_units(units: &[[u8; 2]], encoding: TextEncoding) -> UnitDecoder<'_> {
    let decode: fn([u8; 2]) -> u16 = match encoding {
        TextEncoding::Utf16Be => u16::from_be_bytes,
        TextEncoding::Utf8 | TextEncoding::Utf16Le => u16::from_le_bytes,
    };
    char::decode_utf16(units.iter().copied().map(decode))
}

enum SourceChars<'a> {
    Utf8(str::Chars<'a>),
    Utf16(UnitDecoder<'a>),
}

impl Iterator for SourceChars<'_> {
    type Item = char;
    fn next(&mut self) -> Option<char> {
        match self {
            Self::Utf8(chars) => chars.next(),
            Self::Utf16(chars) => chars.next().map(|decoded| {
                decoded.expect("immutable UTF-16 units were validated at construction")
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_suffixes_accept_only_original_scalar_boundaries() {
        let text = SourceText::from_utf8("\u{feff}aλ😀z");
        assert_eq!(text.byte_len(), 11);
        for (offset, suffix) in [
            (0, "\u{feff}aλ😀z"),
            (3, "aλ😀z"),
            (4, "λ😀z"),
            (6, "😀z"),
            (10, "z"),
            (11, ""),
        ] {
            assert_eq!(
                text.chars_from_byte(offset).unwrap().collect::<String>(),
                suffix
            );
        }
        for offset in [1, 2, 5, 7, 8, 9, 12, usize::MAX] {
            assert!(text.chars_from_byte(offset).is_none());
        }
        for big_endian in [false, true] {
            let bytes: Vec<_> = iter::once(0xfeff)
                .chain("aλ😀z".encode_utf16())
                .flat_map(|unit| {
                    if big_endian {
                        unit.to_be_bytes()
                    } else {
                        unit.to_le_bytes()
                    }
                })
                .collect();
            let text = SourceText::parse(&bytes, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(text.byte_len(), 12);
            for (offset, suffix) in [
                (0, "aλ😀z"),
                (2, "aλ😀z"),
                (4, "λ😀z"),
                (6, "😀z"),
                (10, "z"),
                (12, ""),
            ] {
                assert_eq!(
                    text.chars_from_byte(offset).unwrap().collect::<String>(),
                    suffix
                );
            }
            for offset in [1, 3, 5, 7, 8, 9, 11, 13, usize::MAX] {
                assert!(text.chars_from_byte(offset).is_none());
            }
        }
    }

    #[test]
    fn unicode_views_preserve_original_byte_widths_without_debug_content() {
        let source = "private_source_λ𝄞\r\n";
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ] {
            let bytes = match encoding {
                TextEncoding::Utf8 => source.as_bytes().to_vec(),
                TextEncoding::Utf16Le => iter::once(0xfeff_u16)
                    .chain(source.encode_utf16())
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                TextEncoding::Utf16Be => iter::once(0xfeff_u16)
                    .chain(source.encode_utf16())
                    .flat_map(u16::to_be_bytes)
                    .collect(),
            };
            let text = SourceText::parse(&bytes, &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(text.encoding(), encoding);
            assert_eq!(text.chars().collect::<String>(), source);
            let bom = if encoding == TextEncoding::Utf8 { 0 } else { 2 };
            assert_eq!(
                text.chars()
                    .map(|value| encoding.character_bytes(value))
                    .sum::<usize>()
                    + bom,
                bytes.len()
            );
            assert!(!format!("{text:?}").contains("private_source"));
            assert_eq!(
                text.utf8(),
                (encoding == TextEncoding::Utf8).then_some(source)
            );
        }
    }

    #[test]
    fn utf16_validation_honors_caller_checks_before_an_invalid_tail() {
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(iter::repeat_n([b'a', 0], 4096).flatten());
        bytes.extend([0, 0xd8]);
        let mut checkpoints = 0;
        let result = SourceText::parse_with_check(&bytes, || {
            checkpoints += 1;
            if checkpoints == 4 { Err(()) } else { Ok(()) }
        });
        assert!(matches!(result, Err(())));
        assert_eq!(checkpoints, 4);
        assert!(
            SourceText::parse(&bytes, &Cancellation::new())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn invalid_tail_and_nul_never_produce_a_partial_text_view() {
        for suffix in [vec![0], vec![0, 0xd8], vec![0, 0], vec![0, 0xdc]] {
            let mut bytes = vec![0xff, 0xfe];
            bytes.extend(iter::repeat_n([b'a', 0], 10_000).flatten());
            bytes.extend(suffix);
            assert!(
                SourceText::parse(&bytes, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
        assert!(
            SourceText::parse(b"\xff\xfe\0\0a\0\0\0", &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(
            SourceText::parse(b"\xfe\xff\xd8\0", &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(
            SourceText::parse(b"\xff", &Cancellation::new())
                .unwrap()
                .is_none()
        );
        let cancelled = Cancellation::with_deadline(std::time::Instant::now());
        assert!(SourceText::parse(b"source", &cancelled).is_err());
    }
}
