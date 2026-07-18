//! Cross-language contract primitives for Rootlight's normalized IR.
//!
//! The frozen version 1.0 envelope remains available for compatibility. Version
//! 1.1 adds language-neutral files, entities, occurrences, relations, provenance,
//! source mappings, coverage, diagnostics, and extension envelopes.
//! Untrusted documents and standalone extension envelopes must enter through
//! the explicit byte-bounded decoders. Public dynamic IR values intentionally
//! do not implement `Deserialize`.

#![forbid(unsafe_code)]

mod identity;
mod lexical;
mod normalized;
mod validation;

pub use identity::{
    FILE_IDENTITY_CLAIM_NAMESPACE, FactIdentityRecipeError, FileIdentityClaim,
    IDENTITY_CLAIM_VERSION, IdentityClaimError, SYMBOL_IDENTITY_CLAIM_NAMESPACE,
    SymbolIdentityClaim, decode_file_identity_claim_envelope,
    decode_file_identity_claim_envelope_with_checkpoint, decode_symbol_identity_claim_envelope,
    decode_symbol_identity_claim_envelope_with_checkpoint, derive_coverage_record_id,
    derive_coverage_record_id_with_checkpoint, derive_diagnostic_record_id,
    derive_diagnostic_record_id_with_checkpoint, derive_occurrence_record_id,
    derive_occurrence_record_id_with_checkpoint, derive_provenance_record_id,
    derive_provenance_record_id_with_checkpoint, derive_relation_record_id,
    derive_relation_record_id_with_checkpoint, derive_skipped_region_id,
    derive_skipped_region_id_with_checkpoint, derive_source_mapping_record_id,
    derive_source_mapping_record_id_with_checkpoint, entity_kind_identity_label,
    new_file_identity_claim_envelope, new_symbol_identity_claim_envelope,
};
pub use lexical::{
    LEXICAL_EXTENSION_NAMESPACE, LEXICAL_EXTENSION_VERSION, LexicalEvidenceFormat,
    LexicalEvidenceKind, LexicalEvidenceV1, LexicalExtensionError, MAX_LEXICAL_PAYLOAD_BYTES,
    MAX_LEXICAL_SIGNATURE_BYTES, MAX_LEXICAL_SUMMARY_BYTES, decode_lexical_evidence,
    decode_lexical_evidence_envelope, encode_lexical_evidence, new_lexical_evidence_envelope,
    validate_lexical_evidence_envelope,
};
pub use normalized::{
    ContainerRef, CoverageRecord, CoverageScope, DiagnosticRecord, DiagnosticSeverity, EntityFlag,
    EntityKind, EntityRecord, EntityVisibility, ExtensionCriticality, ExtensionEnvelope,
    ExtensionEnvelopeDecodeError, FactDomain, FactEvidence, FactRef, FilePathLocator,
    FilePathLocatorEncoding, FilePathLocatorError, FileRecord, IrDocument, IrDocumentDecodeError,
    LegacyIrDocumentDecodeError, MAX_FILE_PATH_LOCATOR_COMPONENTS,
    MAX_FILE_PATH_LOCATOR_ENCODED_BYTES, NORMALIZED_IR_VERSION, NormalizedIrDocument,
    NormalizedIrVersion, NormalizedRecordDecodeError, OccurrenceRecord, OccurrenceRole,
    OccurrenceTarget, ProducerKind, ProvenanceRecord, RelationEndpoint, RelationPredicate,
    RelationRecord, SkippedRegion, SkippedRegionReason, SourceMappingKind, SourceMappingRecord,
    decode_diagnostic_record_with_checkpoint, decode_extension_envelope,
    decode_extension_envelope_with_checkpoint, decode_ir_document, decode_legacy_ir_document,
    decode_skipped_region_with_checkpoint, decode_source_mapping_record_with_checkpoint,
};
pub use validation::{
    ExtensionIdentifier, ExtensionSupport, IrDocumentValidationError, IrLimits,
    UnknownNoncriticalExtensionPolicy, canonicalize_ir_document, validate_ir_document,
};

use rootlight_ids::{ContentHash, FileId, GenerationId, RepositoryId};
use serde::{Deserialize, Serialize};

/// The initial production IR contract version.
pub const IR_VERSION: IrVersion = IrVersion::new(1, 0);

/// Major/minor version for the normalized IR contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

    /// Ensures the version belongs to the supported production major.
    ///
    /// Additive minor versions under major 1 are accepted so newer producers
    /// can emit compatible documents without changing the decoding boundary.
    ///
    /// # Errors
    ///
    /// Returns [`IrValidationError::UnsupportedMajor`] when the major differs
    /// from [`IR_VERSION`].
    pub const fn require_supported(self) -> Result<Self, IrValidationError> {
        if self.major == IR_VERSION.major {
            Ok(self)
        } else {
            Err(IrValidationError::UnsupportedMajor { major: self.major })
        }
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for IrVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "IrVersion".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "properties": {
                "major": { "type": "integer", "const": 1 },
                "minor": { "type": "integer", "minimum": 0, "maximum": 65_535 }
            },
            "required": ["major", "minor"],
            "additionalProperties": false
        })
    }
}

/// A validated half-open source byte span within one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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

