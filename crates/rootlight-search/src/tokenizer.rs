use rootlight_unicode::{GeneralCategory, canonical_case_fold, get_general_category};
use tantivy::tokenizer::{Token, TokenStream, Tokenizer};

use crate::model::{MAX_TERM_BYTES, SearchError};

/// Splits code spelling at separators, case transitions, and letter/digit edges.
#[derive(Debug, Clone, Default)]
pub(crate) struct CodeTokenizer {
    token: Token,
    spans: Vec<Span>,
}

impl Tokenizer for CodeTokenizer {
    type TokenStream<'a> = CodeTokenStream<'a>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        self.token.reset();
        split_spans(text, &mut self.spans);
        CodeTokenStream {
            text,
            spans: &self.spans,
            next: 0,
            token: &mut self.token,
        }
    }
}

pub(crate) struct CodeTokenStream<'a> {
    text: &'a str,
    spans: &'a [Span],
    next: usize,
    token: &'a mut Token,
}

impl TokenStream for CodeTokenStream<'_> {
    fn advance(&mut self) -> bool {
        let Some(span) = self.spans.get(self.next).copied() else {
            return false;
        };
        self.next += 1;
        self.token.position = self.token.position.wrapping_add(1);
        self.token.offset_from = span.start;
        self.token.offset_to = span.end;
        self.token.text.clear();
        let text = &self.text[span.start..span.end];
        if text.is_ascii() {
            self.token.text.push_str(text);
            self.token.text.make_ascii_lowercase();
        } else {
            self.token.text.push_str(&normalize_text(text));
        }
        true
    }

    fn token(&self) -> &Token {
        self.token
    }

    fn token_mut(&mut self) -> &mut Token {
        self.token
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Span {
    start: usize,
    end: usize,
}

pub(crate) fn has_oversized_term(text: &str) -> bool {
    let mut spans = Vec::new();
    split_spans(text, &mut spans);
    spans
        .into_iter()
        .any(|span| normalize_text(&text[span.start..span.end]).len() > MAX_TERM_BYTES)
}

pub(crate) fn token_texts(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    split_spans(text, &mut spans);
    spans
        .into_iter()
        .map(|span| normalize_text(&text[span.start..span.end]))
        .collect()
}

pub(crate) fn oversized_name_tokens(text: &str) -> (u64, u64) {
    let mut spans = Vec::new();
    split_spans(text, &mut spans);
    let mut count = 0;
    let mut bytes = 0;
    for span in spans {
        if normalize_text(&text[span.start..span.end]).len() > MAX_TERM_BYTES {
            // Callers admit at most one 32-KiB name, so both sums fit u64.
            count += 1;
            bytes += u64::try_from(span.end - span.start).expect("bounded name span fits u64");
        }
    }
    (count, bytes)
}

/// Applies the index format's full, non-Turkic Unicode normalization.
pub(crate) fn normalize_text(input: &str) -> String {
    canonical_case_fold(input)
}

/// Visits complete source tokens without retaining an oversized word or span list.
///
/// A pending base scalar and its marks preserve acronym lookahead across arbitrary
/// mark runs. Scratch grows fallibly within the caller's source-witness bound.
/// Exhaustion omits a whole token and never creates a boundary.
pub(crate) struct BoundedCodeTokenizer {
    bytes: Vec<u8>,
    maximum_bytes: usize,
    length: usize,
    pending_start: usize,
    token_oversized: bool,
    pending_oversized: bool,
    pending_base: Option<char>,
    previous_base: Option<char>,
}

impl BoundedCodeTokenizer {
    pub(crate) fn new(maximum_bytes: usize) -> Self {
        Self {
            bytes: Vec::new(),
            maximum_bytes,
            length: 0,
            pending_start: 0,
            token_oversized: false,
            pending_oversized: false,
            pending_base: None,
            previous_base: None,
        }
    }
    /// Consumes one scalar, stopping when the observer has enough complete tokens.
    pub(crate) fn push(
        &mut self,
        character: char,
        observe: &mut impl FnMut(&str) -> Result<bool, SearchError>,
    ) -> Result<bool, SearchError> {
        if is_mark(character) {
            if self.pending_base.is_some() {
                self.push_pending(character)?;
            }
            return Ok(false);
        }
        if self.advance_pending(Some(character), observe)? {
            return Ok(true);
        }
        if self
            .previous_base
            .is_some_and(|previous| is_boundary(previous, character, None))
        {
            if self.observe(self.length, observe)? {
                return Ok(true);
            }
            self.clear();
            self.previous_base = None;
        }
        self.pending_start = self.length;
        self.pending_base = Some(character);
        self.push_pending(character)?;
        Ok(false)
    }

    /// Finishes the current word without carrying its boundary into the next word.
    pub(crate) fn finish(
        &mut self,
        observe: &mut impl FnMut(&str) -> Result<bool, SearchError>,
    ) -> Result<bool, SearchError> {
        if self.advance_pending(None, observe)? {
            return Ok(true);
        }
        let complete = self.observe(self.length, observe)?;
        self.clear();
        self.previous_base = None;
        Ok(complete)
    }

    fn advance_pending(
        &mut self,
        next: Option<char>,
        observe: &mut impl FnMut(&str) -> Result<bool, SearchError>,
    ) -> Result<bool, SearchError> {
        let Some(current) = self.pending_base.take() else {
            return Ok(false);
        };
        if !is_alphanumeric(current) {
            if self.observe(self.pending_start, observe)? {
                return Ok(true);
            }
            self.clear();
            self.previous_base = None;
        } else {
            if self.previous_base.is_some_and(|previous| {
                is_boundary(
                    previous,
                    current,
                    next.filter(|next| is_alphanumeric(*next)),
                )
            }) {
                if self.observe(self.pending_start, observe)? {
                    return Ok(true);
                }
                self.retain_pending();
                self.token_oversized = self.pending_oversized;
            } else {
                self.token_oversized |= self.pending_oversized;
            }
            self.previous_base = Some(current);
        }
        self.pending_oversized = false;
        Ok(false)
    }

    fn observe(
        &self,
        length: usize,
        observe: &mut impl FnMut(&str) -> Result<bool, SearchError>,
    ) -> Result<bool, SearchError> {
        if self.token_oversized || length == 0 {
            return Ok(false);
        }
        observe(
            std::str::from_utf8(&self.bytes[..length])
                .expect("bounded token contains only complete UTF-8 scalars"),
        )
    }

    fn push_pending(&mut self, character: char) -> Result<(), SearchError> {
        if self.pending_oversized {
            return Ok(());
        }
        let mut bytes = [0; 4];
        let encoded = character.encode_utf8(&mut bytes).as_bytes();
        if encoded.len() > self.maximum_bytes - self.length {
            if !self.token_oversized && self.pending_start <= MAX_TERM_BYTES {
                self.pending_oversized = true;
                return Ok(());
            }
            // The next base can put the pending spelling after a real boundary.
            // Keep that spelling even when the preceding token has overflowed.
            self.token_oversized = true;
            self.retain_pending();
        }
        if encoded.len() > self.maximum_bytes - self.length {
            self.pending_oversized = true;
            return Ok(());
        }
        let end = self.length + encoded.len();
        if end > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(64)
                .max(end)
                .min(self.maximum_bytes);
            self.bytes
                .try_reserve_exact(capacity - self.length)
                .map_err(|_| SearchError::BuildBudgetExceeded {
                    resource: "text_bytes",
                })?;
        }
        self.bytes.extend_from_slice(encoded);
        self.length = end;
        Ok(())
    }

    fn retain_pending(&mut self) {
        self.bytes.copy_within(self.pending_start..self.length, 0);
        self.length -= self.pending_start;
        self.bytes.truncate(self.length);
        self.pending_start = 0;
    }

    fn clear(&mut self) {
        self.length = 0;
        self.bytes.clear();
        self.pending_start = 0;
        self.token_oversized = false;
        self.pending_oversized = false;
    }
}

