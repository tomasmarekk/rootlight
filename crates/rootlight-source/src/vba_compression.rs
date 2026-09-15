//! Bounded MS-OVBA decompression for project metadata and module source streams.
//! Chunk provenance identifies decoding dependencies, not byte-for-byte source
//! mappings; code-page decoding and repository identities belong to the caller.

use std::{fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};

const CHUNK_BYTES: usize = 4096;
const MAX_CHUNKS: usize = 65_536;

/// Independent input, expanded-output and provenance limits for one container.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct DecompressionLimits {
    /// Maximum compressed bytes, bounded by the existing source snapshot ceiling.
    pub input_bytes: usize,
    /// Maximum expanded bytes, bounded by the existing source snapshot ceiling.
    pub output_bytes: usize,
    /// Maximum independently decoded chunks, at most 65,536.
    pub chunks: usize,
}

impl Default for DecompressionLimits {
    fn default() -> Self {
        Self {
            input_bytes: 16 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
            chunks: 4096,
        }
    }
}

/// A malformed container or a caller resource/cancellation boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecompressionError {
    /// A container or chunk signature is invalid.
    #[error("invalid vba compression signature at byte {offset}")]
    Signature {
        /// Position in the supplied compressed container, not the outer file.
        offset: usize,
    },
    /// A declared chunk or token extends beyond its enclosing input.
    #[error("truncated vba compressed data at byte {offset}")]
    Truncated {
        /// Position of the incomplete record in the supplied container.
        offset: usize,
    },
    /// Chunk size, token sequence or chunk-local copy history is inconsistent.
    #[error("invalid vba compressed data at byte {offset}")]
    Invalid {
        /// Position of the invalid record in the supplied container.
        offset: usize,
    },
    /// Caller-selected limits exceed a hard ceiling.
    #[error("invalid vba decompression limits")]
    Limits,
    /// A caller-selected resource boundary was reached before further allocation.
    #[error("vba decompression {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource name, never source payload.
        resource: &'static str,
        /// Caller-selected maximum.
        maximum: usize,
    },
    /// The caller cancelled or its own deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

/// Encoding selected by a validated chunk header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkEncoding {
    /// Exactly 4,096 uncompressed bytes after the two-byte chunk header.
    Raw,
    /// Literal and chunk-local copy tokens after the two-byte chunk header.
    Tokens,
}

/// One independently decoded chunk and its complete encoded-input dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedChunk {
    encoded: Range<usize>,
    decoded: Range<usize>,
    encoding: ChunkEncoding,
}

impl DecodedChunk {
    /// Returns the header and payload range in the supplied compressed container.
    /// The single container signature byte is outside every chunk range.
    #[must_use]
    pub fn encoded_range(&self) -> Range<usize> {
        self.encoded.clone()
    }

    /// Returns the matching expanded range; this is not a byte-preserving mapping.
    #[must_use]
    pub fn decoded_range(&self) -> Range<usize> {
        self.decoded.clone()
    }

    /// Returns whether the chunk payload contains raw bytes or compression tokens.
    #[must_use]
    pub const fn encoding(&self) -> ChunkEncoding {
        self.encoding
    }
}

/// Fully decoded bytes and ordered chunks that partition the expanded output.
pub struct DecompressedContainer {
    bytes: Vec<u8>,
    chunks: Vec<DecodedChunk>,
}

impl fmt::Debug for DecompressedContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecompressedContainer")
            .field("bytes", &self.bytes.len())
            .field("chunks", &self.chunks.len())
            .finish()
    }
}

impl DecompressedContainer {
    /// Returns the exact expanded bytes, without code-page conversion or execution.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns ordered decoding dependencies; these do not assert source identity.
    #[must_use]
    pub fn chunks(&self) -> &[DecodedChunk] {
        &self.chunks
    }
}

