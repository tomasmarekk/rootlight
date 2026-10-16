//! Bounded, case-sensitive scalar matching over immutable original source bytes.
//!
//! KMP continuations retain partial matches between forward pages. Source and
//! query identity authentication remains the responsibility of protocol callers.

use std::{fmt, mem::size_of, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_vfs::{SourceText, TextEncoding};

/// Independent resource limits for compilation and one forward scan page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiteralSearchBudget {
    /// Maximum UTF-8 bytes in the literal query.
    pub max_query_bytes: usize,
    /// Maximum original source bytes consumed forward in one page.
    pub max_scan_bytes: usize,
    /// Maximum match spans retained in one page.
    pub max_matches: usize,
    /// Maximum aggregate capacities of pattern tables and returned spans.
    pub max_memory_bytes: usize,
    /// Maximum preceding source bytes decoded to authenticate a resumed prefix.
    pub max_restore_bytes: usize,
}

/// A failure before a complete, resumable page could be returned.
#[derive(Debug, thiserror::Error)]
pub enum LiteralSearchError {
    /// Empty literals do not have a finite sequence of source matches.
    #[error("literal query must not be empty")]
    EmptyQuery,
    /// The query exceeds its independent byte allowance.
    #[error("literal query exceeds its byte limit")]
    QueryLimit,
    /// A scan or result allowance is zero.
    #[error("literal scan and match limits must be positive")]
    InvalidBudget,
    /// Pattern or result capacity cannot fit the memory allowance.
    #[error("literal search exceeds its memory limit")]
    MemoryLimit,
    /// Validating a continuation requires more lookbehind than allowed.
    #[error("literal cursor validation exceeds its restoration limit")]
    RestoreLimit,
    /// The offset or partial prefix does not describe the supplied source/query.
    #[error("literal cursor does not match the source and query")]
    InvalidCursor,
    /// The next scalar cannot fit the forward allowance; no cursor is advanced.
    #[error("literal scan allowance cannot consume the next scalar")]
    NoProgress,
    /// A fallible vector allocation failed.
    #[error("literal search allocation failed")]
    AllocationFailed,
    /// Cooperative cancellation or its deadline stopped the operation.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// A caller-owned control stopped the operation; its original error is external.
    #[error("literal search stopped by caller control")]
    ExternalControlStopped,
}

/// The explicit reason a page reached its boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralSearchStop {
    /// Every scalar through the original source EOF was examined.
    EndOfSource,
    /// The forward byte allowance was consumed or cannot fit the next scalar.
    ScanLimit,
    /// The page contains the requested maximum number of matches.
    MatchLimit,
    /// The page filled the match capacity permitted by its memory allowance.
    MemoryLimit,
}

/// A forward continuation whose partial prefix is validated on every use.
///
/// This cursor does not authenticate file or generation identity. Protocol
/// callers must additionally bind those identities and the query to their wire
/// cursor. Restoring arbitrary parts still verifies the actual source suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiteralSearchCursor {
    next_byte: usize,
    matched_prefix: usize,
}

impl LiteralSearchCursor {
    /// Returns the next original source byte to examine.
    #[must_use]
    pub const fn next_byte(self) -> usize {
        self.next_byte
    }

    /// Returns the number of query scalars matched immediately before that byte.
    #[must_use]
    pub const fn matched_prefix(self) -> usize {
        self.matched_prefix
    }
}

/// All matches and work coverage from one successful scan page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiteralSearchPage {
    /// Exact original byte spans, including overlapping matches in source order.
    pub matches: Vec<Range<usize>>,
    /// Forward source bytes examined by this page, excluding restoration work.
    pub scanned: Range<usize>,
    /// Preceding source bytes independently examined to validate the cursor.
    pub restoration_bytes: usize,
    /// Whether scanning ended at EOF or at an explicit resource boundary.
    pub stop: LiteralSearchStop,
    /// Present only when a positive-progress page leaves source bytes unexamined.
    pub continuation: Option<LiteralSearchCursor>,
}

/// A compiled scalar literal with KMP failure links and no owned source text.
pub struct LiteralPattern {
    scalars: Vec<char>,
    failure: Vec<usize>,
    query_bytes: usize,
    utf16_bytes: usize,
}

