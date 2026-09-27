//! Budgeted ZIP expansion over the source library's validated archive records.
//! Expanded bytes retain their archive hash and physical decoding dependencies;
//! Office part ownership, repository identities and generation publication are separate.

use std::{fmt, ops::Range};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, content_hash};
use rootlight_source::zip::{ZipArchive, ZipEntry, ZipError, ZipLimits};

const DECODE_CHUNK_BYTES: usize = 16 * 1024;
const MAX_READS: usize = 65_536;

use crate::compression::{Error as CompressionError, inflate};

/// Independent preparation, expansion and repeated-work limits for one archive.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ZipSourceLimits {
    /// Input and directory limits enforced by the source archive reader.
    pub archive: ZipLimits,
    /// Maximum declared or actual output bytes for one entry.
    pub entry_bytes: usize,
    /// Maximum cumulative declared output reservation, including failed reads.
    pub expanded_bytes: usize,
    /// Maximum cumulative encoded input reservation, including failed reads.
    pub encoded_bytes: usize,
    /// Maximum read attempts, including missing or unsupported entries.
    pub reads: usize,
}

impl Default for ZipSourceLimits {
    fn default() -> Self {
        Self {
            archive: ZipLimits::default(),
            entry_bytes: 16 * 1024 * 1024,
            expanded_bytes: 16 * 1024 * 1024,
            encoded_bytes: 16 * 1024 * 1024,
            reads: 4096,
        }
    }
}

/// A source-free failure to prepare or expand one archive entry.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ZipExpansionError {
    /// Archive metadata or expanded CRC/length validation failed.
    #[error(transparent)]
    Archive(#[from] ZipError),
    /// A resource ceiling exceeds the existing hard bound.
    #[error("invalid zip expansion limits")]
    Limits,
    /// One caller-selected expansion budget was exceeded.
    #[error("zip expansion {resource} limit exceeded: {maximum}")]
    Limit {
        /// Static resource label, never source content.
        resource: &'static str,
        /// Configured ceiling.
        maximum: usize,
    },
    /// The selector does not identify a record in this archive.
    #[error("zip entry ordinal is absent")]
    MissingEntry,
    /// The method or flags require unsupported decoding, encryption or patching.
    #[error("unsupported zip method {method} or flags {flags}")]
    Encoding {
        /// Original compression method.
        method: u16,
        /// Original general-purpose flags.
        flags: u16,
    },
    /// The raw stream is malformed, truncated, stalled or has trailing input.
    #[error("invalid deflate stream at encoded byte {input_offset}")]
    Deflate {
        /// Offset within the entry's encoded payload, not the original file.
        input_offset: usize,
    },
    /// Actual expansion exceeded its checked declaration.
    #[error("zip expanded size differs from its declaration")]
    OutputSize,
    /// A bounded output allocation could not be reserved.
    #[error("zip expanded output allocation failed")]
    Allocation,
    /// The caller cancelled or its deadline expired.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

impl From<CompressionError> for ZipExpansionError {
    fn from(error: CompressionError) -> Self {
        match error {
            CompressionError::Deflate { input_offset } => Self::Deflate { input_offset },
            CompressionError::OutputSize => Self::OutputSize,
            CompressionError::Allocation => Self::Allocation,
            CompressionError::Cancelled(error) => Self::Cancelled(error),
        }
    }
}

/// Verified entry bytes and their original archive decoding dependencies.
///
/// These ranges identify encoded records, not byte-for-byte expanded mappings.
/// The archive hash is not a repository, package ownership or generation identity.
pub struct ExpandedZipSource {
    bytes: Vec<u8>,
    archive_hash: ContentHash,
    ordinal: usize,
    central: Range<usize>,
    local: Range<usize>,
    encoded: Range<usize>,
    descriptor: Range<usize>,
}

impl fmt::Debug for ExpandedZipSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExpandedZipSource")
            .field("archive_hash", &self.archive_hash)
            .field("ordinal", &self.ordinal)
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

impl ExpandedZipSource {
    /// Returns exact expanded bytes after complete-stream and CRC validation.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    /// Transfers ownership of the verified expanded bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
    /// Returns the hash of the complete original archive snapshot.
    #[must_use]
    pub const fn archive_hash(&self) -> ContentHash {
        self.archive_hash
    }
    /// Returns the original central directory ordinal, preserving duplicate names.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }
    /// Returns the complete original central directory record range.
    #[must_use]
    pub fn central_range(&self) -> Range<usize> {
        self.central.clone()
    }
    /// Returns the original local header, name and extra-field range.
    #[must_use]
    pub fn local_range(&self) -> Range<usize> {
        self.local.clone()
    }
    /// Returns the encoded byte dependency in the original archive.
    #[must_use]
    pub fn encoded_range(&self) -> Range<usize> {
        self.encoded.clone()
    }
    /// Returns the original data descriptor range, empty when absent.
    #[must_use]
    pub fn descriptor_range(&self) -> Range<usize> {
        self.descriptor.clone()
    }
}

/// One archive snapshot with cumulative, non-refundable decoding reservations.
#[derive(Debug)]
pub struct ZipSourceArchive<'a> {
    archive: ZipArchive<'a>,
    archive_hash: ContentHash,
    limits: ZipSourceLimits,
    reads: usize,
    expanded_reserved: usize,
    encoded_reserved: usize,
}