impl<'de> Deserialize<'de> for SourceSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireSourceSpan {
            file: FileId,
            start_byte: u64,
            end_byte: u64,
        }

        let wire = WireSourceSpan::deserialize(deserializer)?;
        Self::new(wire.file, wire.start_byte, wire.end_byte).map_err(serde::de::Error::custom)
    }
}

/// A validated one-based inclusive source line range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct LineRange {
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
    start_line: u64,
    #[cfg_attr(feature = "schema", schemars(range(min = 1)))]
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

impl<'de> Deserialize<'de> for LineRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireLineRange {
            start_line: u64,
            end_line: u64,
        }

        let wire = WireLineRange::deserialize(deserializer)?;
        Self::new(wire.start_line, wire.end_line).map_err(serde::de::Error::custom)
    }
}

/// A generation-bound reference to immutable repository source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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

impl<'de> Deserialize<'de> for Confidence {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct IrDocumentSchema {
    /// IR contract version used by the document.
    version: IrVersion,
    /// Immutable repository generation owning every source reference.
    generation: GenerationId,
    /// Producer identity for the contained facts.
    producer: ProducerIdentity,
    /// Build context used to interpret conditional source.
    build_context: BuildContextIdentity,
    /// Declared completeness of the document.
    coverage: CoverageStatus,
    /// Evidence class supporting the document.
    evidence: EvidenceKind,
}

impl IrDocumentSchema {
    /// Creates a checked normalized-IR contract envelope.
    ///
    /// # Errors
    ///
    /// Returns [`IrValidationError::UnsupportedMajor`] when `version` is not in
    /// the supported production major.
    pub fn new(
        version: IrVersion,
        generation: GenerationId,
        producer: ProducerIdentity,
        build_context: BuildContextIdentity,
        coverage: CoverageStatus,
        evidence: EvidenceKind,
    ) -> Result<Self, IrValidationError> {
        let version = version.require_supported()?;
        Ok(Self {
            version,
            generation,
            producer,
            build_context,
            coverage,
            evidence,
        })
    }

    /// Returns the compatible IR contract version.
    #[must_use]
    pub const fn version(&self) -> IrVersion {
        self.version
    }

    /// Returns the immutable repository generation.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the producer identity.
    #[must_use]
    pub const fn producer(&self) -> &ProducerIdentity {
        &self.producer
    }

    /// Returns the build-context identity.
    #[must_use]
    pub const fn build_context(&self) -> BuildContextIdentity {
        self.build_context
    }

    /// Returns the declared coverage status.
    #[must_use]
    pub const fn coverage(&self) -> CoverageStatus {
        self.coverage
    }

    /// Returns the evidence class.
    #[must_use]
    pub const fn evidence(&self) -> EvidenceKind {
        self.evidence
    }
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
    /// The document uses an unsupported IR major version.
    #[error("unsupported IR major version {major}")]
    UnsupportedMajor {
        /// Unsupported major component.
        major: u16,
    },
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

    #[test]
    fn deserialization_preserves_primitive_invariants() {
        let file = FileId::from_bytes([3; 20]).to_string();
        assert!(
            serde_json::from_value::<SourceSpan>(serde_json::json!({
                "file": file,
                "start_byte": 12,
                "end_byte": 11
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<LineRange>(serde_json::json!({
                "start_line": 0,
                "end_line": 1
            }))
            .is_err()
        );
        assert!(serde_json::from_value::<Confidence>(serde_json::json!(1_001)).is_err());
        assert!(
            ProducerIdentity::new("path/shaped", "1.0", content_hash(b"configuration")).is_err()
        );
    }

    #[test]
    fn nested_source_reference_deserialization_is_checked() {
        let repository = derive_repository(b"repository").id();
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let invalid = serde_json::json!({
            "repository": repository,
            "generation": generation,
            "span": {"file": file, "start_byte": 8, "end_byte": 7},
            "content_hash": content_hash(b"fixture"),
            "line_hint": {"start_line": 0, "end_line": 1}
        });

        assert!(serde_json::from_value::<SourceRef>(invalid).is_err());
    }

    #[cfg(feature = "schema")]
    #[test]
    fn document_accepts_additive_minors_and_rejects_other_majors() {
        let generation = GenerationId::from_bytes([2; 20]);
        let hash = content_hash(b"fixture");
        let document = |major, minor| {
            serde_json::json!({
                "version": {"major": major, "minor": minor},
                "generation": generation,
                "producer": {
                    "name": "fixture",
                    "version": "1.0",
                    "configuration_hash": hash
                },
                "build_context": {"digest": hash},
                "coverage": "complete",
                "evidence": "syntax"
            })
        };

        let encoded =
            serde_json::to_vec(&document(1, u16::MAX)).expect("legacy fixture serializes");
        let decoded = decode_legacy_ir_document(&encoded, &IrLimits::default())
            .expect("additive minor is supported");
        assert_eq!(decoded.version(), IrVersion::new(1, u16::MAX));
        for major in [0, 2] {
            let encoded =
                serde_json::to_vec(&document(major, 0)).expect("legacy fixture serializes");
            assert!(decode_legacy_ir_document(&encoded, &IrLimits::default()).is_err());
        }
    }
}