impl fmt::Debug for LiteralPattern {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LiteralPattern")
            .field("scalar_count", &self.scalars.len())
            .finish_non_exhaustive()
    }
}

impl LiteralPattern {
    /// Compiles a literal without normalization or case folding.
    ///
    /// # Errors
    /// Returns query, memory, allocation, or cancellation failures.
    pub fn compile(
        query: &str,
        budget: LiteralSearchBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, LiteralSearchError> {
        Self::compile_with_check(query, budget, || Ok(cancellation.check()?))
    }

    /// Compiles while enforcing caller-owned cooperative controls.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::compile`] or the first `check` error.
    pub fn compile_with_check(
        query: &str,
        budget: LiteralSearchBudget,
        mut check: impl FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<Self, LiteralSearchError> {
        check()?;
        if query.is_empty() {
            return Err(LiteralSearchError::EmptyQuery);
        }
        if query.len() > budget.max_query_bytes {
            return Err(LiteralSearchError::QueryLimit);
        }
        let mut count = 0_usize;
        let mut utf16_bytes = 0_usize;
        for character in query.chars() {
            check()?;
            count += 1;
            utf16_bytes = utf16_bytes
                .checked_add(character.len_utf16() * 2)
                .ok_or(LiteralSearchError::MemoryLimit)?;
        }
        let required = count
            .checked_mul(size_of::<char>() + size_of::<usize>())
            .ok_or(LiteralSearchError::MemoryLimit)?;
        if required > budget.max_memory_bytes {
            return Err(LiteralSearchError::MemoryLimit);
        }
        let mut scalars = Vec::new();
        let mut failure = Vec::new();
        scalars
            .try_reserve_exact(count)
            .map_err(|_| LiteralSearchError::AllocationFailed)?;
        failure
            .try_reserve_exact(count)
            .map_err(|_| LiteralSearchError::AllocationFailed)?;
        for character in query.chars() {
            check()?;
            let mut prefix = failure.last().copied().unwrap_or(0);
            while prefix > 0 && scalars[prefix] != character {
                check()?;
                prefix = failure[prefix - 1];
            }
            if !scalars.is_empty() && scalars[prefix] == character {
                prefix += 1;
            }
            scalars.push(character);
            failure.push(prefix);
        }
        let pattern = Self {
            scalars,
            failure,
            query_bytes: query.len(),
            utf16_bytes,
        };
        if pattern.memory_bytes() > budget.max_memory_bytes {
            return Err(LiteralSearchError::MemoryLimit);
        }
        check()?;
        Ok(pattern)
    }

    /// Reconstructs cursor parts only after validating the actual preceding text.
    ///
    /// # Errors
    /// Rejects non-scalar offsets, forged prefixes, insufficient restoration work
    /// allowance, and cancellation. UTF-16 byte zero is canonicalized to byte two.
    pub fn restore_cursor(
        &self,
        text: SourceText<'_>,
        next_byte: usize,
        matched_prefix: usize,
        budget: LiteralSearchBudget,
        cancellation: &Cancellation,
    ) -> Result<LiteralSearchCursor, LiteralSearchError> {
        self.restore_cursor_with_check(text, next_byte, matched_prefix, budget, || {
            Ok(cancellation.check()?)
        })
    }

    /// Restores a cursor while enforcing caller-owned cooperative controls.
    ///
    /// # Errors
    /// Returns cursor, restoration, or caller-control failures.
    pub fn restore_cursor_with_check(
        &self,
        text: SourceText<'_>,
        next_byte: usize,
        matched_prefix: usize,
        budget: LiteralSearchBudget,
        mut check: impl FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<LiteralSearchCursor, LiteralSearchError> {
        let cursor = LiteralSearchCursor {
            next_byte: if next_byte == 0 {
                content_start(text)
            } else {
                next_byte
            },
            matched_prefix,
        };
        self.validate_cursor(text, cursor, budget, &mut check)?;
        Ok(cursor)
    }

    /// Scans one strictly forward page and retains every match found in that work.
    ///
    /// # Errors
    /// Returns invalid-cursor, resource, allocation, or cancellation failures.
    /// A failure returns no partial page and never advances the caller's cursor.
    pub fn scan_page(
        &self,
        text: SourceText<'_>,
        cursor: Option<LiteralSearchCursor>,
        budget: LiteralSearchBudget,
        cancellation: &Cancellation,
    ) -> Result<LiteralSearchPage, LiteralSearchError> {
        self.scan_page_with_check(text, cursor, budget, || Ok(cancellation.check()?))
    }

    /// Scans while enforcing caller-owned controls, including duration limits.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::scan_page`] or the first `check` error.
    pub fn scan_page_with_check(
        &self,
        text: SourceText<'_>,
        cursor: Option<LiteralSearchCursor>,
        budget: LiteralSearchBudget,
        mut check: impl FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<LiteralSearchPage, LiteralSearchError> {
        check()?;
        if budget.max_scan_bytes == 0 || budget.max_matches == 0 {
            return Err(LiteralSearchError::InvalidBudget);
        }
        self.check_pattern_budget(budget)?;
        let mut current = cursor.unwrap_or(LiteralSearchCursor {
            next_byte: content_start(text),
            matched_prefix: 0,
        });
        let restoration_bytes = if cursor.is_some() {
            self.validate_cursor(text, current, budget, &mut check)?
        } else {
            0
        };
        let start = current.next_byte;
        if start == text.byte_len() {
            return Ok(LiteralSearchPage {
                matches: Vec::new(),
                scanned: start..start,
                restoration_bytes,
                stop: LiteralSearchStop::EndOfSource,
                continuation: None,
            });
        }
        let mut scan_end = start + budget.max_scan_bytes.min(text.byte_len() - start);
        while text.chars_from_byte(scan_end).is_none() {
            scan_end -= 1;
        }
        if scan_end == start {
            return Err(LiteralSearchError::NoProgress);
        }
        let capacity = budget
            .max_matches
            .min((budget.max_memory_bytes - self.memory_bytes()) / size_of::<Range<usize>>())
            .min(text.byte_len() - start);
        if capacity == 0 {
            return Err(LiteralSearchError::MemoryLimit);
        }
        let mut matches = Vec::new();
        matches
            .try_reserve_exact(capacity)
            .map_err(|_| LiteralSearchError::AllocationFailed)?;
        if matches.capacity()
            > (budget.max_memory_bytes - self.memory_bytes()) / size_of::<Range<usize>>()
        {
            return Err(LiteralSearchError::MemoryLimit);
        }
        let width = self.encoded_width(text.encoding());
        let mut stop = if scan_end == text.byte_len() {
            LiteralSearchStop::EndOfSource
        } else {
            LiteralSearchStop::ScanLimit
        };
        let mut characters = text
            .chars_from_byte(start)
            .ok_or(LiteralSearchError::InvalidCursor)?;
        while current.next_byte < scan_end {
            check()?;
            let character = characters.next().ok_or(LiteralSearchError::InvalidCursor)?;
            let character_bytes = text.encoding().character_bytes(character);
            let (prefix, found) = self.advance(current.matched_prefix, character, &mut check)?;
            current.matched_prefix = prefix;
            current.next_byte += character_bytes;
            if found {
                matches.push(current.next_byte - width..current.next_byte);
                if matches.len() == capacity && current.next_byte < text.byte_len() {
                    stop = if capacity == budget.max_matches {
                        LiteralSearchStop::MatchLimit
                    } else {
                        LiteralSearchStop::MemoryLimit
                    };
                    break;
                }
            }
        }
        check()?;
        Ok(LiteralSearchPage {
            matches,
            scanned: start..current.next_byte,
            restoration_bytes,
            stop,
            continuation: (stop != LiteralSearchStop::EndOfSource).then_some(current),
        })
    }

    pub(crate) fn scan_parts_with_check(
        &self,
        text: SourceText<'_>,
        cursor: Option<(usize, usize)>,
        budget: LiteralSearchBudget,
        check: impl FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<LiteralSearchPage, LiteralSearchError> {
        // Wire parts enter the same validation path as an existing cursor, so
        // source services need not perform and charge the lookbehind twice.
        let cursor = cursor.map(|(next_byte, matched_prefix)| LiteralSearchCursor {
            next_byte: if next_byte == 0 {
                content_start(text)
            } else {
                next_byte
            },
            matched_prefix,
        });
        self.scan_page_with_check(text, cursor, budget, check)
    }

    fn check_pattern_budget(&self, budget: LiteralSearchBudget) -> Result<(), LiteralSearchError> {
        if self.query_bytes > budget.max_query_bytes {
            return Err(LiteralSearchError::QueryLimit);
        }
        if self.memory_bytes() > budget.max_memory_bytes {
            return Err(LiteralSearchError::MemoryLimit);
        }
        Ok(())
    }

    pub(super) fn memory_bytes(&self) -> usize {
        self.scalars
            .capacity()
            .saturating_mul(size_of::<char>())
            .saturating_add(self.failure.capacity().saturating_mul(size_of::<usize>()))
    }

    pub(super) fn encoded_width(&self, encoding: TextEncoding) -> usize {
        match encoding {
            TextEncoding::Utf8 => self.query_bytes,
            TextEncoding::Utf16Le | TextEncoding::Utf16Be => self.utf16_bytes,
        }
    }

    pub(super) fn scalar_count(&self) -> usize {
        self.scalars.len()
    }

    pub(super) fn advance(
        &self,
        mut prefix: usize,
        character: char,
        check: &mut impl FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<(usize, bool), LiteralSearchError> {
        while prefix > 0 && self.scalars[prefix] != character {
            check()?;
            prefix = self.failure[prefix - 1];
        }
        if self.scalars[prefix] == character {
            prefix += 1;
        }
        let found = prefix == self.scalars.len();
        if found {
            prefix = self.failure[prefix - 1];
        }
        Ok((prefix, found))
    }

    fn validate_cursor(
        &self,
        text: SourceText<'_>,
        cursor: LiteralSearchCursor,
        budget: LiteralSearchBudget,
        check: &mut impl FnMut() -> Result<(), LiteralSearchError>,
    ) -> Result<usize, LiteralSearchError> {
        check()?;
        self.check_pattern_budget(budget)?;
        if cursor.matched_prefix >= self.scalars.len()
            || cursor.next_byte < content_start(text)
            || text.chars_from_byte(cursor.next_byte).is_none()
        {
            return Err(LiteralSearchError::InvalidCursor);
        }
        // A proper KMP prefix is shorter than the complete encoded pattern.
        // One pattern-width suffix therefore determines its exact state without
        // trusting wire parts or repeatedly decoding the entire source prefix.
        let mut start = cursor
            .next_byte
            .saturating_sub(self.encoded_width(text.encoding()))
            .max(content_start(text));
        while text.chars_from_byte(start).is_none() {
            start += 1;
        }
        let restoration_bytes = cursor.next_byte - start;
        if restoration_bytes > budget.max_restore_bytes {
            return Err(LiteralSearchError::RestoreLimit);
        }
        let mut position = start;
        let mut prefix = 0;
        let mut characters = text
            .chars_from_byte(start)
            .ok_or(LiteralSearchError::InvalidCursor)?;
        while position < cursor.next_byte {
            check()?;
            let character = characters.next().ok_or(LiteralSearchError::InvalidCursor)?;
            prefix = self.advance(prefix, character, check)?.0;
            position += text.encoding().character_bytes(character);
        }
        if position != cursor.next_byte || prefix != cursor.matched_prefix {
            return Err(LiteralSearchError::InvalidCursor);
        }
        check()?;
        Ok(restoration_bytes)
    }
}

fn content_start(text: SourceText<'_>) -> usize {
    if text.encoding() == TextEncoding::Utf8 {
        0
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> LiteralSearchBudget {
        LiteralSearchBudget {
            max_query_bytes: 4096,
            max_scan_bytes: 4096,
            max_matches: 128,
            max_memory_bytes: 32 * 1024,
            max_restore_bytes: 8192,
        }
    }

    fn encode(source: &str, encoding: TextEncoding) -> Vec<u8> {
        match encoding {
            TextEncoding::Utf8 => source.as_bytes().to_vec(),
            TextEncoding::Utf16Le => std::iter::once(0xfeff)
                .chain(source.encode_utf16())
                .flat_map(u16::to_le_bytes)
                .collect(),
            TextEncoding::Utf16Be => std::iter::once(0xfeff)
                .chain(source.encode_utf16())
                .flat_map(u16::to_be_bytes)
                .collect(),
        }
    }

    fn expected(source: &str, query: &str, encoding: TextEncoding) -> Vec<Range<usize>> {
        let mut byte = if encoding == TextEncoding::Utf8 { 0 } else { 2 };
        let width: usize = query
            .chars()
            .map(|value| encoding.character_bytes(value))
            .sum();
        source
            .char_indices()
            .filter_map(|(index, value)| {
                let start = byte;
                byte += encoding.character_bytes(value);
                source[index..]
                    .starts_with(query)
                    .then_some(start..start + width)
            })
            .collect()
    }

    fn pages(
        source: &str,
        query: &str,
        encoding: TextEncoding,
        limits: LiteralSearchBudget,
    ) -> Vec<Range<usize>> {
        let bytes = encode(source, encoding);
        let text = SourceText::parse(&bytes, &Cancellation::new())
            .unwrap()
            .unwrap();
        let pattern = LiteralPattern::compile(query, budget(), &Cancellation::new()).unwrap();
        let mut cursor = None;
        let mut position = content_start(text);
        let mut found = Vec::new();
        loop {
            let page = pattern
                .scan_page(text, cursor, limits, &Cancellation::new())
                .unwrap();
            assert_eq!(page.scanned.start, position);
            assert!(page.scanned.len() <= limits.max_scan_bytes);
            assert!(page.restoration_bytes <= pattern.encoded_width(encoding));
            position = page.scanned.end;
            found.extend(page.matches);
            cursor = page.continuation;
            if let Some(next) = cursor {
                assert!(page.scanned.start < page.scanned.end);
                assert_eq!(next.next_byte(), position);
                assert_ne!(page.stop, LiteralSearchStop::EndOfSource);
                let restored = pattern
                    .restore_cursor(
                        text,
                        next.next_byte(),
                        next.matched_prefix(),
                        limits,
                        &Cancellation::new(),
                    )
                    .unwrap();
                assert_eq!(restored, next);
            } else {
                assert_eq!(page.stop, LiteralSearchStop::EndOfSource);
                assert_eq!(position, bytes.len());
                break;
            }
        }
        found
    }

    #[test]
    fn differential_overlaps_and_paging_preserve_original_bytes_in_three_encodings() {
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ] {
            for source in [
                "",
                "aaaaaa",
                "ababababacaba",
                "xλ😀λ😀λz",
                "\u{feff}A\r\nA\r\n",
                "AaAaAAa",
                "abcabcababcda",
            ] {
                for query in [
                    "a", "aa", "aaa", "aba", "abac", "λ😀λ", "😀", "\r\n", "A", "\u{feff}",
                    "abcda", "never",
                ] {
                    for scan_bytes in [4, 5, 7, 4096] {
                        for max_matches in [1, 2, 128] {
                            let limits = LiteralSearchBudget {
                                max_scan_bytes: scan_bytes,
                                max_matches,
                                ..budget()
                            };
                            assert_eq!(
                                pages(source, query, encoding, limits),
                                expected(source, query, encoding),
                                "{source:?} {query:?} {encoding:?} {scan_bytes} {max_matches}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn one_byte_pages_keep_long_partial_prefixes_and_find_an_internal_literal() {
        let query = "AAAAAACEEEEIIIINOOOOOUUUUYaaaaaaceeeeiiiinooooouuuuyyAaAaAaCcCcC";
        assert_eq!(query.len(), 64);
        let source = format!("UnicodeWordPrefix{query}WordSuffix{query}");
        let limits = LiteralSearchBudget {
            max_scan_bytes: 1,
            max_matches: 1,
            ..budget()
        };
        assert_eq!(
            pages(&source, query, TextEncoding::Utf8, limits),
            expected(&source, query, TextEncoding::Utf8)
        );
        assert_eq!(
            pages("aaaaa", "aaa", TextEncoding::Utf8, limits),
            vec![0..3, 1..4, 2..5]
        );
    }

    #[test]
    fn restored_prefixes_reject_forged_high_low_and_changed_source_states() {
        let cancellation = Cancellation::new();
        let pattern = LiteralPattern::compile("abcda", budget(), &cancellation).unwrap();
        let source = SourceText::from_utf8("xxxxxa");
        assert!(matches!(
            pattern.restore_cursor(source, 5, 4, budget(), &cancellation),
            Err(LiteralSearchError::InvalidCursor)
        ));
        assert!(matches!(
            pattern.scan_page(
                source,
                Some(LiteralSearchCursor {
                    next_byte: 5,
                    matched_prefix: 4
                }),
                budget(),
                &cancellation
            ),
            Err(LiteralSearchError::InvalidCursor)
        ));
        let real = SourceText::from_utf8("xabcdabcda");
        assert!(matches!(
            pattern.restore_cursor(real, 5, 0, budget(), &cancellation),
            Err(LiteralSearchError::InvalidCursor)
        ));
        let cursor = pattern
            .restore_cursor(real, 5, 4, budget(), &cancellation)
            .unwrap();
        assert!(matches!(
            pattern.scan_page(source, Some(cursor), budget(), &cancellation),
            Err(LiteralSearchError::InvalidCursor)
        ));
        let page = pattern
            .scan_page(real, Some(cursor), budget(), &cancellation)
            .unwrap();
        assert_eq!(page.matches, vec![1..6, 5..10]);
        for (position, prefix) in [(usize::MAX, 0), (1, 5), (1, usize::MAX)] {
            assert!(matches!(
                pattern.restore_cursor(real, position, prefix, budget(), &cancellation),
                Err(LiteralSearchError::InvalidCursor)
            ));
        }
    }

    #[test]
    fn every_restored_prefix_matches_an_independent_suffix_oracle() {
        let cancellation = Cancellation::new();
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ] {
            for query in ["a", "aaaa", "ababa", "λ😀λ", "😀λ😀x", "abcda"] {
                let pattern = LiteralPattern::compile(query, budget(), &cancellation).unwrap();
                let prefix_strings: Vec<String> = (0..query.chars().count())
                    .map(|count| query.chars().take(count).collect())
                    .collect();
                let source = format!("xx{query}{query}ababaλ😀λ😀xaaaa{query}");
                let bytes = encode(&source, encoding);
                let text = SourceText::parse(&bytes, &cancellation).unwrap().unwrap();
                let mut byte = content_start(text);
                for (index, character) in source
                    .char_indices()
                    .chain(std::iter::once((source.len(), '\0')))
                {
                    let expected = prefix_strings
                        .iter()
                        .enumerate()
                        .rev()
                        .find(|(_, prefix)| source[..index].ends_with(prefix.as_str()))
                        .unwrap()
                        .0;
                    for supplied in 0..=prefix_strings.len() {
                        let result =
                            pattern.restore_cursor(text, byte, supplied, budget(), &cancellation);
                        assert_eq!(
                            result.is_ok(),
                            supplied == expected,
                            "{encoding:?} {query:?} {index} {supplied} {expected}"
                        );
                    }
                    byte += encoding.character_bytes(character);
                }
            }
        }
    }

    #[test]
    fn scalar_boundaries_boms_eof_and_restoration_have_independent_limits() {
        let cancellation = Cancellation::new();
        let pattern = LiteralPattern::compile("😀x", budget(), &cancellation).unwrap();
        for encoding in [
            TextEncoding::Utf8,
            TextEncoding::Utf16Le,
            TextEncoding::Utf16Be,
        ] {
            let bytes = encode("😀x", encoding);
            let text = SourceText::parse(&bytes, &cancellation).unwrap().unwrap();
            let start = content_start(text);
            for inside in 1..4 {
                assert!(matches!(
                    pattern.restore_cursor(text, start + inside, 0, budget(), &cancellation),
                    Err(LiteralSearchError::InvalidCursor)
                ));
            }
            assert!(matches!(
                pattern.scan_page(
                    text,
                    None,
                    LiteralSearchBudget {
                        max_scan_bytes: 3,
                        ..budget()
                    },
                    &cancellation
                ),
                Err(LiteralSearchError::NoProgress)
            ));
            let first = pattern
                .scan_page(
                    text,
                    None,
                    LiteralSearchBudget {
                        max_scan_bytes: 4,
                        ..budget()
                    },
                    &cancellation,
                )
                .unwrap();
            assert_eq!(first.scanned, start..start + 4);
            assert!(first.matches.is_empty());
            assert!(matches!(
                pattern.scan_page(
                    text,
                    first.continuation,
                    LiteralSearchBudget {
                        max_restore_bytes: 3,
                        ..budget()
                    },
                    &cancellation
                ),
                Err(LiteralSearchError::RestoreLimit)
            ));
            let last = pattern
                .scan_page(
                    text,
                    first.continuation,
                    LiteralSearchBudget {
                        max_scan_bytes: 2,
                        ..budget()
                    },
                    &cancellation,
                )
                .unwrap();
            assert_eq!(last.restoration_bytes, 4);
            assert_eq!(last.matches, vec![start..bytes.len()]);
            assert_eq!(last.stop, LiteralSearchStop::EndOfSource);
        }
    }

    #[test]
    fn memory_query_result_and_work_limits_remain_distinct() {
        let cancellation = Cancellation::new();
        assert!(matches!(
            LiteralPattern::compile("", budget(), &cancellation),
            Err(LiteralSearchError::EmptyQuery)
        ));
        assert!(matches!(
            LiteralPattern::compile(
                "abcd",
                LiteralSearchBudget {
                    max_query_bytes: 3,
                    ..budget()
                },
                &cancellation
            ),
            Err(LiteralSearchError::QueryLimit)
        ));
        assert!(matches!(
            LiteralPattern::compile(
                "abcd",
                LiteralSearchBudget {
                    max_memory_bytes: 1,
                    ..budget()
                },
                &cancellation
            ),
            Err(LiteralSearchError::MemoryLimit)
        ));
        let pattern = LiteralPattern::compile("a", budget(), &cancellation).unwrap();
        let text = SourceText::from_utf8("aaaa");
        let memory = LiteralSearchBudget {
            max_memory_bytes: pattern.memory_bytes() + size_of::<Range<usize>>(),
            ..budget()
        };
        let page = pattern
            .scan_page(text, None, memory, &cancellation)
            .unwrap();
        assert_eq!(page.matches, vec![0..1]);
        assert_eq!(page.stop, LiteralSearchStop::MemoryLimit);
        let page = pattern
            .scan_page(
                text,
                None,
                LiteralSearchBudget {
                    max_matches: 1,
                    ..budget()
                },
                &cancellation,
            )
            .unwrap();
        assert_eq!(page.stop, LiteralSearchStop::MatchLimit);
        let page = pattern
            .scan_page(
                text,
                None,
                LiteralSearchBudget {
                    max_scan_bytes: 1,
                    ..budget()
                },
                &cancellation,
            )
            .unwrap();
        assert_eq!(page.stop, LiteralSearchStop::ScanLimit);
        for limits in [
            LiteralSearchBudget {
                max_scan_bytes: 0,
                ..budget()
            },
            LiteralSearchBudget {
                max_matches: 0,
                ..budget()
            },
        ] {
            assert!(matches!(
                pattern.scan_page(text, None, limits, &cancellation),
                Err(LiteralSearchError::InvalidBudget)
            ));
        }
        assert_eq!(
            pages("aaaa", "a", TextEncoding::Utf8, memory),
            vec![0..1, 1..2, 2..3, 3..4]
        );
    }

    #[test]
    fn cancellation_and_caller_controls_do_not_return_partial_matches() {
        let cancellation = Cancellation::new();
        let pattern = LiteralPattern::compile("a", budget(), &cancellation).unwrap();
        let cancelled = Cancellation::with_deadline(std::time::Instant::now());
        assert!(matches!(
            LiteralPattern::compile("a", budget(), &cancelled),
            Err(LiteralSearchError::Cancelled(_))
        ));
        assert!(matches!(
            pattern.scan_page(SourceText::from_utf8("aaaa"), None, budget(), &cancelled),
            Err(LiteralSearchError::Cancelled(_))
        ));
        let mut checks = 0;
        let result =
            pattern.scan_page_with_check(SourceText::from_utf8("aaaa"), None, budget(), || {
                checks += 1;
                if checks == 4 {
                    Err(LiteralSearchError::ExternalControlStopped)
                } else {
                    Ok(())
                }
            });
        assert!(matches!(
            result,
            Err(LiteralSearchError::ExternalControlStopped)
        ));
        assert_eq!(checks, 4);
        let mut checks = 0;
        let result = LiteralPattern::compile_with_check("aaaa", budget(), || {
            checks += 1;
            if checks == 3 {
                Err(LiteralSearchError::ExternalControlStopped)
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            result,
            Err(LiteralSearchError::ExternalControlStopped)
        ));
    }
}
