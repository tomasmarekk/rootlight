//! Cross-language contract primitives for Rootlight's normalized IR.
//!
//! P0 defines only the common version, source, evidence, confidence, coverage,
//! producer, and extension boundaries. Language entities and relations remain
//! owned by their later roadmap tasks.

#![forbid(unsafe_code)]

use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use serde::{Deserialize, Serialize};

/// The initial production IR contract version.
pub const IR_VERSION: IrVersion = IrVersion::new(1, 0);

/// Major/minor version for the normalized IR contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct IrVersion {
    major: u16,
    minor: u16,
}

impl IrVersion {
    /// Creates an IR version from numeric components.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the major compatibility component.
    #[must_use]
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the additive minor component.
    #[must_use]
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

/// A validated half-open source byte span within one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceSpan {
    file: FileId,
    start_byte: u64,
    end_byte: u64,
}

impl SourceSpan {
    /// Creates a half-open span when `start_byte <= end_byte`.
    ///
    /// # Errors
    ///
    /// Returns [`IrValidationError::InvalidSourceSpan`] for an inverted range.
    pub fn new(file: FileId, start_byte: u64, end_byte: u64) -> Result<Self, IrValidationError> {
        if start_byte > end_byte {
            return Err(IrValidationError::InvalidSourceSpan);
        }
        Ok(Self {
            file,
            start_byte,
            end_byte,
        })
    }

    /// Returns the owning file identity.
    #[must_use]
    pub const fn file(self) -> FileId {
        self.file
    }

    /// Returns the inclusive start byte.
    #[must_use]
    pub const fn start_byte(self) -> u64 {
        self.start_byte
    }

    /// Returns the exclusive end byte.
    #[must_use]
    pub const fn end_byte(self) -> u64 {
        self.end_byte
    }
}

/// A validated one-based inclusive source line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LineRange {
    start_line: u64,
    end_line: u64,
}

impl LineRange {
    /// Creates a nonzero inclusive line range.
    ///
    /// # Errors
    ///
    /// Returns [`IrValidationError::InvalidLineRange`] for zero or inverted
    /// line numbers.
    pub fn new(start_line: u64, end_line: u64) -> Result<Self, IrValidationError> {
        if start_line == 0 || start_line > end_line {
            return Err(IrValidationError::InvalidLineRange);
        }
        Ok(Self {
            start_line,
            end_line,
        })
    }

    /// Returns the inclusive first line.
    #[must_use]
    pub const fn start_line(self) -> u64 {
        self.start_line
    }

    /// Returns the inclusive last line.
    #[must_use]
    pub const fn end_line(self) -> u64 {
        self.end_line
    }
}

/// A generation-bound reference to immutable repository source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct SourceRef {
    repository: RepositoryId,
    generation: GenerationId,
    span: SourceSpan,
    content_hash: ContentHash,
    line_hint: Option<LineRange>,
}

impl SourceRef {
    /// Creates a generation-bound source reference.
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        generation: GenerationId,
        span: SourceSpan,
        content_hash: ContentHash,
        line_hint: Option<LineRange>,
    ) -> Self {
        Self {
            repository,
            generation,
            span,
            content_hash,
            line_hint,
        }
    }

    /// Returns the repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the pinned generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the authoritative byte span.
    #[must_use]
    pub const fn span(&self) -> SourceSpan {
        self.span
    }

    /// Returns the expected immutable content hash.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns the optional presentation-only line hint.
    #[must_use]
    pub const fn line_hint(&self) -> Option<LineRange> {
        self.line_hint
    }
}

/// Evidence class supporting a normalized fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EvidenceKind {
    /// Direct syntax declaration or occurrence.
    Syntax,
    /// Compiler or precise index evidence.
    Compiler,
    /// Language-server evidence.
    LanguageServer,
    /// Imported SCIP evidence.
    Scip,
    /// Bounded deterministic derivation from base facts.
    Derived,
}

/// Declared language-analysis tier for one producer or result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnalysisTier {
    /// Compiler-assisted project semantics.
    TierA,
    /// Full structural semantics.
    TierB,
    /// Symbols and imports.
    TierC,
    /// Syntax fallback.
    TierD,
}

/// Fixed-point confidence from 0 through 1000 inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Confidence(#[cfg_attr(feature = "schema", schemars(range(max = 1_000)))] u16);