/// Decodes one complete MS-OVBA compressed container without I/O or execution.
///
/// The input starts at the signature and ends at the containing stream's end.
/// A signature-only container represents empty input. Non-final chunks expand
/// to exactly 4,096 bytes; the last chunk expands to 1–4,096 bytes. Work is linear
/// in admitted input plus output, and cancellation is checked for every token
/// and expanded copy byte. Returned chunk ranges are relative to the supplied slice.
///
/// # Errors
/// Returns a typed error for malformed or truncated records, limits or cancellation.
/// No partial output is returned on error. Neither byte limit may exceed the
/// existing source snapshot ceiling; the chunk limit also bounds metadata.
///
/// # Examples
/// ```
/// use rootlight_cancel::Cancellation;
/// use rootlight_source::vba_compression::{decompress, DecompressionLimits};
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let decoded = decompress(&[1, 1, 0xb0, 0, b'x'], DecompressionLimits::default(), &Cancellation::new())?;
/// assert_eq!(decoded.bytes(), b"x");
/// assert_eq!(decoded.chunks()[0].encoded_range(), 1..5);
/// # Ok(())
/// # }
/// ```
pub fn decompress(
    input: &[u8],
    limits: DecompressionLimits,
    cancellation: &Cancellation,
) -> Result<DecompressedContainer, DecompressionError> {
    cancellation.check()?;
    for maximum in [limits.input_bytes, limits.output_bytes] {
        if u64::try_from(maximum).map_err(|_| DecompressionError::Limits)?
            > crate::HARD_MAX_SNAPSHOT_BYTES
        {
            return Err(DecompressionError::Limits);
        }
    }
    if limits.chunks > MAX_CHUNKS {
        return Err(DecompressionError::Limits);
    }
    if input.len() > limits.input_bytes {
        return Err(DecompressionError::Limit {
            resource: "input bytes",
            maximum: limits.input_bytes,
        });
    }
    if input.first() != Some(&1) {
        return Err(DecompressionError::Signature { offset: 0 });
    }
    let mut result = DecompressedContainer {
        bytes: Vec::new(),
        chunks: Vec::new(),
    };
    let mut position = 1;
    while position < input.len() {
        cancellation.check()?;
        if result.chunks.len() == limits.chunks {
            return Err(DecompressionError::Limit {
                resource: "chunks",
                maximum: limits.chunks,
            });
        }
        let header = read_word(input, position, position)?;
        if header & 0x7000 != 0x3000 {
            return Err(DecompressionError::Signature { offset: position });
        }
        let size = usize::from(header & 0x0fff) + 3;
        let end = position + size;
        let payload = input
            .get(position + 2..end)
            .ok_or(DecompressionError::Truncated { offset: position })?;
        let start = result.bytes.len();
        let encoding = if header & 0x8000 == 0 {
            if payload.len() != CHUNK_BYTES {
                return Err(DecompressionError::Invalid { offset: position });
            }
            admit_output(start, payload.len(), limits.output_bytes)?;
            result.bytes.extend_from_slice(payload);
            ChunkEncoding::Raw
        } else {
            decode_tokens(
                payload,
                position + 2,
                &mut result.bytes,
                limits.output_bytes,
                cancellation,
            )?;
            ChunkEncoding::Tokens
        };
        let length = result.bytes.len() - start;
        if length == 0 || (end < input.len() && length != CHUNK_BYTES) {
            return Err(DecompressionError::Invalid { offset: position });
        }
        result.chunks.push(DecodedChunk {
            encoded: position..end,
            decoded: start..result.bytes.len(),
            encoding,
        });
        position = end;
    }
    Ok(result)
}

fn read_word(input: &[u8], at: usize, offset: usize) -> Result<u16, DecompressionError> {
    let bytes = input
        .get(at..at + 2)
        .ok_or(DecompressionError::Truncated { offset })?;
    Ok(u16::from_le_bytes(
        bytes
            .try_into()
            .map_err(|_| DecompressionError::Truncated { offset })?,
    ))
}

fn admit_output(
    length: usize,
    additional: usize,
    maximum: usize,
) -> Result<(), DecompressionError> {
    if length.checked_add(additional).is_none_or(|n| n > maximum) {
        return Err(DecompressionError::Limit {
            resource: "output bytes",
            maximum,
        });
    }
    Ok(())
}

fn decode_tokens(
    payload: &[u8],
    base: usize,
    output: &mut Vec<u8>,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<(), DecompressionError> {
    let chunk_start = output.len();
    let mut position = 0;
    while let Some(flags) = payload.get(position).copied() {
        cancellation.check()?;
        position += 1;
        // MS-OVBA 2.4.1.3.4 admits a terminal flag without tokens. Its bits are
        // unused; the full encoded range still includes this byte. An entirely
        // empty decoded chunk remains invalid at the container boundary.
        for bit in 0..8 {
            let Some(literal) = payload.get(position).copied() else {
                break;
            };
            cancellation.check()?;
            let offset = base + position;
            let produced = output.len() - chunk_start;
            if produced >= CHUNK_BYTES {
                return Err(DecompressionError::Invalid { offset });
            }
            if flags & (1 << bit) == 0 {
                admit_output(output.len(), 1, maximum)?;
                output.push(literal);
                position += 1;
            } else {
                let token = read_word(payload, position, offset)?;
                if produced == 0 {
                    return Err(DecompressionError::Invalid { offset });
                }
                // MS-OVBA 2.4.1.3.19.1 changes the split after each power of two,
                // not at it. Integer ceil(log2(produced)) avoids rounding drift.
                let bits = (usize::BITS - (produced - 1).leading_zeros()).max(4);
                let mask = u16::MAX >> bits;
                let length = usize::from(token & mask) + 3;
                let distance = usize::from(token >> (16 - bits)) + 1;
                if distance > produced || length > CHUNK_BYTES - produced {
                    return Err(DecompressionError::Invalid { offset });
                }
                admit_output(output.len(), length, maximum)?;
                // Copy history grows during a token: overlapping repetition is
                // intentional, and must never read bytes from the prior chunk.
                for _ in 0..length {
                    cancellation.check()?;
                    let byte = *output
                        .get(output.len() - distance)
                        .ok_or(DecompressionError::Invalid { offset })?;
                    output.push(byte);
                }
                position += 2;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
