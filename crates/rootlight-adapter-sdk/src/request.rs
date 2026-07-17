//! Per-file immutable parser and analyzer requests.
//!
//! The checked snapshot wrapper binds concrete VFS bytes to a full-file
//! repository generation reference before an adapter can observe them.

use std::fmt;

use rootlight_ir::{AnalysisTier, BuildContextIdentity, SourceRef, SourceSpan};
use rootlight_vfs::{RelativePath, SourceSnapshot};

use crate::{
    descriptor::{EncodingId, LanguageId},
    error::{RequestError, SnapshotError},
    limits::AnalysisLimits,
};

/// A checked full-file source range with an embedded language identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncludedRange {
    span: SourceSpan,
    language: LanguageId,
}

impl IncludedRange {
    /// Creates an included range; its owning request validates file and bounds.
    #[must_use]
    pub const fn new(span: SourceSpan, language: LanguageId) -> Self {
        Self { span, language }
    }

    /// Returns the included source span.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    /// Returns the language active in the included range.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }
}

/// Concrete immutable VFS bytes bound to one full-file generation reference.
#[derive(Clone)]
pub struct GenerationBoundSnapshot<'a> {
    snapshot: &'a SourceSnapshot,
    source: SourceRef,
}

impl<'a> GenerationBoundSnapshot<'a> {
    /// Binds a VFS snapshot to an exact full-file [`SourceRef`].
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when file, hash, or full-file span identity
    /// differs from the captured bytes.
    pub fn new(snapshot: &'a SourceSnapshot, source: &SourceRef) -> Result<Self, SnapshotError> {
        if snapshot.file() != source.span().file() {
            return Err(SnapshotError::FileMismatch);
        }
        if snapshot.content_hash() != source.content_hash() {
            return Err(SnapshotError::ContentHashMismatch);
        }
        let byte_length =
            u64::try_from(snapshot.content().len()).map_err(|_| SnapshotError::LengthOverflow)?;
        if source.span().start_byte() != 0
            || source.span().end_byte() != byte_length
            || snapshot.metadata().length != byte_length
        {
            return Err(SnapshotError::NotFullFile);
        }
        Ok(Self {
            snapshot,
            source: source.clone(),
        })
    }

    /// Returns the immutable captured source bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        self.snapshot.content()
    }

    /// Returns the canonical repository-relative path.
    #[must_use]
    pub const fn path(&self) -> &RelativePath {
        self.snapshot.path()
    }

    /// Returns the exact full-file generation reference.
    #[must_use]
    pub const fn source_ref(&self) -> &SourceRef {
        &self.source
    }

    /// Returns the underlying immutable VFS snapshot.
    #[must_use]
    pub const fn snapshot(&self) -> &SourceSnapshot {
        self.snapshot
    }
}

impl fmt::Debug for GenerationBoundSnapshot<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GenerationBoundSnapshot")
            .field("source", &self.source)
            .field("byte_length", &self.snapshot.content().len())
            .finish()
    }
}

/// One bounded synchronous parse request for immutable source bytes.
#[derive(Debug, Clone)]
pub struct ParseRequest<'a> {
    source: GenerationBoundSnapshot<'a>,
    language: LanguageId,
    encoding: EncodingId,
    included_ranges: Vec<IncludedRange>,
    limits: &'a AnalysisLimits,
}

impl<'a> ParseRequest<'a> {
    /// Creates a checked per-file parse request.
    ///
    /// # Errors
    ///
    /// Returns [`RequestError`] when source bytes or included ranges violate
    /// the explicit analysis limits.
    pub fn new(
        source: GenerationBoundSnapshot<'a>,
        language: LanguageId,
        encoding: EncodingId,
        included_ranges: Vec<IncludedRange>,
        limits: &'a AnalysisLimits,
    ) -> Result<Self, RequestError> {
        validate_source_size(&source, limits)?;
        validate_included_ranges(&source, &included_ranges, limits)?;
        Ok(Self {
            source,
            language,
            encoding,
            included_ranges,
            limits,
        })
    }