fn split_spans(text: &str, output: &mut Vec<Span>) {
    output.clear();
    let mut characters = text
        .char_indices()
        .filter(|(_, character)| !is_mark(*character))
        .peekable();
    let mut start = None;
    let mut previous_base = None;

    while let Some((offset, current)) = characters.next() {
        if !is_alphanumeric(current) {
            finish_span(start.take(), offset, output);
            previous_base = None;
            continue;
        }
        let Some(token_start) = start else {
            start = Some(offset);
            previous_base = Some(current);
            continue;
        };
        let next = characters
            .peek()
            .map(|(_, character)| *character)
            .filter(|character| is_alphanumeric(*character));
        if previous_base.is_some_and(|previous| is_boundary(previous, current, next)) {
            finish_span(Some(token_start), offset, output);
            start = Some(offset);
        }
        previous_base = Some(current);
    }
    finish_span(start, text.len(), output);
}

fn finish_span(start: Option<usize>, end: usize, output: &mut Vec<Span>) {
    if let Some(start) = start.filter(|start| *start < end) {
        output.push(Span { start, end });
    }
}

fn is_boundary(previous: char, current: char, next: Option<char>) -> bool {
    let alpha_digit_edge =
        previous.is_alphabetic() != current.is_alphabetic() && previous.is_alphanumeric();
    let lower_to_upper = previous.is_lowercase() && current.is_uppercase();
    let acronym_to_word =
        previous.is_uppercase() && current.is_uppercase() && next.is_some_and(char::is_lowercase);
    alpha_digit_edge || lower_to_upper || acronym_to_word
}

