//! Coordinate-preserving native expression envelopes for authored host braces.
//! Only the two outer delimiters change in the parser callback; source evidence,
//! UTF-8 bytes, line coordinates and all interior tokens remain original.

use super::{AdapterError, CANCELLATION_CHECK_INTERVAL, Cancellation, provider_failure};

#[derive(Debug, Clone, Copy)]
pub(super) struct ExpressionEnvelope {
    open: usize,
    close: usize,
}

impl ExpressionEnvelope {
    pub(super) fn new(source: &[u8], start: usize, end: usize) -> Option<Self> {
        let close = end.checked_sub(1)?;
        (start < close && source.get(start) == Some(&b'{') && source.get(close) == Some(&b'}'))
            .then_some(Self { open: start, close })
    }

    pub(super) fn contains_only_comments(
        self,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let interior = self
            .open
            .checked_add(1)
            .and_then(|start| source.get(start..self.close))
            .ok_or_else(|| provider_failure("expression-comment-range"))?;
        let text = std::str::from_utf8(interior)
            .map_err(|_| provider_failure("expression-comment-encoding"))?;
        let mut state = CommentState::Trivia;
        let mut comment_seen = false;
        // This recognizes trivia only, not JavaScript expressions. Retained
        // braces are still validated by the native grammar; no errors are hidden.
        for (index, character) in text.chars().enumerate() {
            if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                cancellation.check()?;
            }
            state = match (state, character) {
                (CommentState::Trivia, '/') => CommentState::Slash,
                (CommentState::Trivia, character) if javascript_space(character) => {
                    CommentState::Trivia
                }
                (CommentState::Slash, '/') => {
                    comment_seen = true;
                    CommentState::Line
                }
                (CommentState::Slash, '*') => {
                    comment_seen = true;
                    CommentState::Block
                }
                (CommentState::Line, '\n' | '\r' | '\u{2028}' | '\u{2029}') => CommentState::Trivia,
                (CommentState::Line, _) => CommentState::Line,
                (CommentState::Block | CommentState::BlockStar, '*') => CommentState::BlockStar,
                (CommentState::BlockStar, '/') => CommentState::Trivia,
                (CommentState::Block | CommentState::BlockStar, _) => CommentState::Block,
                (CommentState::Trivia | CommentState::Slash, _) => return Ok(false),
            };
        }
        Ok(comment_seen && matches!(state, CommentState::Trivia))
    }

    pub(super) fn chunk(self, source: &[u8], offset: usize, end: usize) -> &[u8] {
        if offset >= end {
            return &[];
        }
        if offset == self.open {
            return b"(";
        }
        if offset == self.close {
            return b")";
        }
        let boundary = if offset < self.open {
            self.open
        } else if offset < self.close {
            self.close
        } else {
            end
        };
        source.get(offset..end.min(boundary)).unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
enum CommentState {
    Trivia,
    Slash,
    Line,
    Block,
    BlockStar,
}

fn javascript_space(character: char) -> bool {
    // ECMAScript whitespace differs from Rust's Unicode whitespace (notably
    // BOM and NEXT LINE); keep this aligned with the host scanner's js_space.
    matches!(
        character,
        '\t' | '\u{b}'
            | '\u{c}'
            | ' '
            | '\n'
            | '\r'
            | '\u{a0}'
            | '\u{feff}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_detection_preserves_lexical_boundaries_and_rejects_code() {
        let cancellation = Cancellation::new();
        for (source, expected) in [
            ("{/* ignored */}", true),
            ("{/***/ /* é雪 { } */ // tail\r\n}", true),
            (
                "{\u{feff}\u{1680}\u{2000}\u{202f}\u{205f}\u{3000}/* x */}",
                true,
            ),
            ("{// tail\u{2028}// other\u{2029}/* x */}", true),
            ("{}", false),
            ("{ \r\n }", false),
            ("{/* x */ call()}", false),
            ("{call() /* x */}", false),
            ("{/* x */ const value = 1}", false),
            ("{/* unclosed}", false),
            ("{/* outer /* inner */ value}", false),
            ("{// no line ending}", false),
            ("{/regex/}", false),
            ("{'/* string */'}", false),
            ("{`/* template */`}", false),
            ("{\u{85}/* not ECMAScript whitespace */}", false),
            ("{/* x */ /}", false),
        ] {
            let bytes = source.as_bytes();
            let envelope = ExpressionEnvelope::new(bytes, 0, bytes.len()).unwrap();
            assert_eq!(
                envelope
                    .contains_only_comments(bytes, &cancellation)
                    .unwrap(),
                expected,
                "{source}"
            );
        }
        let bytes = b"{/*\xff*/}";
        let envelope = ExpressionEnvelope::new(bytes, 0, bytes.len()).unwrap();
        assert!(
            matches!(envelope.contains_only_comments(bytes, &cancellation),
            Err(AdapterError::ProviderFailed { code }) if code.as_str() == "expression-comment-encoding")
        );
    }

    #[test]
    fn comment_detection_honors_cancellation_before_scanning() {
        let source = b"{/* comment */}";
        let envelope = ExpressionEnvelope::new(source, 0, source.len()).unwrap();
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(
            envelope
                .contains_only_comments(source, &cancellation)
                .is_err()
        );
    }

    #[test]
    fn every_chunk_boundary_preserves_interior_and_original_coordinates() {
        let source = "é\r\n{({label: '雪', call: render(/}/)})}\r\nend".as_bytes();
        let start = source.iter().position(|byte| *byte == b'{').unwrap();
        let end = source.iter().rposition(|byte| *byte == b'}').unwrap() + 1;
        let envelope = ExpressionEnvelope::new(source, start, end).unwrap();
        let mut expected = source.to_vec();
        expected[start] = b'(';
        expected[end - 1] = b')';
        for size in 1..=source.len() + 1 {
            let mut actual = Vec::new();
            while actual.len() < source.len() {
                let offset = actual.len();
                let chunk = envelope.chunk(source, offset, (offset + size).min(source.len()));
                assert!(!chunk.is_empty());
                actual.extend_from_slice(chunk);
            }
            assert_eq!(actual, expected);
        }
        for offset in 0..=source.len() {
            let chunk = envelope.chunk(source, offset, source.len());
            assert_eq!(chunk, &expected[offset..offset + chunk.len()]);
        }
        assert!(ExpressionEnvelope::new(source, 0, source.len()).is_none());
        assert!(ExpressionEnvelope::new(source, usize::MAX, usize::MAX).is_none());
        assert!(ExpressionEnvelope::new(source, 0, 0).is_none());
    }
}