impl Confidence {
    /// Creates a checked fixed-point confidence.
    ///
    /// # Errors
    ///
    /// Returns [`IrValidationError::InvalidConfidence`] above 1000.
    pub const fn new(value: u16) -> Result<Self, IrValidationError> {
        if value <= 1_000 {
            Ok(Self(value))
        } else {
            Err(IrValidationError::InvalidConfidence)
        }
    }

    /// Returns the fixed-point value.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

/// Completeness of a bounded fact or result set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CoverageStatus {
    /// The declared domain was completely analyzed.
    Complete,
    /// A documented bound truncated the domain.
    Bounded,
    /// A representative sample was analyzed.
    Sampled,
    /// Completeness could not be established.
    Unknown,
}

/// Stable identity of the component that produced facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ProducerIdentity {
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 128), regex(pattern = r"^[A-Za-z0-9_.+-]+$"))
    )]
    name: String,
    #[cfg_attr(
        feature = "schema",
        schemars(length(min = 1, max = 128), regex(pattern = r"^[A-Za-z0-9_.+-]+$"))
    )]
    version: String,
    configuration_hash: ContentHash,
}

impl ProducerIdentity {
    /// Creates a producer identity from validated source-free labels.
    ///
    /// # Errors
    ///
    /// Returns [`IrValidationError::InvalidProducerLabel`] for empty, oversized,
    /// whitespace-containing, or path-shaped labels.
    pub fn new(
        name: &str,
        version: &str,
        configuration_hash: ContentHash,
    ) -> Result<Self, IrValidationError> {
        if !valid_label(name) || !valid_label(version) {
            return Err(IrValidationError::InvalidProducerLabel);
        }
        Ok(Self {
            name: name.to_owned(),
            version: version.to_owned(),
            configuration_hash,
        })
    }

    /// Returns the producer name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the producer version label.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Returns the producer configuration hash.
    #[must_use]
    pub const fn configuration_hash(&self) -> ContentHash {
        self.configuration_hash
    }
}

/// Stable digest identifying one build-context interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct BuildContextIdentity {
    digest: ContentHash,
}

impl BuildContextIdentity {
    /// Creates a build-context identity from its canonical digest.
    #[must_use]
    pub const fn new(digest: ContentHash) -> Self {
        Self { digest }
    }

    /// Returns the canonical build-context digest.
    #[must_use]
    pub const fn digest(self) -> ContentHash {
        self.digest
    }
}

/// Versioned P0 envelope for normalized IR contract fixtures.
#[cfg(feature = "schema")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct IrDocumentSchema {
    /// IR contract version used by the document.
    pub version: IrVersion,
    /// Immutable repository generation owning every source reference.
    pub generation: GenerationId,
    /// Producer identity for the contained facts.
    pub producer: ProducerIdentity,
    /// Build context used to interpret conditional source.
    pub build_context: BuildContextIdentity,
    /// Declared completeness of the document.
    pub coverage: CoverageStatus,
    /// Evidence class supporting the document.
    pub evidence: EvidenceKind,
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'+'))
}

/// Validation failures for normalized IR primitives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IrValidationError {
    /// A byte span was inverted.
    #[error("source span start exceeds end")]
    InvalidSourceSpan,
    /// A line range was zero or inverted.
    #[error("source line range is invalid")]
    InvalidLineRange,
    /// Confidence exceeded its fixed-point range.
    #[error("confidence exceeds 1000")]
    InvalidConfidence,
    /// A producer label was unsafe or unbounded.
    #[error("producer label is invalid")]
    InvalidProducerLabel,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash, derive_repository};

    #[test]
    fn validates_source_ranges() {
        let file = FileId::from_bytes([1; 20]);
        assert!(SourceSpan::new(file, 10, 9).is_err());
        assert!(LineRange::new(0, 1).is_err());
        assert!(LineRange::new(2, 1).is_err());
    }

    #[test]
    fn confidence_is_bounded() {
        assert_eq!(Confidence::new(1_000).map(Confidence::get), Ok(1_000));
        assert_eq!(
            Confidence::new(1_001),
            Err(IrValidationError::InvalidConfidence)
        );
    }

    #[test]
    fn source_reference_is_generation_bound() {
        let repository = derive_repository(b"repository").id();
        let generation = GenerationId::from_bytes([2; 20]);
        let span =
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, 12).expect("fixture span is valid");
        let reference =
            SourceRef::new(repository, generation, span, content_hash(b"fixture"), None);
        assert_eq!(reference.repository(), repository);
        assert_eq!(reference.generation(), generation);
    }
}
