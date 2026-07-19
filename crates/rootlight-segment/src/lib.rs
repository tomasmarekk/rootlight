//! Bounded in-memory prototype for immutable generation segments.
//!
//! The crate owns encoded bytes and deliberately exposes no filesystem,
//! mapping, publication, or recovery API while native publication is disabled.

#![forbid(unsafe_code)]

mod format;
mod reader;

use std::sync::Arc;

use rootlight_storage::{
    GenerationContext, GenerationControlError, GenerationStats, IdentityVerifiedGeneration,
    ReadPageError,
};

pub use reader::SegmentReader;

/// Current portable little-endian segment format major version.
pub const SEGMENT_FORMAT_MAJOR: u16 = 1;
/// Current portable little-endian segment format minor version.
pub const SEGMENT_FORMAT_MINOR: u16 = 0;
/// Hard ceiling for one encoded research segment.
pub const MAX_SEGMENT_BYTES: usize = 512 * 1024 * 1024;

/// One deterministically encoded immutable segment held in owned memory.
#[derive(Debug, Clone)]
pub struct Segment {
    bytes: Arc<[u8]>,
}

impl Segment {
    /// Encodes one identity-verified generation and its sealed statistics.
    ///
    /// Logical counters in `stats` are checked against the canonical
    /// generation before they enter the segment manifest. The remaining
    /// counters retain the build backend's measured resource accounting.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentError`] for cancellation, resource exhaustion,
    /// inconsistent statistics, encoding failure, or the segment byte ceiling.
    pub fn encode(
        generation: IdentityVerifiedGeneration,
        stats: GenerationStats,
        context: &GenerationContext<'_>,
    ) -> Result<Self, SegmentError> {
        let bytes = format::encode(generation, stats, context)?;
        Ok(Self { bytes })
    }

    /// Borrows the complete portable byte representation.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns a shared owner of the immutable byte representation.
    #[must_use]
    pub fn bytes(&self) -> Arc<[u8]> {
        Arc::clone(&self.bytes)
    }

    /// Consumes the segment into its immutable byte owner.
    #[must_use]
    pub fn into_bytes(self) -> Arc<[u8]> {
        self.bytes
    }
}

/// Failure while encoding, opening, or reading an immutable segment.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SegmentError {
    /// Cooperative cancellation or a generation resource budget stopped work.
    #[error("segment work was interrupted")]
    Control(#[source] GenerationControlError),
    /// The writer could not produce the canonical representation.
    #[error("segment encoding failed")]
    Encoding,
    /// A bounded allocation could not be reserved.
    #[error("segment allocation failed")]
    Allocation,
    /// The encoded form would exceed the fixed research ceiling.
    #[error("segment exceeds its byte ceiling of {maximum} bytes")]
    TooLarge {
        /// Largest accepted encoded segment.
        maximum: usize,
    },
    /// The format major/minor pair is not understood by this reader.
    #[error("segment format {major}.{minor} is unsupported")]
    UnsupportedVersion {
        /// Unsupported major version.
        major: u16,
        /// Unsupported minor version.
        minor: u16,
    },
    /// Caller-supplied generation statistics disagree with logical records.
    #[error("segment statistics do not match the generation")]
    InvalidStatistics,
    /// Header, bounds, checksum, canonical encoding, index, or identity failed.
    #[error("segment is corrupt")]
    Corrupt,
    /// A backend-neutral page invariant failed after index reconstruction.
    #[error("segment page construction failed")]
    Page(#[source] ReadPageError),
}

impl From<GenerationControlError> for SegmentError {
    fn from(error: GenerationControlError) -> Self {
        Self::Control(error)
    }
}

impl From<ReadPageError> for SegmentError {
    fn from(error: ReadPageError) -> Self {
        Self::Page(error)
    }
}
