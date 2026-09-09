//! Bounded source-text projections for compact declaration signatures.
//! Byte mappings preserve exact written fragments; inserted separators are
//! presentation whitespace, not fabricated source tokens.

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::{LexicalExtensionError, MAX_LEXICAL_SIGNATURE_BYTES};

/// Maximum number of source fragments in one compact signature.
pub const MAX_SIGNATURE_SOURCE_PARTS: usize = 64;

/// One byte mapping from an envelope-relative source range to retained text.
///
/// Individual mappings are wire data; their ordering, bounds and UTF-8 text
/// boundaries are validated together by the lexical envelope decoder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SignatureSourcePart {
    source_start: u64,
    source_end: u64,
    text_start: usize,
    text_end: usize,
}

impl SignatureSourcePart {
    /// Returns source byte offsets relative to the envelope's direct source.
    #[must_use]
    pub fn source_range(&self) -> Range<u64> {
        self.source_start..self.source_end
    }

    /// Returns byte offsets in the compact retained text.
    #[must_use]
    pub fn text_range(&self) -> Range<usize> {
        self.text_start..self.text_end
    }
}

/// Complete compact text assembled from exact, ordered source fragments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceTextProjection {
    pub(crate) text: String,
    pub(crate) parts: Vec<SignatureSourcePart>,
}

impl SourceTextProjection {
    /// Copies exact UTF-8 source fragments, joining each pair with one space.
    ///
    /// The producer selects only meaningful source ranges. This constructor
    /// checks byte provenance, not whether omitted text is language trivia.
    /// Allocation is bounded by the existing signature limit and fragment cap.
    ///
    /// # Errors
    ///
    /// Rejects empty, overlapping, unordered, out-of-bounds or non-UTF-8 ranges,
    /// and projections exceeding either bound. No partial result is returned.
    pub fn from_source(
        source: &str,
        ranges: &[Range<usize>],
    ) -> Result<Self, LexicalExtensionError> {
        if ranges.is_empty() || ranges.len() > MAX_SIGNATURE_SOURCE_PARTS {
            return Err(LexicalExtensionError::InvalidSourceProjection);
        }
        let mut text = String::new();
        let mut parts = Vec::with_capacity(ranges.len());
        let mut previous_end = 0;
        for range in ranges {
            let fragment = source
                .get(range.clone())
                .filter(|fragment| !fragment.is_empty())
                .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
            if range.start < previous_end {
                return Err(LexicalExtensionError::InvalidSourceProjection);
            }
            let separator = usize::from(!parts.is_empty());
            let length = text
                .len()
                .checked_add(separator)
                .and_then(|size| size.checked_add(fragment.len()))
                .filter(|size| *size <= MAX_LEXICAL_SIGNATURE_BYTES)
                .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
            if separator != 0 {
                text.push(' ');
            }
            let text_start = text.len();
            text.push_str(fragment);
            parts.push(SignatureSourcePart {
                source_start: u64::try_from(range.start)
                    .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?,
                source_end: u64::try_from(range.end)
                    .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?,
                text_start,
                text_end: length,
            });
            previous_end = range.end;
        }
        validate_parts(&text, &parts)?;
        Ok(Self { text, parts })
    }

    /// Returns the complete bounded text, including presentation separators.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns exact ordered mappings for every retained source fragment.
    #[must_use]
    pub fn parts(&self) -> &[SignatureSourcePart] {
        &self.parts
    }
}

pub(crate) fn validate_parts(
    text: &str,
    parts: &[SignatureSourcePart],
) -> Result<(), LexicalExtensionError> {
    if parts.is_empty() || parts.len() > MAX_SIGNATURE_SOURCE_PARTS {
        return Err(LexicalExtensionError::InvalidSourceProjection);
    }
    let mut source_end = 0;
    let mut text_end = 0;
    for (index, part) in parts.iter().enumerate() {
        let expected_start = text_end + usize::from(index != 0);
        if part.source_start < source_end
            || part.source_start >= part.source_end
            || part.text_start != expected_start
            || part.text_start >= part.text_end
            || text.get(part.text_range()).is_none()
            || (index != 0 && text.get(text_end..expected_start) != Some(" "))
            || u64::try_from(part.text_end - part.text_start).ok()
                != Some(part.source_end - part.source_start)
        {
            return Err(LexicalExtensionError::InvalidSourceProjection);
        }
        source_end = part.source_end;
        text_end = part.text_end;
    }
    if text_end != text.len() {
        return Err(LexicalExtensionError::InvalidSourceProjection);
    }
    Ok(())
}
