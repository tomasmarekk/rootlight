//! Strict Unicode validation without retaining or normalizing source bytes.
//! Immutable byte ownership and source identity remain caller responsibilities.

use std::fmt;

use rootlight_cancel::{Cancellation, Cancelled};

use crate::TextEncoding;

const CHECK_BYTES: usize = 4 * 1024;

/// Incrementally validates UTF-8 or BOM-backed UTF-16 using constant memory.
///
/// Chunks must contain successive original bytes in order. Only [`Self::finish`]
/// after EOF establishes complete validation. No bytes are retained beyond one
/// incomplete scalar or encoding prefix, and no source identity, language or
/// filesystem stability is established. Rules match [`crate::SourceText::parse`],
/// including acceptance of UTF-8 NUL and rejection of decoded UTF-16 NUL.
pub struct StreamingSourceTextValidator {
    state: State,
    contains_nul: bool,
}

enum State {
    Undecided,
    BomStart(u8),
    Utf8(Utf8State),
    Utf16(Utf16State),
    Invalid,
}

#[derive(Default)]
struct Utf8State {
    carry: [u8; 4],
    length: usize,
}

struct Utf16State {
    little_endian: bool,
    odd_byte: Option<u8>,
    high_surrogate: bool,
}

impl StreamingSourceTextValidator {
    /// Creates an empty validator; EOF without any chunks is valid UTF-8.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: State::Undecided,
            contains_nul: false,
        }
    }

    /// Validates the next ordered chunk without retaining its complete contents.
    ///
    /// Cancellation is checked at entry, at least every 4096 input bytes, and
    /// before success. Invalid text remains invalid even if later chunks are
    /// valid. Cancellation also makes the validator permanently invalid, so a
    /// caller cannot ignore an error and obtain a complete-text result.
    ///
    /// # Errors
    /// Returns [`Cancelled`] when cancellation or its deadline is observed.
    pub fn push(&mut self, bytes: &[u8], cancellation: &Cancellation) -> Result<(), Cancelled> {
        self.push_with_check(bytes, || cancellation.check())
    }

    /// Reports whether a NUL scalar has been observed in the processed input.
    ///
    /// UTF-8 NUL is valid text here; binary classification remains caller-owned.
    /// UTF-16 NUL invalidates the stream. This observation is not proof that an
    /// unfinished or invalid stream contains no NUL elsewhere.
    #[must_use]
    pub const fn contains_nul(&self) -> bool {
        self.contains_nul
    }

    /// Consumes the validator at EOF and returns the complete input's encoding.
    ///
    /// Returns `None` for malformed text, incomplete prefixes, scalars, UTF-16
    /// code units or surrogate pairs, or any earlier cancelled push. UTF-8 BOM
    /// bytes remain part of the original input; no normalization is performed.
    #[must_use]
    pub fn finish(self) -> Option<TextEncoding> {
        match self.state {
            State::Undecided => Some(TextEncoding::Utf8),
            State::Utf8(state) if state.length == 0 => Some(TextEncoding::Utf8),
            State::Utf16(state) if state.odd_byte.is_none() && !state.high_surrogate => {
                Some(if state.little_endian {
                    TextEncoding::Utf16Le
                } else {
                    TextEncoding::Utf16Be
                })
            }
            State::BomStart(_) | State::Utf8(_) | State::Utf16(_) | State::Invalid => None,
        }
    }

    fn push_with_check(
        &mut self,
        bytes: &[u8],
        mut check: impl FnMut() -> Result<(), Cancelled>,
    ) -> Result<(), Cancelled> {
        let result = (|| {
            check()?;
            for block in bytes.chunks(CHECK_BYTES) {
                self.push_block(block);
                check()?;
            }
            check()
        })();
        if result.is_err() {
            self.state = State::Invalid;
        }
        result
    }

    fn push_block(&mut self, mut bytes: &[u8]) {
        loop {
            match &mut self.state {
                State::Undecided => {
                    let Some((&first, rest)) = bytes.split_first() else {
                        return;
                    };
                    if matches!(first, 0xff | 0xfe) {
                        self.state = State::BomStart(first);
                        bytes = rest;
                    } else {
                        self.state = State::Utf8(Utf8State::default());
                    }
                }
                State::BomStart(first) => {
                    let Some((&second, rest)) = bytes.split_first() else {
                        return;
                    };
                    let little_endian = match (*first, second) {
                        (0xff, 0xfe) => true,
                        (0xfe, 0xff) => false,
                        _ => {
                            self.state = State::Invalid;
                            return;
                        }
                    };
                    self.state = State::Utf16(Utf16State {
                        little_endian,
                        odd_byte: None,
                        high_surrogate: false,
                    });
                    bytes = rest;
                }
                State::Utf8(state) => {
                    self.contains_nul |= bytes.contains(&0);
                    if !state.push(bytes) {
                        self.state = State::Invalid;
                    }
                    return;
                }
                State::Utf16(state) => {
                    if !state.push(bytes, &mut self.contains_nul) {
                        self.state = State::Invalid;
                    }
                    return;
                }
                State::Invalid => return,
            }
        }
    }
}

