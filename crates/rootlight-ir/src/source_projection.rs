//! Bounded source-text projections for compact declaration signatures.
//! Byte mappings preserve exact written fragments; inserted separators are
//! presentation whitespace, not fabricated source tokens.

use std::{fmt, ops::Range};

use serde::{Deserialize, Serialize};

use crate::{LexicalExtensionError, MAX_LEXICAL_SIGNATURE_BYTES};

/// Maximum number of source fragments in one compact signature.
pub const MAX_SIGNATURE_SOURCE_PARTS: usize = 64;

/// Explicit physical UTF-16 encoding for a transcoded signature projection.
///
/// Declaration slices need not contain a BOM. The caller establishes this
/// encoding from its original full-file source before selecting fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SourceTextEncoding {
    /// UTF-16 code units stored least-significant byte first.
    #[serde(rename = "utf-16le")]
    Utf16Le,
    /// UTF-16 code units stored most-significant byte first.
    #[serde(rename = "utf-16be")]
    Utf16Be,
}

impl SourceTextEncoding {
    /// Returns the canonical physical source-encoding label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }
}

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

/// Complete UTF-8 presentation text mapped to original UTF-16 byte fragments.
///
/// Unlike [`SourceTextProjection`], original and retained byte widths may differ.
/// Every selected fragment is strictly decoded without replacement characters;
/// omitted bytes are not re-decoded for every declaration. Full-file encoding
/// validation remains the caller's responsibility.
#[derive(Clone, PartialEq, Eq)]
pub struct EncodedSourceTextProjection {
    pub(crate) text: String,
    pub(crate) parts: Vec<SignatureSourcePart>,
    pub(crate) encoding: SourceTextEncoding,
}

impl EncodedSourceTextProjection {
    /// Decodes exact ordered source fragments, joining them with one space.
    ///
    /// `source` is the original direct-source slice; `ranges` are relative raw
    /// byte ranges, not offsets in a separately decoded snapshot. Input storage
    /// is borrowed and allocation is capped before fragment decoding begins.
    /// A BOM is neither required nor automatically removed from selected text.
    ///
    /// # Errors
    ///
    /// Rejects odd-length source slices, invalid or unaligned ranges, malformed
    /// selected UTF-16 scalars, bounded allocation failures, and signatures
    /// exceeding the existing text or fragment limits. No partial result is
    /// returned and unselected source bytes are not claimed as validated.
    pub fn from_source(
        source: &[u8],
        encoding: SourceTextEncoding,
        ranges: &[Range<usize>],
    ) -> Result<Self, LexicalExtensionError> {
        if !source.len().is_multiple_of(2)
            || ranges.is_empty()
            || ranges.len() > MAX_SIGNATURE_SOURCE_PARTS
        {
            return Err(LexicalExtensionError::InvalidSourceProjection);
        }
        let mut text = String::new();
        text.try_reserve_exact(MAX_LEXICAL_SIGNATURE_BYTES)
            .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?;
        let mut parts = Vec::new();
        parts
            .try_reserve_exact(ranges.len())
            .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?;
        let mut previous_end = 0;
        let mut source_bytes = 0_usize;
        for range in ranges {
            if range.start < previous_end
                || range.start >= range.end
                || !range.start.is_multiple_of(2)
                || !range.end.is_multiple_of(2)
            {
                return Err(LexicalExtensionError::InvalidSourceProjection);
            }
            let fragment = source
                .get(range.clone())
                .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
            // A UTF-16 scalar needs at most twice its UTF-8 presentation width.
            // Bound raw scanning too, even when the omitted direct source is huge.
            source_bytes = source_bytes
                .checked_add(fragment.len())
                .filter(|size| *size <= 2 * MAX_LEXICAL_SIGNATURE_BYTES)
                .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
            if !parts.is_empty() {
                if text.len() == MAX_LEXICAL_SIGNATURE_BYTES {
                    return Err(LexicalExtensionError::InvalidSourceProjection);
                }
                text.push(' ');
            }
            let text_start = text.len();
            visit_utf16_scalars(fragment, encoding, |character| {
                text.len()
                    .checked_add(character.len_utf8())
                    .filter(|size| *size <= MAX_LEXICAL_SIGNATURE_BYTES)
                    .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
                text.push(character);
                Ok(())
            })?;
            parts.push(SignatureSourcePart {
                source_start: u64::try_from(range.start)
                    .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?,
                source_end: u64::try_from(range.end)
                    .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?,
                text_start,
                text_end: text.len(),
            });
            previous_end = range.end;
        }
        validate_encoded_parts(&text, &parts)?;
        Ok(Self {
            text,
            parts,
            encoding,
        })
    }