impl<'a> ZipSourceArchive<'a> {
    /// Prepares bounded archive metadata and pins expansion to the supplied bytes.
    ///
    /// # Errors
    /// Rejects invalid limits/records, exceeded preparation bounds or cancellation.
    /// No files, processes, external resources or embedded code are accessed.
    pub fn open(
        input: &'a [u8],
        limits: ZipSourceLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, ZipExpansionError> {
        cancellation.check()?;
        let hard = usize::try_from(rootlight_source::HARD_MAX_SNAPSHOT_BYTES)
            .map_err(|_| ZipExpansionError::Limits)?;
        if [
            limits.entry_bytes,
            limits.expanded_bytes,
            limits.encoded_bytes,
        ]
        .into_iter()
        .any(|maximum| maximum > hard)
            || limits.reads == 0
            || limits.reads > MAX_READS
        {
            return Err(ZipExpansionError::Limits);
        }
        let archive = ZipArchive::open(input, limits.archive, cancellation)?;
        let archive_hash = content_hash(input);
        cancellation.check()?;
        Ok(Self {
            archive,
            archive_hash,
            limits,
            reads: 0,
            expanded_reserved: 0,
            encoded_reserved: 0,
        })
    }

    /// Returns validated metadata in directory order; names remain untrusted bytes.
    #[must_use]
    pub fn entries(&self) -> &[ZipEntry<'a>] {
        self.archive.entries()
    }
    /// Returns consumed read attempts, including missing or unsupported selectors.
    #[must_use]
    pub const fn read_attempts(&self) -> usize {
        self.reads
    }
    /// Returns output reservations, including attempts that failed after admission.
    #[must_use]
    pub const fn expanded_bytes_reserved(&self) -> usize {
        self.expanded_reserved
    }
    /// Returns encoded input reservations, including attempts that failed after admission.
    #[must_use]
    pub const fn encoded_bytes_reserved(&self) -> usize {
        self.encoded_reserved
    }

    /// Reads one stored or raw-Deflate entry under cumulative work and output bounds.
    ///
    /// Every admitted attempt reserves declared bytes before allocation. Reservations
    /// remain charged on errors or when callers drop outputs, preventing retry loops
    /// from repeatedly inflating the same malformed or highly compressed input.
    ///
    /// # Errors
    /// Returns typed selection, encoding, limit, allocation, framing, size, CRC or
    /// cancellation failures; partial expanded bytes are never returned as source.
    pub fn read_entry(
        &mut self,
        ordinal: usize,
        cancellation: &Cancellation,
    ) -> Result<ExpandedZipSource, ZipExpansionError> {
        cancellation.check()?;
        self.reads = charge(self.reads, 1, self.limits.reads, "reads")?;
        let entry = self
            .archive
            .entries()
            .get(ordinal)
            .ok_or(ZipExpansionError::MissingEntry)?;
        let flags = entry.flags();
        let allowed_flags = match entry.method() {
            0 => 8 | 0x0800,
            8 => 6 | 8 | 0x0800,
            _ => {
                return Err(ZipExpansionError::Encoding {
                    method: entry.method(),
                    flags,
                });
            }
        };
        if flags & !allowed_flags != 0 {
            return Err(ZipExpansionError::Encoding {
                method: entry.method(),
                flags,
            });
        }
        let declared =
            usize::try_from(entry.expanded_bytes()).map_err(|_| ZipExpansionError::Limits)?;
        charge(0, declared, self.limits.entry_bytes, "entry bytes")?;
        let expanded = charge(
            self.expanded_reserved,
            declared,
            self.limits.expanded_bytes,
            "expanded bytes",
        )?;
        let encoded = charge(
            self.encoded_reserved,
            entry.encoded_bytes().len(),
            self.limits.encoded_bytes,
            "encoded bytes",
        )?;
        self.expanded_reserved = expanded;
        self.encoded_reserved = encoded;
        let bytes = if entry.method() == 0 {
            let stored = entry.stored_bytes(self.limits.entry_bytes, cancellation)?;
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(stored.len())
                .map_err(|_| ZipExpansionError::Allocation)?;
            for chunk in stored.chunks(DECODE_CHUNK_BYTES) {
                cancellation.check()?;
                bytes.extend_from_slice(chunk);
            }
            bytes
        } else {
            let bytes = inflate(entry.encoded_bytes(), declared, cancellation)?;
            entry.verify_expanded(&bytes, self.limits.entry_bytes, cancellation)?;
            bytes
        };
        cancellation.check()?;
        Ok(ExpandedZipSource {
            bytes,
            archive_hash: self.archive_hash,
            ordinal,
            central: entry.central_range(),
            local: entry.local_range(),
            encoded: entry.payload_range(),
            descriptor: entry.descriptor_range(),
        })
    }
}

fn charge(
    used: usize,
    amount: usize,
    maximum: usize,
    resource: &'static str,
) -> Result<usize, ZipExpansionError> {
    used.checked_add(amount)
        .filter(|total| *total <= maximum)
        .ok_or(ZipExpansionError::Limit { resource, maximum })
}

#[cfg(test)]
mod tests;
