//! First-party Unicode case folding for repository and query identity.
//!
//! The pinned Unicode 9 data preserves existing index keys while the
//! implementation and build path remain owned by Rootlight.

use std::{iter, str::Chars};

include!(concat!(env!("OUT_DIR"), "/case_folding.rs"));

/// Unicode data version used for case-insensitive index keys.
pub const UNICODE_VERSION: (u8, u8, u8) = (9, 0, 0);

/// Iterates the full, locale-independent folded form of each scalar.
#[derive(Clone, Debug)]
pub struct CaseFold<I> {
    inner: I,
    pending: [char; 2],
    pending_len: u8,
}

impl<I: Iterator<Item = char>> Iterator for CaseFold<I> {
    type Item = char;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pending_len > 0 {
            let next = self.pending[0];
            if self.pending_len == 2 {
                self.pending[0] = self.pending[1];
            }
            self.pending_len -= 1;
            return Some(next);
        }
        let original = self.inner.next()?;
        let Ok(index) = CASE_FOLD_TABLE.binary_search_by_key(&original, |entry| entry.0) else {
            return Some(original);
        };
        let (_, mapping, width) = CASE_FOLD_TABLE[index];
        if width > 1 {
            self.pending[0] = mapping[1];
            self.pending_len = 1;
        }
        if width > 2 {
            self.pending[1] = mapping[2];
            self.pending_len = 2;
        }
        Some(mapping[0])
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let (lower, upper) = self.inner.size_hint();
        let pending = usize::from(self.pending_len);
        (
            lower.saturating_add(pending),
            upper
                .and_then(|upper| upper.checked_mul(3))
                .and_then(|upper| upper.checked_add(pending)),
        )
    }
}

/// Converts character iterators, strings, and scalars to full case-fold iterators.
pub trait UnicodeCaseFold<I: Iterator<Item = char>>: Sized {
    /// Returns the Unicode 9 full, non-Turkic case folding of the input.
    fn case_fold(self) -> CaseFold<I>;
}

impl<I: Iterator<Item = char>> UnicodeCaseFold<I> for I {
    fn case_fold(self) -> CaseFold<I> {
        CaseFold {
            inner: self,
            pending: ['\0'; 2],
            pending_len: 0,
        }
    }
}

impl<'a> UnicodeCaseFold<Chars<'a>> for &'a str {
    fn case_fold(self) -> CaseFold<Chars<'a>> {
        self.chars().case_fold()
    }
}

impl UnicodeCaseFold<iter::Once<char>> for char {
    fn case_fold(self) -> CaseFold<iter::Once<char>> {
        iter::once(self).case_fold()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_folding_preserves_expansions_and_pinned_unicode_version() {
        assert_eq!("Straße".case_fold().collect::<String>(), "strasse");
        assert_eq!("İΣς".case_fold().collect::<String>(), "i\u{307}σσ");
        assert_eq!("ﬃ".case_fold().collect::<String>(), "ffi");
        assert_eq!("\u{1c90}".case_fold().collect::<String>(), "\u{1c90}");
        assert_eq!(UNICODE_VERSION, (9, 0, 0));
    }

    #[test]
    fn expansion_and_size_hints_remain_exactly_iterable() {
        let mut folded = vec!['ß', 'X'].into_iter().case_fold();
        assert_eq!(folded.size_hint(), (2, Some(6)));
        assert_eq!(folded.next(), Some('s'));
        assert_eq!(folded.size_hint(), (2, Some(4)));
        assert_eq!(folded.collect::<String>(), "sx");
    }

    #[test]
    fn every_unicode_scalar_matches_the_pinned_mapping() {
        let mut digest = 0xcbf29ce484222325_u64;
        for code in 0..=0x10ffff {
            let Some(character) = char::from_u32(code) else {
                continue;
            };
            for folded in character.case_fold() {
                for byte in u32::from(folded).to_le_bytes() {
                    digest = (digest ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                }
            }
            digest = (digest ^ 0xff).wrapping_mul(0x100000001b3);
        }
        assert_eq!(digest, 0xc8a1f123816de847);
    }
}