    /// Returns the immutable generation-bound source.
    #[must_use]
    pub const fn source(&self) -> &GenerationBoundSnapshot<'a> {
        &self.source
    }

    /// Returns the requested language identity.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }

    /// Returns the declared source encoding.
    #[must_use]
    pub const fn encoding(&self) -> &EncodingId {
        &self.encoding
    }

    /// Returns sorted, disjoint embedded source ranges.
    #[must_use]
    pub fn included_ranges(&self) -> &[IncludedRange] {
        &self.included_ranges
    }

    /// Returns explicit analysis limits.
    #[must_use]
    pub const fn limits(&self) -> &AnalysisLimits {
        self.limits
    }
}

/// One bounded synchronous normalized-IR analysis request.
#[derive(Debug, Clone)]
pub struct AnalysisRequest<'a> {
    source: GenerationBoundSnapshot<'a>,
    language: LanguageId,
    tier: AnalysisTier,
    build_context: BuildContextIdentity,
    limits: &'a AnalysisLimits,
}

impl<'a> AnalysisRequest<'a> {
    /// Creates a checked per-file analysis request.
    ///
    /// # Errors
    ///
    /// Returns [`RequestError::SourceTooLarge`] when the immutable file exceeds
    /// its explicit request bound.
    pub fn new(
        source: GenerationBoundSnapshot<'a>,
        language: LanguageId,
        tier: AnalysisTier,
        build_context: BuildContextIdentity,
        limits: &'a AnalysisLimits,
    ) -> Result<Self, RequestError> {
        validate_source_size(&source, limits)?;
        Ok(Self {
            source,
            language,
            tier,
            build_context,
            limits,
        })
    }

    /// Returns the immutable generation-bound source.
    #[must_use]
    pub const fn source(&self) -> &GenerationBoundSnapshot<'a> {
        &self.source
    }

    /// Returns the requested language identity.
    #[must_use]
    pub const fn language(&self) -> &LanguageId {
        &self.language
    }

    /// Returns the requested analysis tier.
    #[must_use]
    pub const fn tier(&self) -> AnalysisTier {
        self.tier
    }

    /// Returns the build-context identity.
    #[must_use]
    pub const fn build_context(&self) -> BuildContextIdentity {
        self.build_context
    }

    /// Returns explicit analysis limits.
    #[must_use]
    pub const fn limits(&self) -> &AnalysisLimits {
        self.limits
    }
}

fn validate_source_size(
    source: &GenerationBoundSnapshot<'_>,
    limits: &AnalysisLimits,
) -> Result<(), RequestError> {
    let observed = source.bytes().len();
    if observed > limits.max_source_bytes() {
        Err(RequestError::SourceTooLarge {
            observed,
            limit: limits.max_source_bytes(),
        })
    } else {
        Ok(())
    }
}

fn validate_included_ranges(
    source: &GenerationBoundSnapshot<'_>,
    ranges: &[IncludedRange],
    limits: &AnalysisLimits,
) -> Result<(), RequestError> {
    if ranges.len() > limits.max_embedded_ranges() {
        return Err(RequestError::TooManyIncludedRanges {
            observed: ranges.len(),
            limit: limits.max_embedded_ranges(),
        });
    }
    let full = source.source_ref().span();
    let mut previous_end = full.start_byte();
    for (index, range) in ranges.iter().enumerate() {
        let span = range.span();
        if span.file() != full.file()
            || span.start_byte() < full.start_byte()
            || span.end_byte() > full.end_byte()
            || span.start_byte() == span.end_byte()
        {
            return Err(RequestError::IncludedRangeOutsideSource { index });
        }
        if span.start_byte() < previous_end {
            return Err(RequestError::IncludedRangeOrder { index });
        }
        previous_end = span.end_byte();
    }
    Ok(())
}