impl Default for StreamingSourceTextValidator {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for StreamingSourceTextValidator {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StreamingSourceTextValidator")
            .field("contains_nul", &self.contains_nul)
            .finish_non_exhaustive()
    }
}

impl Utf8State {
    fn push(&mut self, mut bytes: &[u8]) -> bool {
        while self.length != 0 {
            let Some((&next, rest)) = bytes.split_first() else {
                return true;
            };
            let Some(slot) = self.carry.get_mut(self.length) else {
                return false;
            };
            *slot = next;
            self.length += 1;
            bytes = rest;
            match std::str::from_utf8(&self.carry[..self.length]) {
                Ok(_) => self.length = 0,
                Err(error) if error.error_len().is_some() => return false,
                Err(_) => {}
            }
        }
        match std::str::from_utf8(bytes) {
            Ok(_) => true,
            Err(error) if error.error_len().is_some() => false,
            Err(error) => {
                let tail = &bytes[error.valid_up_to()..];
                let Some(carry) = self.carry.get_mut(..tail.len()) else {
                    return false;
                };
                carry.copy_from_slice(tail);
                self.length = tail.len();
                true
            }
        }
    }
}

impl Utf16State {
    fn push(&mut self, mut bytes: &[u8], contains_nul: &mut bool) -> bool {
        if let Some(first) = self.odd_byte.take() {
            let Some((&second, rest)) = bytes.split_first() else {
                self.odd_byte = Some(first);
                return true;
            };
            if !self.unit([first, second], contains_nul) {
                return false;
            }
            bytes = rest;
        }
        let (units, remainder) = bytes.as_chunks::<2>();
        for unit in units {
            if !self.unit(*unit, contains_nul) {
                return false;
            }
        }
        self.odd_byte = remainder.first().copied();
        true
    }

