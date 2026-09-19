//! Strict declared-codepage text views with original module-byte coordinates.
//! Sparse checkpoints map parser UTF-8 boundaries back to MBCS bytes; decoding
//! never normalizes characters or replaces malformed/unsupported input.

use std::{borrow::Cow, fmt, ops::Range, str};

use rootlight_cancel::{Cancellation, Cancelled};

#[cfg(test)]
mod tests;

const CHECKPOINT_STRIDE: usize = 256;
const MAX_POINTS: usize = 64 * 1024 * 1024 / CHECKPOINT_STRIDE + 1;

/// Limits for one decoded module or project-name view, not aggregate retention.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct TextLimits {
    /// Maximum original input bytes, at most the source snapshot ceiling.
    pub input_bytes: usize,
    /// Maximum UTF-8 output bytes, at most the source snapshot ceiling.
    pub output_bytes: usize,
    /// Maximum retained mapping checkpoints; identity views require none.
    pub mapping_points: usize,
}

impl Default for TextLimits {
    fn default() -> Self {
        Self {
            input_bytes: 16 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
            mapping_points: 16 * 1024 * 1024 / CHECKPOINT_STRIDE + 1,
        }
    }
}

/// An explicit decoding failure, with no source payload in diagnostics.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TextError {
    /// No reviewed decoder exists for the declared page; no fallback was used.
    #[error("unsupported vba code page: {code_page}")]
    Unsupported {
        /// Original PROJECTCODEPAGE value, not a machine-default codepage.
        code_page: u16,
    },
    /// A byte sequence is invalid or undefined in the declared page.
    #[error("invalid vba code page {code_page} sequence at byte {offset}")]
    Invalid {
        /// The declared page used for validation.
        code_page: u16,
        /// Original byte offset of the invalid sequence.
        offset: usize,
    },
    /// Caller-supplied limits exceed hard ceilings.
    #[error("invalid vba text limits")]
    Limits,
    /// A resource ceiling was reached before returning a partial text view.
    #[error("vba text {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource label, never source content.
        resource: &'static str,
        /// Caller-supplied maximum.
        maximum: usize,
    },
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// A complete validated text view and its immutable original byte coordinates.
/// UTF-8 and ASCII Windows-1252 borrow their input; transcoded text is owned.
pub struct DecodedText<'a> {
    original: &'a [u8],
    text: Cow<'a, str>,
    code_page: u16,
    checkpoints: Vec<usize>,
}

impl fmt::Debug for DecodedText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodedText")
            .field("code_page", &self.code_page)
            .field("original_bytes", &self.original.len())
            .field("utf8_bytes", &self.text.len())
            .field("mapping_points", &self.checkpoints.len())
            .finish()
    }
}

impl<'a> DecodedText<'a> {
    /// Validates a complete input under explicit Windows-1252 or UTF-8 (65001).
    ///
    /// Undefined Windows-1252 bytes are errors, not replacement characters or
    /// guessed C1 controls. Unknown pages, including machine-default page zero,
    /// never fall back even when the input happens to be ASCII. BOMs, NULs, line
    /// endings, and normalization forms remain unchanged; source classification
    /// and language semantics belong to the caller.
    ///
    /// # Errors
    /// Returns [`TextError`] for unsupported pages, invalid sequences, invalid or
    /// exhausted resource limits, or cancellation. No partial view is returned.
    pub fn decode(
        original: &'a [u8],
        code_page: u16,
        limits: TextLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, TextError> {
        cancellation.check()?;
        for limit in [limits.input_bytes, limits.output_bytes] {
            if u64::try_from(limit).map_err(|_| TextError::Limits)? > crate::HARD_MAX_SNAPSHOT_BYTES
            {
                return Err(TextError::Limits);
            }
        }
        if limits.mapping_points > MAX_POINTS {
            return Err(TextError::Limits);
        }
        if !matches!(code_page, 1252 | 65001) {
            return Err(TextError::Unsupported { code_page });
        }
        limit(original.len(), limits.input_bytes, "input bytes")?;
        if code_page == 65001 || original.is_ascii() {
            limit(original.len(), limits.output_bytes, "output bytes")?;
            let text = str::from_utf8(original).map_err(|error| TextError::Invalid {
                code_page,
                offset: error.valid_up_to(),
            })?;
            cancellation.check()?;
            return Ok(Self {
                original,
                text: Cow::Borrowed(text),
                code_page,
                checkpoints: Vec::new(),
            });
        }
        let mut text = String::new();
        let mut checkpoints = Vec::new();
        for (offset, byte) in original.iter().copied().enumerate() {
            if offset % CHECKPOINT_STRIDE == 0 {
                cancellation.check()?;
                limit(
                    checkpoints.len() + 1,
                    limits.mapping_points,
                    "mapping points",
                )?;
                checkpoints.push(text.len());
            }
            let character = windows_1252(byte).ok_or(TextError::Invalid { code_page, offset })?;
            let length = text
                .len()
                .checked_add(character.len_utf8())
                .ok_or(TextError::Limits)?;
            limit(length, limits.output_bytes, "output bytes")?;
            text.push(character);
        }
        cancellation.check()?;
        Ok(Self {
            original,
            text: Cow::Owned(text),
            code_page,
            checkpoints,
        })
    }

    /// Returns exact Unicode text without normalization or BOM stripping.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the unchanged original MBCS or UTF-8 input.
    #[must_use]
    pub const fn original_bytes(&self) -> &'a [u8] {
        self.original
    }

