use tantivy::tokenizer::{Token, TokenStream, Tokenizer};
use unicode_casefold::UnicodeCaseFold as _;
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use crate::model::MAX_TERM_BYTES;

/// Splits code spelling at separators, case transitions, and letter/digit edges.
#[derive(Clone, Default)]
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
        self.token
            .text
            .push_str(&normalize_text(&self.text[span.start..span.end]));
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

/// Applies the index format's full, non-Turkic Unicode normalization.
pub(crate) fn normalize_text(input: &str) -> String {
    if input.is_ascii() {
        input.to_ascii_lowercase()
    } else {
        input.nfd().case_fold().nfc().collect()
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

fn is_mark(character: char) -> bool {
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
    use tantivy::tokenizer::{TextAnalyzer, TokenStream};

    use super::{
        CodeTokenizer, Span, finish_span, is_boundary, is_mark, normalize_text, split_spans,
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