    fn unit(&mut self, bytes: [u8; 2], contains_nul: &mut bool) -> bool {
        let unit = if self.little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        };
        if unit == 0 {
            *contains_nul = true;
            return false;
        }
        if self.high_surrogate {
            self.high_surrogate = false;
            return (0xdc00..=0xdfff).contains(&unit);
        }
        match unit {
            0xd800..=0xdbff => self.high_surrogate = true,
            0xdc00..=0xdfff => return false,
            _ => {}
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceText;
    use rootlight_cancel::CancellationReason;

    fn encoded(text: &str, little_endian: bool) -> Vec<u8> {
        let mut bytes = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        bytes
    }

    fn assert_every_split(bytes: &[u8]) {
        let cancellation = Cancellation::new();
        let expected = SourceText::parse(bytes, &cancellation)
            .unwrap()
            .map(SourceText::encoding);
        for first in 0..=bytes.len() {
            for second in first..=bytes.len() {
                let mut validator = StreamingSourceTextValidator::new();
                for chunk in [
                    &[][..],
                    &bytes[..first],
                    &bytes[first..second],
                    &bytes[second..],
                    &[],
                ] {
                    validator.push(chunk, &cancellation).unwrap();
                }
                if expected == Some(TextEncoding::Utf8) {
                    assert_eq!(validator.contains_nul(), bytes.contains(&0));
                }
                assert_eq!(
                    validator.finish(),
                    expected,
                    "{bytes:?} splits {first}/{second}"
                );
            }
        }
    }

    #[test]
    fn empty_input_and_utf8_nul_keep_whole_source_rules() {
        assert_eq!(
            StreamingSourceTextValidator::new().finish(),
            Some(TextEncoding::Utf8)
        );
        assert_every_split(b"");
        assert_every_split(b"a\0b");
        let mut validator = StreamingSourceTextValidator::default();
        validator.push(b"\0", &Cancellation::new()).unwrap();
        assert!(validator.contains_nul());
        assert_eq!(validator.finish(), Some(TextEncoding::Utf8));
    }

    #[test]
    fn valid_encodings_match_whole_validation_at_every_split() {
        for text in [
            "",
            "source\r\n",
            "\u{feff}aλ😀z",
            "\u{7f}\u{80}\u{7ff}\u{800}\u{d7ff}\u{e000}\u{ffff}\u{10000}\u{10ffff}",
        ] {
            assert_every_split(text.as_bytes());
            assert_every_split(&encoded(text, true));
            assert_every_split(&encoded(text, false));
        }
    }

    #[test]
    fn malformed_and_truncated_inputs_match_whole_validation_at_every_split() {
        for bytes in [
            &b"\xff"[..],
            b"\xfe",
            b"\xef",
            b"\xef\xbb",
            b"\x80",
            b"\xc0\x80",
            b"\xe0\x80\x80",
            b"\xed\xa0\x80",
            b"\xf0\x80\x80\x80",
            b"\xf4\x90\x80\x80",
            b"\xf5\x80\x80\x80",
            b"a\xffz",
            b"\xff\xfe\x00\x00A\x00\x00\x00",
            b"\x00\x00\xfe\xff\x00\x00\x00A",
        ] {
            assert_every_split(bytes);
        }
        for little_endian in [true, false] {
            for units in [
                &[][..],
                &[0xd800],
                &[0xdc00],
                &[0xd800, 0xd800],
                &[0xd800, 0x61],
                &[0xd800, 0],
                &[0],
                &[0xd83d, 0xde00, 0xd800],
            ] {
                let mut bytes = encoded("", little_endian);
                for unit in units {
                    bytes.extend(if little_endian {
                        u16::to_le_bytes(*unit)
                    } else {
                        u16::to_be_bytes(*unit)
                    });
                }
                assert_every_split(&bytes);
                bytes.push(0x61);
                assert_every_split(&bytes);
            }
        }
    }

    #[test]
    fn every_two_byte_input_matches_whole_validation() {
        let cancellation = Cancellation::new();
        for first in u8::MIN..=u8::MAX {
            for second in u8::MIN..=u8::MAX {
                let bytes = [first, second];
                let expected = SourceText::parse(&bytes, &cancellation)
                    .unwrap()
                    .map(SourceText::encoding);
                let mut validator = StreamingSourceTextValidator::new();
                validator.push(&bytes[..1], &cancellation).unwrap();
                validator.push(&bytes[1..], &cancellation).unwrap();
                assert_eq!(validator.finish(), expected, "{bytes:?}");
            }
        }
    }

    #[test]
    fn scalars_and_surrogates_cross_internal_check_boundaries() {
        let cancellation = Cancellation::new();
        for prefix in CHECK_BYTES - 4..=CHECK_BYTES + 1 {
            let text = format!("{}😀λz", "a".repeat(prefix));
            for bytes in [
                text.as_bytes().to_vec(),
                encoded(&text, true),
                encoded(&text, false),
            ] {
                let expected = SourceText::parse(&bytes, &cancellation)
                    .unwrap()
                    .map(SourceText::encoding);
                let mut validator = StreamingSourceTextValidator::new();
                validator.push(&bytes, &cancellation).unwrap();
                assert_eq!(validator.finish(), expected);
            }
        }
    }

    #[test]
    fn cancellation_before_or_during_a_push_prevents_complete_validation() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        for bytes in [&b"source"[..], b""] {
            let mut validator = StreamingSourceTextValidator::new();
            assert!(validator.push(bytes, &cancellation).is_err());
            assert_eq!(validator.finish(), None);
        }
        let cancellation = Cancellation::new();
        let mut checks = 0;
        let mut validator = StreamingSourceTextValidator::new();
        let result = validator.push_with_check(&[b'a'; CHECK_BYTES * 3], || {
            checks += 1;
            if checks == 2 {
                cancellation.cancel(CancellationReason::ClientRequest);
            }
            cancellation.check()
        });
        assert_eq!(
            result.unwrap_err().reason(),
            CancellationReason::ClientRequest
        );
        assert_eq!(checks, 2);
        validator
            .push(b"valid suffix", &Cancellation::new())
            .unwrap();
        assert_eq!(validator.finish(), None);
    }

    #[test]
    fn input_beyond_snapshot_ceiling_needs_only_reused_bounded_chunks() {
        let cancellation = Cancellation::new();
        let block = [b'a'; 64 * 1024];
        let mut validator = StreamingSourceTextValidator::new();
        for _ in 0..1024 {
            validator.push(&block, &cancellation).unwrap();
        }
        validator.push(b"z", &cancellation).unwrap();
        assert_eq!(validator.finish(), Some(TextEncoding::Utf8));
        assert_eq!(crate::MAX_SNAPSHOT_BYTES, 64 * 1024 * 1024);
        assert!(size_of::<StreamingSourceTextValidator>() <= 32);
    }
}