pub(crate) fn is_mark(character: char) -> bool {
    !character.is_ascii()
        && matches!(
            get_general_category(character),
            GeneralCategory::NonspacingMark
                | GeneralCategory::SpacingMark
                | GeneralCategory::EnclosingMark
        )
}

fn is_alphanumeric(character: char) -> bool {
    if character.is_ascii() {
        character.is_ascii_alphanumeric()
    } else {
        character.is_alphanumeric()
    }
}

#[cfg(test)]
mod tests {
    use tantivy::tokenizer::{TextAnalyzer, TokenStream, Tokenizer};

    use super::{
        BoundedCodeTokenizer, CodeTokenizer, MAX_TERM_BYTES, SearchError, Span, finish_span,
        is_boundary, is_mark, normalize_text, split_spans,
    };

    fn tokens(input: &str) -> Vec<String> {
        let mut analyzer = TextAnalyzer::from(CodeTokenizer::default());
        let mut stream = analyzer.token_stream(input);
        let mut output = Vec::new();
        stream.process(&mut |token| output.push(token.text.clone()));
        output
    }

    fn reference_spans(text: &str) -> Vec<Span> {
        let characters = text.char_indices().collect::<Vec<_>>();
        let mut output = Vec::new();
        let mut start = None;
        let mut previous_base = None;

        for (position, &(offset, current)) in characters.iter().enumerate() {
            if is_mark(current) {
                if start.is_none() {
                    previous_base = None;
                }
                continue;
            }
            if !current.is_alphanumeric() {
                finish_span(start.take(), offset, &mut output);
                previous_base = None;
                continue;
            }
            let Some(token_start) = start else {
                start = Some(offset);
                previous_base = Some(current);
                continue;
            };
            let next = characters[position + 1..]
                .iter()
                .map(|(_, character)| *character)
                .find(|character| !is_mark(*character))
                .filter(|character| character.is_alphanumeric());
            if previous_base.is_some_and(|previous| is_boundary(previous, current, next)) {
                finish_span(Some(token_start), offset, &mut output);
                start = Some(offset);
            }
            previous_base = Some(current);
        }
        finish_span(start, text.len(), &mut output);
        output
    }

    #[test]
    fn bounded_source_tokens_preserve_complete_reference_spans() {
        let mut cases = vec![
            "snake_case HTTP2Server/path.rs".to_owned(),
            "Cafe\u{301}Value Straße Σίσυφος İValue".to_owned(),
            "\u{301}leading a\u{301}\u{302}B2\u{303}c :: XML\u{301}Http".to_owned(),
            format!("{}TailMarker", "x".repeat(MAX_TERM_BYTES + 1)),
            format!("{}Bc", "A".repeat(MAX_TERM_BYTES + 1)),
            format!("a{}B2Tail", "\u{301}".repeat(MAX_TERM_BYTES + 1)),
            format!("{}bC", "A\u{301}".repeat(MAX_TERM_BYTES)),
            format!("{}A_{}Z", "x".repeat(MAX_TERM_BYTES), "İ".repeat(81)),
            format!("{}Tail", "ſ".repeat(MAX_TERM_BYTES)),
        ];
        let alphabet = ['a', 'B', 'C', '2', '_', '\u{301}', 'İ', 'λ'];
        for first in alphabet {
            for second in alphabet {
                for third in alphabet {
                    cases.push([first, second, third].into_iter().collect());
                }
            }
        }
        let mut tokenizer = BoundedCodeTokenizer::new(480);
        for source in cases {
            let expected = reference_spans(&source)
                .into_iter()
                .filter(|span| span.end - span.start <= 480)
                .map(|span| source[span.start..span.end].to_owned())
                .collect::<Vec<_>>();
            let mut actual = Vec::new();
            let mut observe = |token: &str| {
                actual.push(token.to_owned());
                Ok::<_, SearchError>(false)
            };
            for character in source.chars() {
                assert!(!tokenizer.push(character, &mut observe).unwrap());
            }
            assert!(!tokenizer.finish(&mut observe).unwrap());
            assert_eq!(actual, expected, "source={source:?}");
        }
    }

