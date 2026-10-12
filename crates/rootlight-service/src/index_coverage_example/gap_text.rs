//! Collects bounded exact gap text from already verified source-read chunks.
//! The diagnostic never rereads live files for excerpts, so extracted modules
//! and ordinary files share the same generation-bound evidence path.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
};

use serde_json::{Value, json};

const MAX_SPAN_BYTES: usize = 256;
const MAX_FILE_BYTES: usize = 256 * 1024;

/// Bounded text selected from complete, identity-verified source chunks.
#[derive(Debug)]
pub(super) struct GapTexts {
    selected: BTreeMap<(usize, usize), Vec<u8>>,
    utf8: bool,
    file_length: usize,
    consumed: usize,
}

impl GapTexts {
    /// Selects exact spans in source order; omission reasons remain reportable.
    pub(super) fn new(
        file_length: usize,
        utf8: bool,
        spans: impl IntoIterator<Item = (usize, usize)>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut selected = BTreeMap::new();
        let mut retained = 0_usize;
        for (start, end) in spans.into_iter().collect::<BTreeSet<_>>() {
            let length = end
                .checked_sub(start)
                .filter(|_| end <= file_length)
                .ok_or("gap source range exceeds indexed file")?;
            if utf8 && length <= MAX_SPAN_BYTES && length <= MAX_FILE_BYTES - retained {
                selected.insert((start, end), Vec::new());
                retained += length;
            }
        }
        Ok(Self {
            selected,
            utf8,
            file_length,
            consumed: 0,
        })
    }

    /// Consumes contiguous verified bytes without retaining the complete file.
    pub(super) fn capture(&mut self, start: usize, bytes: &[u8]) -> Result<(), Box<dyn Error>> {
        let end = start
            .checked_add(bytes.len())
            .filter(|end| *end <= self.file_length)
            .ok_or("source chunk exceeds indexed file")?;
        if start != self.consumed {
            return Err("gap source chunks are not contiguous".into());
        }
        // An admitted span can overlap this chunk only if it starts fewer than
        // MAX_SPAN_BYTES before it. Range lookup avoids rescanning every gap.
        for (&(left, right), target) in self
            .selected
            .range_mut((start.saturating_sub(MAX_SPAN_BYTES), 0)..(end, 0))
        {
            let from = left.max(start);
            let to = right.min(end);
            if from >= to {
                continue;
            }
            if target.len() != from - left {
                return Err("gap source bytes are incomplete or repeated".into());
            }
            target.extend_from_slice(
                bytes
                    .get(from - start..to - start)
                    .ok_or("gap chunk range is invalid")?,
            );
        }
        self.consumed = end;
        Ok(())
    }

    /// Returns complete UTF-8 text or an explicit encoding/size omission reason.
    pub(super) fn evidence(&self, start: usize, end: usize) -> Result<Value, Box<dyn Error>> {
        if self.consumed != self.file_length {
            return Err("gap text requested before complete source verification".into());
        }
        let length = end
            .checked_sub(start)
            .filter(|_| end <= self.file_length)
            .ok_or("gap source range exceeds indexed file")?;
        let status = if !self.utf8 {
            "non_utf8_encoding"
        } else if length > MAX_SPAN_BYTES {
            "span_limit"
        } else if let Some(bytes) = self.selected.get(&(start, end)) {
            if bytes.len() != length {
                return Err("gap text is incomplete".into());
            }
            match std::str::from_utf8(bytes) {
                Ok(text) => return Ok(json!({"status":"complete", "text":text})),
                Err(_) => "invalid_utf8_span",
            }
        } else {
            "file_limit"
        };
        Ok(
            json!({"status":status, "maximum_span_bytes":MAX_SPAN_BYTES, "maximum_file_bytes":MAX_FILE_BYTES}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_and_cross_chunk_spans_preserve_exact_utf8() {
        let bytes = "prefix λ𝄞 suffix".as_bytes();
        let mut text = GapTexts::new(
            bytes.len(),
            true,
            [(0, 0), (7, 13), (7, 13), (9, 13), (8, 13)],
        )
        .unwrap();
        for (i, chunk) in bytes.chunks(4).enumerate() {
            text.capture(i * 4, chunk).unwrap();
        }
        assert_eq!(
            text.evidence(7, 13).unwrap(),
            json!({"status":"complete", "text":"λ𝄞"})
        );
        assert_eq!(text.evidence(9, 13).unwrap()["text"], "𝄞");
        assert_eq!(text.evidence(0, 0).unwrap()["text"], "");
        assert_eq!(text.evidence(8, 13).unwrap()["status"], "invalid_utf8_span");
    }

    #[test]
    fn limits_are_explicit_and_deduplicated_without_truncated_text() {
        let bytes = vec![b'x'; MAX_FILE_BYTES + MAX_SPAN_BYTES];
        let spans = (0..bytes.len())
            .step_by(MAX_SPAN_BYTES)
            .map(|start| (start, start + MAX_SPAN_BYTES));
        let mut text = GapTexts::new(
            bytes.len(),
            true,
            spans.chain([(0, MAX_SPAN_BYTES), (0, MAX_SPAN_BYTES + 1)]),
        )
        .unwrap();
        text.capture(0, &bytes).unwrap();
        assert_eq!(
            text.evidence(0, MAX_SPAN_BYTES).unwrap()["text"]
                .as_str()
                .unwrap()
                .len(),
            MAX_SPAN_BYTES
        );
        assert_eq!(
            text.evidence(0, MAX_SPAN_BYTES + 1).unwrap()["status"],
            "span_limit"
        );
        let omitted = text.evidence(MAX_FILE_BYTES, bytes.len()).unwrap();
        assert_eq!(omitted["status"], "file_limit");
        assert!(omitted.get("text").is_none());
        let mut binary = GapTexts::new(2, false, [(0, 2)]).unwrap();
        binary.capture(0, b"ok").unwrap();
        assert_eq!(
            binary.evidence(0, 2).unwrap()["status"],
            "non_utf8_encoding"
        );
    }

    #[test]
    fn invalid_or_incomplete_chunk_sequences_fail_closed() {
        assert!(GapTexts::new(4, true, [(1, 5)]).is_err());
        assert!(GapTexts::new(4, true, [(3, 1)]).is_err());
        let mut text = GapTexts::new(4, true, [(1, 3)]).unwrap();
        assert!(text.evidence(1, 3).is_err());
        assert!(text.capture(1, b"a").is_err());
        assert!(text.capture(0, b"abcde").is_err());
        text.capture(0, b"ab").unwrap();
        assert!(text.capture(0, b"ab").is_err());
        assert!(text.evidence(1, 3).is_err());
        text.capture(2, b"cd").unwrap();
        assert_eq!(text.evidence(1, 3).unwrap()["text"], "bc");
    }
}