    /// Returns complete bounded UTF-8 text, including presentation separators.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns ordered original-byte mappings relative to the direct source.
    #[must_use]
    pub fn parts(&self) -> &[SignatureSourcePart] {
        &self.parts
    }

    /// Returns the physical encoding used to decode the selected source bytes.
    #[must_use]
    pub const fn encoding(&self) -> SourceTextEncoding {
        self.encoding
    }
}

impl fmt::Debug for EncodedSourceTextProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncodedSourceTextProjection")
            .field("text_bytes", &self.text.len())
            .field("parts", &self.parts)
            .field("encoding", &self.encoding)
            .finish()
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

pub(crate) fn validate_encoded_parts(
    text: &str,
    parts: &[SignatureSourcePart],
) -> Result<(), LexicalExtensionError> {
    if parts.is_empty() || parts.len() > MAX_SIGNATURE_SOURCE_PARTS {
        return Err(LexicalExtensionError::InvalidSourceProjection);
    }
    let mut source_end = 0;
    let mut text_end: usize = 0;
    for (index, part) in parts.iter().enumerate() {
        let expected_start = text_end
            .checked_add(usize::from(index != 0))
            .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
        let fragment = text
            .get(part.text_range())
            .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
        let source_width = fragment
            .encode_utf16()
            .count()
            .checked_mul(2)
            .and_then(|width| u64::try_from(width).ok());
        if part.source_start < source_end
            || part.source_start >= part.source_end
            || !part.source_start.is_multiple_of(2)
            || !part.source_end.is_multiple_of(2)
            || part.text_start != expected_start
            || part.text_start >= part.text_end
            || (index != 0 && text.get(text_end..expected_start) != Some(" "))
            || source_width != Some(part.source_end - part.source_start)
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

pub(crate) fn verify_encoded_parts(
    source: &[u8],
    text: &str,
    parts: &[SignatureSourcePart],
    encoding: SourceTextEncoding,
) -> Result<(), LexicalExtensionError> {
    if !source.len().is_multiple_of(2) {
        return Err(LexicalExtensionError::SourceMismatch);
    }
    for part in parts {
        let range = part.source_range();
        let start = usize::try_from(range.start)
            .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?;
        let end = usize::try_from(range.end)
            .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?;
        let fragment = source
            .get(start..end)
            .ok_or(LexicalExtensionError::SourceMismatch)?;
        let mut expected = text
            .get(part.text_range())
            .ok_or(LexicalExtensionError::InvalidSourceProjection)?
            .chars();
        visit_utf16_scalars(fragment, encoding, |character| {
            if expected.next() != Some(character) {
                return Err(LexicalExtensionError::SourceMismatch);
            }
            Ok(())
        })
        .map_err(|_| LexicalExtensionError::SourceMismatch)?;
        if expected.next().is_some() {
            return Err(LexicalExtensionError::SourceMismatch);
        }
    }
    Ok(())
}

fn visit_utf16_scalars(
    source: &[u8],
    encoding: SourceTextEncoding,
    mut visit: impl FnMut(char) -> Result<(), LexicalExtensionError>,
) -> Result<(), LexicalExtensionError> {
    if !source.len().is_multiple_of(2) {
        return Err(LexicalExtensionError::InvalidSourceProjection);
    }
    let units = source.as_chunks::<2>().0.iter().map(|pair| match encoding {
        SourceTextEncoding::Utf16Le => u16::from_le_bytes(*pair),
        SourceTextEncoding::Utf16Be => u16::from_be_bytes(*pair),
    });
    for character in char::decode_utf16(units) {
        visit(character.map_err(|_| LexicalExtensionError::InvalidSourceProjection)?)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded_source(text: &str, encoding: SourceTextEncoding) -> Vec<u8> {
        text.encode_utf16()
            .flat_map(|unit| match encoding {
                SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
            })
            .collect()
    }

    #[test]
    fn encoded_projection_preserves_raw_unicode_crlf_and_comment_fragments() {
        let source = "\u{feff}fn λ\r\n <# 雪 comment #> (😀)";
        let tail = source.find("(😀)").unwrap();
        let first_end = source.find(" <#").unwrap();
        let ranges = [
            2..2 * source[..first_end].encode_utf16().count(),
            2 * source[..tail].encode_utf16().count()..2 * source.encode_utf16().count(),
        ];
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let raw = encoded_source(source, encoding);
            let projection = EncodedSourceTextProjection::from_source(&raw, encoding, &ranges)
                .expect("valid encoded fragments project without source-coordinate substitution");
            assert_eq!(projection.text(), "fn λ\r\n (😀)");
            assert_eq!(projection.encoding(), encoding);
            assert_eq!(projection.parts().len(), 2);
            for (part, range) in projection.parts().iter().zip(&ranges) {
                assert_eq!(
                    part.source_range(),
                    u64::try_from(range.start).unwrap()..u64::try_from(range.end).unwrap(),
                );
            }
            assert!(!format!("{projection:?}").contains("fn λ"));
        }
    }

    #[test]
    fn encoded_projection_accepts_declared_bomless_slices_without_stripping_tokens() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let raw = encoded_source("\u{feff}fn λ", encoding);
            let whole = EncodedSourceTextProjection::from_source(
                &raw,
                encoding,
                std::slice::from_ref(&(0..raw.len())),
            )
            .unwrap();
            assert_eq!(whole.text(), "\u{feff}fn λ");
            let declaration = &raw[2..];
            let projected = EncodedSourceTextProjection::from_source(
                declaration,
                encoding,
                std::slice::from_ref(&(0..declaration.len())),
            )
            .unwrap();
            assert_eq!(projected.text(), "fn λ");
        }
    }

    #[test]
    fn encoded_projection_rejects_malformed_fragments_and_unaligned_ranges() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            for units in [vec![0xd800], vec![0xdc00], vec![0xd800, 0x0061]] {
                let raw: Vec<_> = units
                    .into_iter()
                    .flat_map(|unit: u16| match encoding {
                        SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                        SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
                    })
                    .collect();
                assert!(
                    EncodedSourceTextProjection::from_source(
                        &raw,
                        encoding,
                        std::slice::from_ref(&(0..raw.len())),
                    )
                    .is_err()
                );
            }
            let raw = encoded_source("😀 token", encoding);
            for ranges in [
                vec![],
                std::iter::once(0..0).collect(),
                std::iter::once(0..2).collect(),
                std::iter::once(2..4).collect(),
                std::iter::once(1..4).collect(),
                std::iter::once(0..3).collect(),
                std::iter::once(0..999).collect(),
                vec![0..4, 2..6],
                vec![4..6, 0..4],
            ] {
                assert!(EncodedSourceTextProjection::from_source(&raw, encoding, &ranges).is_err());
            }
            let mut odd_tail = encoded_source("valid", encoding);
            odd_tail.push(0);
            assert!(
                EncodedSourceTextProjection::from_source(
                    &odd_tail,
                    encoding,
                    std::slice::from_ref(&(0..2)),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn encoded_projection_keeps_existing_text_and_fragment_caps() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let exact = encoded_source(&"x".repeat(MAX_LEXICAL_SIGNATURE_BYTES), encoding);
            let projected = EncodedSourceTextProjection::from_source(
                &exact,
                encoding,
                std::slice::from_ref(&(0..exact.len())),
            )
            .unwrap();
            assert_eq!(projected.text().len(), MAX_LEXICAL_SIGNATURE_BYTES);
            let over = encoded_source(&"x".repeat(MAX_LEXICAL_SIGNATURE_BYTES + 1), encoding);
            assert!(
                EncodedSourceTextProjection::from_source(
                    &over,
                    encoding,
                    std::slice::from_ref(&(0..over.len())),
                )
                .is_err()
            );
            let expansion =
                encoded_source(&"雪".repeat(MAX_LEXICAL_SIGNATURE_BYTES / 3 + 1), encoding);
            assert!(
                EncodedSourceTextProjection::from_source(
                    &expansion,
                    encoding,
                    std::slice::from_ref(&(0..expansion.len())),
                )
                .is_err()
            );
            let many = encoded_source(&"x".repeat(MAX_SIGNATURE_SOURCE_PARTS + 1), encoding);
            let ranges: Vec<_> = (0..=MAX_SIGNATURE_SOURCE_PARTS)
                .map(|index| index * 2..index * 2 + 2)
                .collect();
            assert!(EncodedSourceTextProjection::from_source(&many, encoding, &ranges).is_err());
            let exact_parts = EncodedSourceTextProjection::from_source(
                &many,
                encoding,
                &ranges[..MAX_SIGNATURE_SOURCE_PARTS],
            )
            .unwrap();
            assert_eq!(exact_parts.parts().len(), MAX_SIGNATURE_SOURCE_PARTS);
            assert_eq!(exact_parts.text().len(), 2 * MAX_SIGNATURE_SOURCE_PARTS - 1);
        }
    }
}