    #[test]
    fn shared_source_scratch_preserves_short_tokens_next_to_oversized_lookahead() {
        for prefix in [239, 240, 241, 243, 479, 480, 481] {
            for marks in [60, 119, 120, 121, 240, 241] {
                let source = format!("{}B{}cTail", "A".repeat(prefix), "\u{301}".repeat(marks));
                let expected = reference_spans(&source)
                    .into_iter()
                    .map(|span| source[span.start..span.end].to_owned())
                    .collect::<Vec<_>>();
                let mut tokenizer = BoundedCodeTokenizer::new(480);
                let mut actual = Vec::new();
                let mut observe = |token: &str| {
                    actual.push(token.to_owned());
                    Ok::<_, SearchError>(false)
                };
                for character in source.chars() {
                    tokenizer.push(character, &mut observe).unwrap();
                }
                tokenizer.finish(&mut observe).unwrap();
                assert_eq!(
                    actual
                        .iter()
                        .filter(|token| token.len() <= MAX_TERM_BYTES)
                        .collect::<Vec<_>>(),
                    expected
                        .iter()
                        .filter(|token| token.len() <= MAX_TERM_BYTES)
                        .collect::<Vec<_>>(),
                    "prefix={prefix}, marks={marks}"
                );
                assert!(
                    actual
                        .iter()
                        .all(|token| expected.contains(token) && token.len() <= 480)
                );
            }
        }
    }

    #[test]
    fn splits_common_code_and_path_boundaries() {
        assert_eq!(tokens("snake_case"), ["snake", "case"]);
        assert_eq!(tokens("kebab-case"), ["kebab", "case"]);
        assert_eq!(tokens("camelCase"), ["camel", "case"]);
        assert_eq!(tokens("HTTPServer"), ["http", "server"]);
        assert_eq!(
            tokens("crate::HTTP2Server/path.rs"),
            ["crate", "http", "2", "server", "path", "rs"]
        );
    }

    #[test]
    fn lowercases_unicode_without_losing_boundaries() {
        assert_eq!(tokens("CaféValue"), ["café", "value"]);
        assert_eq!(tokens("Cafe\u{301}Value"), ["café", "value"]);
        assert_eq!(tokens("Straße STRASSE"), ["strasse", "strasse"]);
        assert_eq!(tokens("Σίσυφος ςσΣ"), ["σίσυφοσ", "σσ", "σ"]);
        assert_eq!(normalize_text("ςσΣ"), "σσσ");
    }

    #[test]
    fn reused_token_buffer_preserves_folded_text_and_physical_offsets() {
        let ascii = (0_u8..=127).map(char::from).collect::<String>();
        let mut tokenizer = CodeTokenizer::default();
        for text in [
            "HTTP2Server/UPPER_lower.rs",
            "Straße Cafe\u{301}Value İΣς",
            ascii.as_str(),
            "aVeryLongMixedCaseIdentifier123 HTTP",
            "ABC",
        ] {
            let expected = reference_spans(text)
                .into_iter()
                .map(|span| {
                    (
                        span.start,
                        span.end,
                        normalize_text(&text[span.start..span.end]),
                    )
                })
                .collect::<Vec<_>>();
            let mut stream = tokenizer.token_stream(text);
            let mut actual = Vec::new();
            while stream.advance() {
                let token = stream.token();
                actual.push((token.offset_from, token.offset_to, token.text.clone()));
            }
            assert_eq!(actual, expected, "{text:?}");
        }
    }

    #[test]
    fn streaming_span_split_matches_the_original_unicode_boundaries() {
        let cases = [
            "",
            "snake_case HTTP2Server/path.rs",
            "Cafe\u{301}Value Straße Σίσυφος",
            "\u{301}\u{302}leading middle\u{301}-trailing\u{301}\u{302}",
            "a\u{301}\u{302}B2\u{303}c :: XML\u{301}Http",
            "文字列値_42 кириллицаValue",
        ];

        for text in cases {
            let mut actual = Vec::new();
            split_spans(text, &mut actual);
            assert_eq!(actual, reference_spans(text), "input: {text:?}");
        }
    }
}