    /// Returns the explicitly validated codepage, not a detected encoding guess.
    #[must_use]
    pub const fn code_page(&self) -> u16 {
        self.code_page
    }

    /// Returns the number of sparse original-to-UTF-8 checkpoints retained.
    #[must_use]
    pub fn mapping_points(&self) -> usize {
        self.checkpoints.len()
    }

    /// Maps a complete UTF-8 scalar-boundary range to original input byte offsets.
    ///
    /// Empty boundary ranges are supported. Reversed, out-of-bounds and split
    /// scalar ranges return `None`; no widening or rounding invents provenance.
    /// Each endpoint scans at most 256 decoded scalars from a sparse checkpoint.
    /// These offsets address decoded module bytes, not compressed/container bytes.
    #[must_use]
    pub fn original_range(&self, utf8_range: Range<usize>) -> Option<Range<usize>> {
        if utf8_range.start > utf8_range.end {
            return None;
        }
        Some(self.original_offset(utf8_range.start)?..self.original_offset(utf8_range.end)?)
    }

    fn original_offset(&self, offset: usize) -> Option<usize> {
        if !self.text.is_char_boundary(offset) {
            return None;
        }
        if self.checkpoints.is_empty() {
            return Some(offset);
        }
        let index = self
            .checkpoints
            .partition_point(|point| *point <= offset)
            .checked_sub(1)?;
        let start = *self.checkpoints.get(index)?;
        let scalars = self.text.get(start..offset)?.chars().count();
        index.checked_mul(CHECKPOINT_STRIDE)?.checked_add(scalars)
    }
}

fn limit(actual: usize, maximum: usize, resource: &'static str) -> Result<(), TextError> {
    if actual > maximum {
        Err(TextError::Limit { resource, maximum })
    } else {
        Ok(())
    }
}

fn windows_1252(byte: u8) -> Option<char> {
    // Windows-1252 differs from Latin-1 in this interval. The five undefined
    // slots must not silently become control characters: Unicode CP1252.TXT,
    // https://www.unicode.org/Public/MAPPINGS/VENDORS/MICSFT/WINDOWS/CP1252.TXT
    let scalar = match byte {
        0x80 => 0x20ac,
        0x81 | 0x8d | 0x8f | 0x90 | 0x9d => return None,
        0x82 => 0x201a,
        0x83 => 0x0192,
        0x84 => 0x201e,
        0x85 => 0x2026,
        0x86 => 0x2020,
        0x87 => 0x2021,
        0x88 => 0x02c6,
        0x89 => 0x2030,
        0x8a => 0x0160,
        0x8b => 0x2039,
        0x8c => 0x0152,
        0x8e => 0x017d,
        0x91 => 0x2018,
        0x92 => 0x2019,
        0x93 => 0x201c,
        0x94 => 0x201d,
        0x95 => 0x2022,
        0x96 => 0x2013,
        0x97 => 0x2014,
        0x98 => 0x02dc,
        0x99 => 0x2122,
        0x9a => 0x0161,
        0x9b => 0x203a,
        0x9c => 0x0153,
        0x9e => 0x017e,
        0x9f => 0x0178,
        _ => u32::from(byte),
    };
    char::from_u32(scalar)
}
