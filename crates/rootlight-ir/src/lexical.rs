//! Bounded first-party lexical evidence carried by normalized IR extensions.
//!
//! The common IR never depends on this noncritical payload. Typed consumers can
//! validate exact ownership, source, provenance, and subject derivation.

use rootlight_ids::{
    ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId, content_hash, derive_fact,
};
use serde::{Deserialize, Serialize};
use std::fmt;

use crate::{
    EncodedSourceTextProjection, SignatureSourcePart, SourceTextEncoding, SourceTextProjection,
};
use crate::{ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef};

/// Namespace of the first-party lexical evidence extension.
pub const LEXICAL_EXTENSION_NAMESPACE: &str = "rootlight.lexical";
/// Payload version for contiguous first-party lexical evidence.
pub const LEXICAL_EXTENSION_VERSION: &str = "1";
/// Lexical envelope version for complete source-fragment projections.
pub const LEXICAL_PROJECTION_VERSION: &str = "2";
/// Lexical envelope version for bounded batches of source-linked summaries.
pub const LEXICAL_SUMMARY_BATCH_VERSION: &str = "3";
/// Lexical envelope version for strictly transcoded original-byte projections.
pub const LEXICAL_ENCODED_PROJECTION_VERSION: &str = "4";
/// Lexical envelope version for independently source-linked signature batches.
pub const LEXICAL_SIGNATURE_BATCH_VERSION: &str = "5";
/// Maximum signatures in one independently validated lexical envelope.
pub const MAX_LEXICAL_SIGNATURE_BATCH_ENTRIES: usize = 128;
/// Maximum canonical JSON bytes in one signature batch, before caller limits.
pub const MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES: usize = 1024 * 1024;
/// Maximum summaries in one independently validated lexical envelope.
pub const MAX_LEXICAL_SUMMARY_BATCH_ENTRIES: usize = 128;
/// Maximum retained UTF-8 bytes in one signature.
pub const MAX_LEXICAL_SIGNATURE_BYTES: usize = 4 * 1024;
/// Maximum retained UTF-8 bytes in one documentation or comment summary.
pub const MAX_LEXICAL_SUMMARY_BYTES: usize = 512;
/// Maximum canonical JSON bytes in one lexical extension payload.
///
/// The bound covers worst-case JSON escaping of a contiguous 4 KiB signature
/// plus fixed metadata. Projections also charge their source mappings against
/// this unchanged bound and fail encoding rather than dropping fragments.
pub const MAX_LEXICAL_PAYLOAD_BYTES: usize = 25 * 1024;

const LEXICAL_FACT_DOMAIN: &str = "rootlight.lexical/v1";
const LEXICAL_SUMMARY_BATCH_DOMAIN: &str = "rootlight.lexical.summary-batch/v1";
const LEXICAL_SIGNATURE_BATCH_DOMAIN: &str = "rootlight.lexical.signature-batch/v1";
const MAX_LEXICAL_SUMMARY_BATCH_PAYLOAD_BYTES: usize = 1024 * 1024;
const MAX_UTF8_BOUNDARY_BACKTRACK: usize = 3;

/// Closed evidence classes retained by lexical indexing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LexicalEvidenceKind {
    /// A source-language declaration signature.
    Signature,
    /// A bounded plain-text documentation summary supplied by an adapter.
    DocumentationSummary,
    /// A bounded plain-text comment summary supplied by an adapter.
    CommentSummary,
}

impl LexicalEvidenceKind {
    /// Returns the retained UTF-8 byte limit for this evidence class.
    #[must_use]
    pub const fn retained_byte_limit(self) -> usize {
        match self {
            Self::Signature => MAX_LEXICAL_SIGNATURE_BYTES,
            Self::DocumentationSummary | Self::CommentSummary => MAX_LEXICAL_SUMMARY_BYTES,
        }
    }

    const fn required_format(self) -> LexicalEvidenceFormat {
        match self {
            Self::Signature => LexicalEvidenceFormat::SourceText,
            Self::DocumentationSummary | Self::CommentSummary => LexicalEvidenceFormat::PlainText,
        }
    }
}

/// Closed text formats understood by the versioned lexical evidence decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LexicalEvidenceFormat {
    /// Exact source-language text without prose generation.
    SourceText,
    /// Exact source fragments joined by mapped presentation whitespace.
    SourceProjection,
    /// Adapter-supplied plain text without markup semantics.
    PlainText,
}

/// One bounded, hash-linked lexical evidence payload.
///
/// Fields are private so every constructed or decoded value satisfies its
/// byte, format, truncation, mapping and hash invariants. Contiguous payloads
/// retain their frozen version 1 encoding; exact UTF-8 projections use version 2
/// and explicitly transcoded original-byte projections use version 4.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LexicalEvidence {
    kind: LexicalEvidenceKind,
    subject: FactRef,
    format: LexicalEvidenceFormat,
    text: String,
    complete_text_hash: ContentHash,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_parts: Option<Vec<SignatureSourcePart>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_encoding: Option<SourceTextEncoding>,
}

/// Compatibility name for the bounded lexical evidence value.
/// The envelope version distinguishes contiguous text from source projections.
pub type LexicalEvidenceV1 = LexicalEvidence;

impl LexicalEvidence {
    /// Creates bounded lexical evidence from the complete source-derived text.
    ///
    /// The constructor hashes the complete input, retains only a UTF-8-safe
    /// prefix, and never allocates a copy of the unbounded input.
    ///
    /// # Errors
    ///
    /// Returns [`LexicalExtensionError::EmptyText`] for empty evidence or
    /// [`LexicalExtensionError::IncompatibleFormat`] when the format does not
    /// match the selected evidence kind.
    pub fn from_complete_text(
        kind: LexicalEvidenceKind,
        subject: FactRef,
        format: LexicalEvidenceFormat,
        complete_text: &str,
    ) -> Result<Self, LexicalExtensionError> {
        if complete_text.is_empty() {
            return Err(LexicalExtensionError::EmptyText);
        }
        if format != kind.required_format() {
            return Err(LexicalExtensionError::IncompatibleFormat);
        }

        let limit = kind.retained_byte_limit();
        let (retained, truncated) = retained_prefix(complete_text, limit);
        let evidence = Self {
            kind,
            subject,
            format,
            text: retained.to_owned(),
            complete_text_hash: content_hash(complete_text.as_bytes()),
            truncated,
            source_parts: None,
            source_encoding: None,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    /// Returns the lexical evidence class.
    #[must_use]
    pub const fn kind(&self) -> LexicalEvidenceKind {
        self.kind
    }

    /// Returns the common IR fact described by this evidence.
    #[must_use]
    pub const fn subject(&self) -> FactRef {
        self.subject
    }

    /// Returns the retained text format.
    #[must_use]
    pub const fn format(&self) -> LexicalEvidenceFormat {
        self.format
    }

    /// Returns the bounded retained text, including mapped projection separators.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the hash claimed for the complete text before truncation.
    ///
    /// Values created by [`Self::from_complete_text`] hash the complete input.
    /// When [`Self::is_truncated`] is `true`, a decoder cannot independently
    /// verify the unavailable suffix; the hash remains a producer claim bound
    /// into the canonical payload and its deterministic envelope identity.
    /// Nontruncated payloads are verified against the retained text.
    #[must_use]
    pub const fn complete_text_hash(&self) -> ContentHash {
        self.complete_text_hash
    }

    /// Reports whether the complete text exceeded the retained byte limit.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Creates a complete signature from a byte-validated source projection.
    ///
    /// Its envelope uses version 2. Version 1 payload bytes remain unchanged.
    #[must_use]
    pub fn from_source_projection(subject: FactRef, projection: SourceTextProjection) -> Self {
        Self {
            kind: LexicalEvidenceKind::Signature,
            subject,
            format: LexicalEvidenceFormat::SourceProjection,
            complete_text_hash: content_hash(projection.text.as_bytes()),
            text: projection.text,
            truncated: false,
            source_parts: Some(projection.parts),
            source_encoding: None,
        }
    }

    /// Creates a complete UTF-8 signature from strictly decoded raw fragments.
    ///
    /// The envelope uses version 4. Fragment offsets continue to refer to the
    /// original physical bytes; the complete-text hash covers the canonical
    /// UTF-8 presentation text, not a substituted source snapshot.
    #[must_use]
    pub fn from_encoded_source_projection(
        subject: FactRef,
        projection: EncodedSourceTextProjection,
    ) -> Self {
        Self {
            kind: LexicalEvidenceKind::Signature,
            subject,
            format: LexicalEvidenceFormat::SourceProjection,
            complete_text_hash: content_hash(projection.text.as_bytes()),
            text: projection.text,
            truncated: false,
            source_parts: Some(projection.parts),
            source_encoding: Some(projection.encoding),
        }
    }

    /// Returns original-source byte mappings relative to the direct source.
    ///
    /// `None` identifies legacy contiguous text. Projection mappings cover all
    /// retained bytes except the single presentation space between fragments.
    /// Version 4 uses the explicit physical encoding for differing byte widths.
    #[must_use]
    pub fn source_parts(&self) -> Option<&[SignatureSourcePart]> {
        self.source_parts.as_deref()
    }

    /// Returns the explicit physical encoding of a version 4 projection.
    ///
    /// `None` preserves the existing UTF-8 contiguous or projected contract;
    /// it does not mean that an unknown encoding was accepted.
    #[must_use]
    pub const fn source_encoding(&self) -> Option<SourceTextEncoding> {
        self.source_encoding
    }

    /// Verifies retained fragments against the complete direct source text.
    ///
    /// Envelope decoding verifies self-consistency, not the producer's source
    /// claim. Call this when the generation-bound source bytes are available.
    ///
    /// # Errors
    ///
    /// Rejects mismatching or out-of-bounds fragments. Contiguous evidence is
    /// checked against its complete-text hash and retained prefix.
    pub fn verify_source_text(&self, source: &str) -> Result<(), LexicalExtensionError> {
        if self.source_encoding.is_some() {
            return Err(LexicalExtensionError::IncompatibleFormat);
        }
        if let Some(parts) = &self.source_parts {
            for part in parts {
                let range = part.source_range();
                let start = usize::try_from(range.start)
                    .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?;
                let end = usize::try_from(range.end)
                    .map_err(|_| LexicalExtensionError::InvalidSourceProjection)?;
                if source.get(start..end) != self.text.get(part.text_range()) {
                    return Err(LexicalExtensionError::SourceMismatch);
                }
            }
        } else if content_hash(source.as_bytes()) != self.complete_text_hash
            || !source.starts_with(&self.text)
        {
            return Err(LexicalExtensionError::SourceMismatch);
        }
        Ok(())
    }

    /// Verifies retained text against the exact original direct-source bytes.
    ///
    /// For version 4, this strictly decodes only mapped fragments using their
    /// explicit encoding and compares every scalar without allocating decoded
    /// source storage. Work is bounded by the existing signature and fragment
    /// caps, even when omitted source spans are large. The caller independently
    /// binds this slice to the envelope's original source hash and validates the
    /// full-file encoding; a declaration slice need not carry a BOM.
    ///
    /// # Errors
    ///
    /// Rejects odd-length UTF-16 slices, malformed or mismatching mapped bytes,
    /// invalid ranges, and legacy input that is not exact UTF-8 source text.
    pub fn verify_source_bytes(&self, source: &[u8]) -> Result<(), LexicalExtensionError> {
        if let Some(encoding) = self.source_encoding {
            let parts = self
                .source_parts
                .as_deref()
                .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
            crate::source_projection::verify_encoded_parts(source, &self.text, parts, encoding)
        } else {
            let text =
                std::str::from_utf8(source).map_err(|_| LexicalExtensionError::SourceMismatch)?;
            self.verify_source_text(text)
        }
    }

    const fn envelope_version(&self) -> &'static str {
        if self.source_encoding.is_some() {
            LEXICAL_ENCODED_PROJECTION_VERSION
        } else if self.source_parts.is_some() {
            LEXICAL_PROJECTION_VERSION
        } else {
            LEXICAL_EXTENSION_VERSION
        }
    }

    fn from_wire(wire: WireLexicalEvidenceV1) -> Result<Self, LexicalExtensionError> {
        let evidence = Self {
            kind: wire.kind,
            subject: wire.subject.into(),
            format: wire.format,
            text: wire.text,
            complete_text_hash: wire.complete_text_hash,
            truncated: wire.truncated,
            source_parts: wire.source_parts,
            source_encoding: wire.source_encoding,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), LexicalExtensionError> {
        if self.text.is_empty() {
            return Err(LexicalExtensionError::EmptyText);
        }
        if self.source_parts.is_some() {
            if self.kind != LexicalEvidenceKind::Signature
                || self.format != LexicalEvidenceFormat::SourceProjection
                || self.truncated
            {
                return Err(LexicalExtensionError::InvalidSourceProjection);
            }
        } else if self.source_encoding.is_some() || self.format != self.kind.required_format() {
            return Err(LexicalExtensionError::IncompatibleFormat);
        }

        let limit = self.kind.retained_byte_limit();
        let observed = self.text.len();
        if observed > limit {
            return Err(LexicalExtensionError::RetainedTextTooLarge { observed, limit });
        }
        if self.truncated {
            let minimum = limit.saturating_sub(MAX_UTF8_BOUNDARY_BACKTRACK);
            if observed < minimum {
                return Err(LexicalExtensionError::InvalidTruncation);
            }
        } else if content_hash(self.text.as_bytes()) != self.complete_text_hash {
            return Err(LexicalExtensionError::CompleteTextHashMismatch);
        }
        if let Some(parts) = &self.source_parts {
            if self.source_encoding.is_some() {
                crate::source_projection::validate_encoded_parts(&self.text, parts)?;
            } else {
                crate::source_projection::validate_parts(&self.text, parts)?;
            }
        }
        Ok(())
    }
}

impl fmt::Debug for LexicalEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LexicalEvidence")
            .field("kind", &self.kind)
            .field("subject", &self.subject)
            .field("format", &self.format)
            .field("text_bytes", &self.text.len())
            .field("complete_text_hash", &self.complete_text_hash)
            .field("truncated", &self.truncated)
            .field("source_parts", &self.source_parts)
            .field("source_encoding", &self.source_encoding)
            .finish()
    }
}

pub(crate) fn rebind_lexical_evidence_subject(
    evidence: &LexicalEvidenceV1,
    subject: FactRef,
) -> Result<LexicalEvidenceV1, LexicalExtensionError> {
    let mut rebound = evidence.clone();
    rebound.subject = subject;
    rebound.validate()?;
    Ok(rebound)
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for LexicalEvidenceV1 {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LexicalEvidenceV1".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let signature = lexical_schema_variant(
            generator,
            "signature",
            "source_text",
            MAX_LEXICAL_SIGNATURE_BYTES,
        );
        let documentation = lexical_schema_variant(
            generator,
            "documentation_summary",
            "plain_text",
            MAX_LEXICAL_SUMMARY_BYTES,
        );
        let comment = lexical_schema_variant(
            generator,
            "comment_summary",
            "plain_text",
            MAX_LEXICAL_SUMMARY_BYTES,
        );
        schemars::json_schema!({
            "title": "Rootlight lexical evidence extension version 1",
            "description": "Bounded source-derived lexical evidence. Runtime decoding additionally enforces UTF-8 byte limits, canonical JSON, and nontruncated complete-text hashes.",
            "oneOf": [signature, documentation, comment]
        })
    }
}

/// JSON Schema view for source-projection lexical envelope payload version 2.
#[cfg(feature = "schema")]
#[derive(Debug)]
pub struct LexicalProjectionSchema;

#[cfg(feature = "schema")]
impl schemars::JsonSchema for LexicalProjectionSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LexicalProjectionV2".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut projection = lexical_schema_variant(
            generator,
            "signature",
            "source_projection",
            MAX_LEXICAL_SIGNATURE_BYTES,
        );
        let part_schema = generator.subschema_for::<SignatureSourcePart>();
        let projection_object = projection
            .as_object_mut()
            .expect("lexical schema is an object");
        projection_object
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
            .expect("lexical variant properties are an object")
            .insert(
                "source_parts".to_owned(),
                serde_json::json!({
                    "type": "array", "minItems": 1, "maxItems": crate::MAX_SIGNATURE_SOURCE_PARTS,
                    "items": part_schema,
                }),
            );
        projection_object
            .get_mut("required")
            .and_then(serde_json::Value::as_array_mut)
            .expect("lexical variant required fields are an array")
            .push(serde_json::json!("source_parts"));
        projection_object
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
            .expect("lexical variant properties are an object")
            .insert("truncated".to_owned(), serde_json::json!({"const": false}));
        projection
    }
}

/// JSON Schema view for transcoded lexical envelope payload version 4.
#[cfg(feature = "schema")]
#[derive(Debug)]
pub struct EncodedLexicalProjectionSchema;

#[cfg(feature = "schema")]
impl schemars::JsonSchema for EncodedLexicalProjectionSchema {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EncodedLexicalProjectionV4".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut projection =
            <LexicalProjectionSchema as schemars::JsonSchema>::json_schema(generator);
        let encoding = generator.subschema_for::<SourceTextEncoding>();
        let projection_object = projection
            .as_object_mut()
            .expect("lexical schema is an object");
        projection_object
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
            .expect("lexical variant properties are an object")
            .insert("source_encoding".to_owned(), serde_json::json!(encoding));
        projection_object
            .get_mut("required")
            .and_then(serde_json::Value::as_array_mut)
            .expect("lexical variant required fields are an array")
            .push(serde_json::json!("source_encoding"));
        projection
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireLexicalEvidenceV1 {
    kind: LexicalEvidenceKind,
    subject: WireFactRef,
    format: LexicalEvidenceFormat,
    text: String,
    complete_text_hash: ContentHash,
    truncated: bool,
    #[serde(default)]
    source_parts: Option<Vec<SignatureSourcePart>>,
    #[serde(default)]
    source_encoding: Option<SourceTextEncoding>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "id",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireFactRef {
    File(FileId),
    Entity(SymbolId),
    Fact(FactId),
}

impl From<WireFactRef> for FactRef {
    fn from(value: WireFactRef) -> Self {
        match value {
            WireFactRef::File(id) => Self::File(id),
            WireFactRef::Entity(id) => Self::Entity(id),
            WireFactRef::Fact(id) => Self::Fact(id),
        }
    }
}

#[derive(Serialize)]
struct LexicalEnvelopeIdentity<'a> {
    namespace: &'static str,
    version: &'a str,
    repository: RepositoryId,
    generation: GenerationId,
    subject: FactRef,
    source: &'a SourceRef,
    provenance: FactId,
    payload: &'a str,
}

/// Encodes lexical evidence as canonical, compact JSON.
///
/// # Errors
///
/// Returns a typed error if the value violates its invariant, serialization
/// fails, or the encoded payload exceeds [`MAX_LEXICAL_PAYLOAD_BYTES`].
pub fn encode_lexical_evidence(
    evidence: &LexicalEvidenceV1,
) -> Result<String, LexicalExtensionError> {
    evidence.validate()?;
    let payload =
        serde_json::to_string(evidence).map_err(|_| LexicalExtensionError::PayloadEncoding)?;
    require_payload_bound(&payload)?;
    Ok(payload)
}

/// Decodes a contiguous or projected lexical payload from canonical JSON.
///
/// The payload byte bound is checked before JSON decoding can allocate its text
/// field. Direct Serde decoding into [`LexicalEvidenceV1`] is intentionally
/// unavailable, so untrusted input must cross this bounded entry point.
///
/// Complete-text hashes are verified when the payload is not truncated. A
/// truncated payload cannot prove the hash of the unavailable suffix and
/// therefore carries the producer's hash claim.
///
/// # Errors
///
/// Rejects oversized, malformed, noncanonical, nontruncated hash-inconsistent,
/// or otherwise invalid payloads with [`LexicalExtensionError`].
///
/// Direct Serde decoding cannot bypass the payload preflight:
///
/// ```compile_fail
/// use rootlight_ir::LexicalEvidenceV1;
///
/// fn decode_without_preflight(raw: &str) {
///     let _: LexicalEvidenceV1 = serde_json::from_str(raw).unwrap();
/// }
/// ```
pub fn decode_lexical_evidence(payload: &str) -> Result<LexicalEvidenceV1, LexicalExtensionError> {
    require_payload_bound(payload)?;
    let wire: WireLexicalEvidenceV1 =
        serde_json::from_str(payload).map_err(|_| LexicalExtensionError::MalformedPayload)?;
    let evidence = LexicalEvidenceV1::from_wire(wire)?;
    if encode_lexical_evidence(&evidence)? != payload {
        return Err(LexicalExtensionError::NoncanonicalPayload);
    }
    Ok(evidence)
}

/// Builds a deterministic noncritical envelope for lexical evidence.
///
/// The envelope carries one direct source and one derivation reference to the
/// common subject, so removing the extension cannot remove common IR evidence.
///
/// # Errors
///
/// Returns a typed error for invalid payloads, source ownership mismatches, or
/// file-subject/source mismatches.
pub fn new_lexical_evidence_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    evidence: &LexicalEvidenceV1,
) -> Result<ExtensionEnvelope, LexicalExtensionError> {
    validate_source_owner(repository, generation, &source)?;
    validate_file_subject_source(evidence.subject, &source)?;
    validate_projection_source(evidence, &source)?;
    let payload = encode_lexical_evidence(evidence)?;
    let id = derive_envelope_id(
        repository,
        generation,
        evidence.subject,
        &source,
        provenance,
        &payload,
        evidence.envelope_version(),
    )?;
    Ok(ExtensionEnvelope {
        id,
        repository,
        generation,
        namespace: LEXICAL_EXTENSION_NAMESPACE.to_owned(),
        version: evidence.envelope_version().to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![evidence.subject],
        },
    })
}

/// Decodes and validates a self-consistent lexical extension envelope.
///
/// # Errors
///
/// Rejects an unknown namespace or version, wrong criticality, invalid payload,
/// missing or mismatched source and subject evidence, ownership mismatch, or
/// noncanonical deterministic identity.
pub fn decode_lexical_evidence_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<LexicalEvidenceV1, LexicalExtensionError> {
    validate_extension_identity(envelope)?;
    let evidence = decode_lexical_evidence(&envelope.payload)?;
    if envelope.version != evidence.envelope_version() {
        return Err(LexicalExtensionError::UnsupportedVersion);
    }
    let source = envelope
        .evidence
        .source
        .as_ref()
        .ok_or(LexicalExtensionError::MissingDirectSourceEvidence)?;
    validate_source_owner(envelope.repository, envelope.generation, source)?;
    validate_file_subject_source(evidence.subject, source)?;
    validate_projection_source(&evidence, source)?;
    if envelope.evidence.derivation.len() != 1
        || envelope.evidence.derivation.first() != Some(&evidence.subject)
    {
        return Err(LexicalExtensionError::SubjectEvidenceMismatch);
    }
    let expected_id = derive_envelope_id(
        envelope.repository,
        envelope.generation,
        evidence.subject,
        source,
        envelope.provenance,
        &envelope.payload,
        evidence.envelope_version(),
    )?;
    if envelope.id != expected_id {
        return Err(LexicalExtensionError::EnvelopeIdentityMismatch);
    }
    Ok(evidence)
}

/// Validates a lexical envelope against caller-owned subject metadata.
///
/// This strengthens [`decode_lexical_evidence_envelope`] when a consumer has
/// already resolved the common subject, direct source, and provenance record.
///
/// # Errors
///
/// Returns a typed mismatch error when any expected value differs, in addition
/// to all errors returned by [`decode_lexical_evidence_envelope`].
pub fn validate_lexical_evidence_envelope(
    envelope: &ExtensionEnvelope,
    expected_subject: FactRef,
    expected_source: &SourceRef,
    expected_provenance: FactId,
) -> Result<LexicalEvidenceV1, LexicalExtensionError> {
    let evidence = decode_lexical_evidence_envelope(envelope)?;
    if evidence.subject != expected_subject {
        return Err(LexicalExtensionError::SubjectMismatch);
    }
    if envelope.evidence.source.as_ref() != Some(expected_source) {
        return Err(LexicalExtensionError::SourceMismatch);
    }
    if envelope.provenance != expected_provenance {
        return Err(LexicalExtensionError::ProvenanceMismatch);
    }
    Ok(evidence)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LexicalSummaryBatchEntry {
    source: SourceRef,
    payload: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LexicalSummaryBatchPayload {
    entries: Vec<LexicalSummaryBatchEntry>,
}

#[derive(Serialize)]
struct LexicalSummaryBatchIdentity<'a> {
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    payload: &'a str,
}

/// Packs a bounded set of complete source-linked summaries into one envelope.
///
/// Every child remains independently reconstructible with its original source,
/// provenance, subject, payload, and deterministic identity. This legacy version
/// remains summary-only; entity signatures use the separate version 5 codec.
///
/// # Errors
///
/// Rejects empty, oversized, mixed-owner, cross-file, malformed, or signature
/// inputs without producing a partially populated batch.
pub fn new_lexical_summary_batch_envelope(
    entries: &[ExtensionEnvelope],
) -> Result<ExtensionEnvelope, LexicalExtensionError> {
    if entries.is_empty() || entries.len() > MAX_LEXICAL_SUMMARY_BATCH_ENTRIES {
        return Err(LexicalExtensionError::MalformedPayload);
    }
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|entry| entry.id);
    let first = ordered[0];
    let first_source = first
        .evidence
        .source
        .as_ref()
        .ok_or(LexicalExtensionError::MissingDirectSourceEvidence)?;
    let file = first_source.span().file();
    let mut previous_id = None;
    let mut payload_entries = Vec::with_capacity(ordered.len());
    let mut derivation = Vec::with_capacity(ordered.len());
    for entry in ordered {
        if previous_id.replace(entry.id) == Some(entry.id)
            || entry.repository != first.repository
            || entry.generation != first.generation
            || entry.provenance != first.provenance
        {
            return Err(LexicalExtensionError::EnvelopeIdentityMismatch);
        }
        let evidence = decode_lexical_evidence_envelope(entry)?;
        if evidence.kind() == LexicalEvidenceKind::Signature {
            return Err(LexicalExtensionError::IncompatibleFormat);
        }
        let source = entry
            .evidence
            .source
            .as_ref()
            .ok_or(LexicalExtensionError::MissingDirectSourceEvidence)?;
        if source.span().file() != file {
            return Err(LexicalExtensionError::FileSubjectSourceMismatch);
        }
        payload_entries.push(LexicalSummaryBatchEntry {
            source: source.clone(),
            payload: entry.payload.clone(),
        });
        derivation.push(evidence.subject());
    }
    derivation.sort_unstable();
    derivation.dedup();
    let payload = serde_json::to_string(&LexicalSummaryBatchPayload {
        entries: payload_entries,
    })
    .map_err(|_| LexicalExtensionError::PayloadEncoding)?;
    if payload.len() > MAX_LEXICAL_SUMMARY_BATCH_PAYLOAD_BYTES {
        return Err(LexicalExtensionError::PayloadTooLarge {
            observed: payload.len(),
            limit: MAX_LEXICAL_SUMMARY_BATCH_PAYLOAD_BYTES,
        });
    }
    let id = derive_lexical_summary_batch_id(
        first.repository,
        first.generation,
        first.provenance,
        &payload,
    )?;
    Ok(ExtensionEnvelope {
        id,
        repository: first.repository,
        generation: first.generation,
        namespace: LEXICAL_EXTENSION_NAMESPACE.to_owned(),
        version: LEXICAL_SUMMARY_BATCH_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance: first.provenance,
        evidence: FactEvidence {
            source: Some(first_source.clone()),
            derivation,
        },
    })
}

/// Reconstructs and validates every independent summary in one batch.
///
/// # Errors
///
/// Rejects noncanonical encoding, altered source or derivation, wrong ownership,
/// an invalid child, or a mismatched deterministic identity.
pub fn decode_lexical_summary_batch_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<Vec<ExtensionEnvelope>, LexicalExtensionError> {
    if envelope.namespace != LEXICAL_EXTENSION_NAMESPACE {
        return Err(LexicalExtensionError::UnsupportedNamespace);
    }
    if envelope.version != LEXICAL_SUMMARY_BATCH_VERSION {
        return Err(LexicalExtensionError::UnsupportedVersion);
    }
    if envelope.criticality != ExtensionCriticality::Noncritical {
        return Err(LexicalExtensionError::WrongCriticality);
    }
    if envelope.payload.len() > MAX_LEXICAL_SUMMARY_BATCH_PAYLOAD_BYTES {
        return Err(LexicalExtensionError::PayloadTooLarge {
            observed: envelope.payload.len(),
            limit: MAX_LEXICAL_SUMMARY_BATCH_PAYLOAD_BYTES,
        });
    }
    let payload: LexicalSummaryBatchPayload = serde_json::from_str(&envelope.payload)
        .map_err(|_| LexicalExtensionError::MalformedPayload)?;
    if payload.entries.is_empty() || payload.entries.len() > MAX_LEXICAL_SUMMARY_BATCH_ENTRIES {
        return Err(LexicalExtensionError::MalformedPayload);
    }
    let mut entries = Vec::with_capacity(payload.entries.len());
    for entry in payload.entries {
        validate_source_owner(envelope.repository, envelope.generation, &entry.source)?;
        let evidence = decode_lexical_evidence(&entry.payload)?;
        let child = new_lexical_evidence_envelope(
            envelope.repository,
            envelope.generation,
            envelope.provenance,
            entry.source,
            &evidence,
        )?;
        if evidence.kind() == LexicalEvidenceKind::Signature {
            return Err(LexicalExtensionError::IncompatibleFormat);
        }
        entries.push(child);
    }
    let canonical = new_lexical_summary_batch_envelope(&entries)?;
    if canonical.payload != envelope.payload {
        return Err(LexicalExtensionError::NoncanonicalPayload);
    }
    if canonical.id != envelope.id
        || canonical.evidence != envelope.evidence
        || canonical.repository != envelope.repository
        || canonical.generation != envelope.generation
        || canonical.provenance != envelope.provenance
    {
        return Err(LexicalExtensionError::EnvelopeIdentityMismatch);
    }
    Ok(entries)
}

fn derive_lexical_summary_batch_id(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    payload: &str,
) -> Result<FactId, LexicalExtensionError> {
    let identity = LexicalSummaryBatchIdentity {
        repository,
        generation,
        provenance,
        payload,
    };
    let bytes =
        serde_json::to_vec(&identity).map_err(|_| LexicalExtensionError::PayloadEncoding)?;
    Ok(derive_fact(LEXICAL_SUMMARY_BATCH_DOMAIN, &bytes).id())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LexicalSignatureBatchPayload {
    #[serde(deserialize_with = "bounded_signature_entries")]
    entries: Vec<LexicalSummaryBatchEntry>,
}

fn bounded_signature_entries<'de, D>(
    deserializer: D,
) -> Result<Vec<LexicalSummaryBatchEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<LexicalSummaryBatchEntry>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("one to 128 signature entries")
        }

        fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::SeqAccess<'de>,
        {
            let mut entries = Vec::new();
            entries
                .try_reserve_exact(MAX_LEXICAL_SIGNATURE_BATCH_ENTRIES)
                .map_err(serde::de::Error::custom)?;
            loop {
                if entries.len() == MAX_LEXICAL_SIGNATURE_BATCH_ENTRIES {
                    if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                        return Err(serde::de::Error::custom("too many signature entries"));
                    }
                    break;
                }
                let Some(entry) = sequence.next_element()? else {
                    break;
                };
                entries.push(entry);
            }
            if entries.is_empty() {
                return Err(serde::de::Error::custom("empty signature batch"));
            }
            Ok(entries)
        }
    }
    deserializer.deserialize_seq(Entries)
}

/// Packs complete entity signatures without substituting one child's source.
///
/// Children must belong to one file, repository, generation and producer. The
/// canonical order uses child identity; physical partitioning belongs to the
/// caller. Summary version 3 remains summary-only and byte-compatible.
///
/// # Errors
/// Rejects empty, oversized, duplicate, truncated, non-entity, mixed-owner or
/// malformed children. No partial batch is returned.
pub fn new_lexical_signature_batch_envelope(
    entries: &[ExtensionEnvelope],
) -> Result<ExtensionEnvelope, LexicalExtensionError> {
    if entries.is_empty() || entries.len() > MAX_LEXICAL_SIGNATURE_BATCH_ENTRIES {
        return Err(LexicalExtensionError::MalformedPayload);
    }
    let mut ordered = entries.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by_key(|entry| entry.id);
    let first = ordered[0];
    let first_source = first
        .evidence
        .source
        .as_ref()
        .ok_or(LexicalExtensionError::MissingDirectSourceEvidence)?;
    let mut previous_id = None;
    let mut payload_entries = Vec::with_capacity(ordered.len());
    let mut derivation = Vec::with_capacity(ordered.len());
    for entry in ordered {
        if previous_id.replace(entry.id) == Some(entry.id)
            || entry.repository != first.repository
            || entry.generation != first.generation
            || entry.provenance != first.provenance
        {
            return Err(LexicalExtensionError::EnvelopeIdentityMismatch);
        }
        let evidence = decode_lexical_evidence_envelope(entry)?;
        if evidence.kind() != LexicalEvidenceKind::Signature
            || evidence.is_truncated()
            || !matches!(evidence.subject(), FactRef::Entity(_))
        {
            return Err(LexicalExtensionError::IncompatibleFormat);
        }
        let source = entry
            .evidence
            .source
            .as_ref()
            .ok_or(LexicalExtensionError::MissingDirectSourceEvidence)?;
        if source.span().file() != first_source.span().file()
            || source.content_hash() != first_source.content_hash()
        {
            return Err(LexicalExtensionError::FileSubjectSourceMismatch);
        }
        payload_entries.push(LexicalSummaryBatchEntry {
            source: source.clone(),
            payload: entry.payload.clone(),
        });
        derivation.push(evidence.subject());
    }
    derivation.sort_unstable();
    derivation.dedup();
    let payload = serde_json::to_string(&LexicalSignatureBatchPayload {
        entries: payload_entries,
    })
    .map_err(|_| LexicalExtensionError::PayloadEncoding)?;
    if payload.len() > MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES {
        return Err(LexicalExtensionError::PayloadTooLarge {
            observed: payload.len(),
            limit: MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES,
        });
    }
    let identity = LexicalSummaryBatchIdentity {
        repository: first.repository,
        generation: first.generation,
        provenance: first.provenance,
        payload: &payload,
    };
    let bytes =
        serde_json::to_vec(&identity).map_err(|_| LexicalExtensionError::PayloadEncoding)?;
    let id = derive_fact(LEXICAL_SIGNATURE_BATCH_DOMAIN, &bytes).id();
    Ok(ExtensionEnvelope {
        id,
        repository: first.repository,
        generation: first.generation,
        namespace: LEXICAL_EXTENSION_NAMESPACE.to_owned(),
        version: LEXICAL_SIGNATURE_BATCH_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance: first.provenance,
        evidence: FactEvidence {
            source: Some(first_source.clone()),
            derivation,
        },
    })
}

/// Reconstructs every signature child and authenticates the entire batch.
///
/// # Errors
/// Rejects invalid children, noncanonical order, altered source, derivation or
/// ownership, and any mismatched batch identity.
pub fn decode_lexical_signature_batch_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<Vec<ExtensionEnvelope>, LexicalExtensionError> {
    decode_lexical_signature_batch_envelope_with_checkpoint(envelope, || true)
}

/// Validates a signature batch with cooperative checks around each child.
///
/// # Errors
/// Returns the ordinary batch errors, or [`LexicalExtensionError::Interrupted`]
/// when the caller stops decoding. No prefix is published.
pub fn decode_lexical_signature_batch_envelope_with_checkpoint(
    envelope: &ExtensionEnvelope,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<Vec<ExtensionEnvelope>, LexicalExtensionError> {
    if !checkpoint() {
        return Err(LexicalExtensionError::Interrupted);
    }
    if envelope.namespace != LEXICAL_EXTENSION_NAMESPACE {
        return Err(LexicalExtensionError::UnsupportedNamespace);
    }
    if envelope.version != LEXICAL_SIGNATURE_BATCH_VERSION {
        return Err(LexicalExtensionError::UnsupportedVersion);
    }
    if envelope.criticality != ExtensionCriticality::Noncritical {
        return Err(LexicalExtensionError::WrongCriticality);
    }
    if envelope.payload.len() > MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES {
        return Err(LexicalExtensionError::PayloadTooLarge {
            observed: envelope.payload.len(),
            limit: MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES,
        });
    }
    let payload: LexicalSignatureBatchPayload = serde_json::from_str(&envelope.payload)
        .map_err(|_| LexicalExtensionError::MalformedPayload)?;
    let mut entries = Vec::with_capacity(payload.entries.len());
    for entry in payload.entries {
        if !checkpoint() {
            return Err(LexicalExtensionError::Interrupted);
        }
        validate_source_owner(envelope.repository, envelope.generation, &entry.source)?;
        let evidence = decode_lexical_evidence(&entry.payload)?;
        entries.push(new_lexical_evidence_envelope(
            envelope.repository,
            envelope.generation,
            envelope.provenance,
            entry.source,
            &evidence,
        )?);
    }
    if !checkpoint() {
        return Err(LexicalExtensionError::Interrupted);
    }
    let canonical = new_lexical_signature_batch_envelope(&entries)?;
    if canonical != *envelope {
        return Err(LexicalExtensionError::EnvelopeIdentityMismatch);
    }
    if !checkpoint() {
        return Err(LexicalExtensionError::Interrupted);
    }
    Ok(entries)
}

/// Bounds temporary reconstruction storage before a query decodes a batch.
///
/// The charge includes encoded strings, canonical re-encoding, identity bytes,
/// reconstructed children, decoded evidence and bounded metadata vectors. It
/// is a conservative workspace allowance, not an allocator or RSS measurement.
///
/// # Errors
/// Rejects a payload over the batch cap or arithmetic overflow.
pub fn lexical_signature_batch_decode_workspace_bytes(
    envelope: &ExtensionEnvelope,
) -> Result<usize, LexicalExtensionError> {
    if envelope.payload.len() > MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES {
        return Err(LexicalExtensionError::PayloadTooLarge {
            observed: envelope.payload.len(),
            limit: MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES,
        });
    }
    let fixed = std::mem::size_of::<LexicalSummaryBatchEntry>()
        + std::mem::size_of::<ExtensionEnvelope>() * 2
        + std::mem::size_of::<LexicalEvidence>()
        + std::mem::size_of::<FactRef>() * 2;
    envelope
        .payload
        .len()
        .checked_mul(8)
        .and_then(|bytes| {
            fixed
                .checked_mul(MAX_LEXICAL_SIGNATURE_BATCH_ENTRIES)
                .and_then(|fixed| bytes.checked_add(fixed))
        })
        .ok_or(LexicalExtensionError::PayloadEncoding)
}

fn retained_prefix(input: &str, limit: usize) -> (&str, bool) {
    if input.len() <= limit {
        return (input, false);
    }
    let mut end = limit;
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    (&input[..end], true)
}

fn require_payload_bound(payload: &str) -> Result<(), LexicalExtensionError> {
    let observed = payload.len();
    if observed > MAX_LEXICAL_PAYLOAD_BYTES {
        Err(LexicalExtensionError::PayloadTooLarge {
            observed,
            limit: MAX_LEXICAL_PAYLOAD_BYTES,
        })
    } else {
        Ok(())
    }
}

fn validate_extension_identity(envelope: &ExtensionEnvelope) -> Result<(), LexicalExtensionError> {
    if envelope.namespace != LEXICAL_EXTENSION_NAMESPACE {
        return Err(LexicalExtensionError::UnsupportedNamespace);
    }
    if envelope.version != LEXICAL_EXTENSION_VERSION
        && envelope.version != LEXICAL_PROJECTION_VERSION
        && envelope.version != LEXICAL_ENCODED_PROJECTION_VERSION
    {
        return Err(LexicalExtensionError::UnsupportedVersion);
    }
    if envelope.criticality != ExtensionCriticality::Noncritical {
        return Err(LexicalExtensionError::WrongCriticality);
    }
    Ok(())
}

fn validate_source_owner(
    repository: RepositoryId,
    generation: GenerationId,
    source: &SourceRef,
) -> Result<(), LexicalExtensionError> {
    if source.repository() != repository {
        return Err(LexicalExtensionError::RepositoryMismatch);
    }
    if source.generation() != generation {
        return Err(LexicalExtensionError::GenerationMismatch);
    }
    Ok(())
}

fn validate_file_subject_source(
    subject: FactRef,
    source: &SourceRef,
) -> Result<(), LexicalExtensionError> {
    if let FactRef::File(file) = subject
        && source.span().file() != file
    {
        return Err(LexicalExtensionError::FileSubjectSourceMismatch);
    }
    Ok(())
}

fn derive_envelope_id(
    repository: RepositoryId,
    generation: GenerationId,
    subject: FactRef,
    source: &SourceRef,
    provenance: FactId,
    payload: &str,
    version: &str,
) -> Result<FactId, LexicalExtensionError> {
    let identity = LexicalEnvelopeIdentity {
        namespace: LEXICAL_EXTENSION_NAMESPACE,
        version,
        repository,
        generation,
        subject,
        source,
        provenance,
        payload,
    };
    let bytes =
        serde_json::to_vec(&identity).map_err(|_| LexicalExtensionError::PayloadEncoding)?;
    Ok(derive_fact(LEXICAL_FACT_DOMAIN, &bytes).id())
}

fn validate_projection_source(
    evidence: &LexicalEvidenceV1,
    source: &SourceRef,
) -> Result<(), LexicalExtensionError> {
    if evidence.source_encoding.is_some()
        && (!source.span().start_byte().is_multiple_of(2)
            || !source.span().end_byte().is_multiple_of(2))
    {
        return Err(LexicalExtensionError::InvalidSourceProjection);
    }
    if let Some(parts) = &evidence.source_parts {
        let length = source
            .span()
            .end_byte()
            .checked_sub(source.span().start_byte())
            .ok_or(LexicalExtensionError::InvalidSourceProjection)?;
        if parts
            .last()
            .is_none_or(|part| part.source_range().end > length)
        {
            return Err(LexicalExtensionError::InvalidSourceProjection);
        }
    }
    Ok(())
}

#[cfg(feature = "schema")]
fn lexical_schema_variant(
    generator: &mut schemars::SchemaGenerator,
    kind: &'static str,
    format: &'static str,
    max_text_length: usize,
) -> schemars::Schema {
    let subject = lexical_subject_schema(generator);
    let complete_text_hash = generator.subschema_for::<ContentHash>();
    schemars::json_schema!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "const": kind
            },
            "subject": subject,
            "format": {
                "type": "string",
                "const": format
            },
            "text": {
                "type": "string",
                "minLength": 1,
                "maxLength": max_text_length,
                "x-rootlight-max-utf8-bytes": max_text_length
            },
            "complete_text_hash": complete_text_hash,
            "truncated": {
                "type": "boolean"
            }
        },
        "required": [
            "kind",
            "subject",
            "format",
            "text",
            "complete_text_hash",
            "truncated"
        ],
        "additionalProperties": false
    })
}

#[cfg(feature = "schema")]
fn lexical_subject_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let file_id = generator.subschema_for::<FileId>();
    let symbol_id = generator.subschema_for::<SymbolId>();
    let fact_id = generator.subschema_for::<FactId>();
    schemars::json_schema!({
        "description": "The common IR fact described by this evidence.",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "file" },
                    "id": file_id
                },
                "required": ["kind", "id"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "entity" },
                    "id": symbol_id
                },
                "required": ["kind", "id"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "const": "fact" },
                    "id": fact_id
                },
                "required": ["kind", "id"],
                "additionalProperties": false
            }
        ]
    })
}

/// Source-free failures for the lexical extension boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LexicalExtensionError {
    /// Caller stopped bounded reconstruction before publication.
    #[error("lexical reconstruction interrupted")]
    Interrupted,
    /// A projection was partial, unordered, oversized or outside its source.
    #[error("lexical source projection is invalid")]
    InvalidSourceProjection,
    /// Complete or retained evidence was empty.
    #[error("lexical evidence text is empty")]
    EmptyText,
    /// The selected evidence kind and text format are incompatible.
    #[error("lexical evidence kind and format are incompatible")]
    IncompatibleFormat,
    /// Retained text exceeded its evidence-kind byte limit.
    #[error("lexical evidence contains {observed} retained UTF-8 bytes, limit is {limit}")]
    RetainedTextTooLarge {
        /// Observed retained UTF-8 byte count.
        observed: usize,
        /// Evidence-kind byte limit.
        limit: usize,
    },
    /// A truncated payload could not have been produced by boundary-safe truncation.
    #[error("lexical evidence has an invalid truncation marker")]
    InvalidTruncation,
    /// Complete hash did not match nontruncated retained text.
    #[error("lexical evidence complete-text hash does not match")]
    CompleteTextHashMismatch,
    /// Canonical serialization failed.
    #[error("lexical evidence payload encoding failed")]
    PayloadEncoding,
    /// Encoded payload exceeded its hard byte limit.
    #[error("lexical payload contains {observed} UTF-8 bytes, limit is {limit}")]
    PayloadTooLarge {
        /// Observed payload UTF-8 byte count.
        observed: usize,
        /// Payload hard byte limit.
        limit: usize,
    },
    /// Payload was not valid strict lexical JSON.
    #[error("lexical evidence payload is malformed")]
    MalformedPayload,
    /// Payload JSON was semantically valid but not canonical.
    #[error("lexical evidence payload is not canonical JSON")]
    NoncanonicalPayload,
    /// Envelope namespace was not the first-party lexical namespace.
    #[error("lexical extension namespace is unsupported")]
    UnsupportedNamespace,
    /// Envelope payload version was unsupported or inconsistent with its format.
    #[error("lexical extension version is unsupported")]
    UnsupportedVersion,
    /// Envelope incorrectly marked the skippable payload critical.
    #[error("lexical extension must be noncritical")]
    WrongCriticality,
    /// Envelope omitted its mandatory direct source.
    #[error("lexical extension is missing direct source evidence")]
    MissingDirectSourceEvidence,
    /// Envelope derivation did not contain exactly its payload subject.
    #[error("lexical extension subject derivation does not match")]
    SubjectEvidenceMismatch,
    /// Direct source belonged to another repository.
    #[error("lexical extension source repository does not match its owner")]
    RepositoryMismatch,
    /// Direct source belonged to another immutable generation.
    #[error("lexical extension source generation does not match its owner")]
    GenerationMismatch,
    /// A file subject and direct source named different files.
    #[error("lexical extension file subject does not match its source")]
    FileSubjectSourceMismatch,
    /// Envelope ID did not match its canonical semantic inputs.
    #[error("lexical extension identity does not match its contents")]
    EnvelopeIdentityMismatch,
    /// Caller-resolved common subject differed from the payload subject.
    #[error("lexical extension subject does not match the expected subject")]
    SubjectMismatch,
    /// Caller-resolved direct source differed from the envelope source.
    #[error("lexical extension source does not match the expected source")]
    SourceMismatch,
    /// Caller-resolved provenance differed from the envelope provenance.
    #[error("lexical extension provenance does not match the expected provenance")]
    ProvenanceMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ExtensionSupport, IrDocument, IrLimits, NormalizedIrDocument, SourceSpan,
        UnknownNoncriticalExtensionPolicy, canonicalize_ir_document, decode_ir_document,
    };

    fn normalized_fixture() -> NormalizedIrDocument {
        let decoded = decode_ir_document(
            include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json"),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("frozen normalized IR fixture decodes");
        let IrDocument::NormalizedV1_1(document) = decoded else {
            panic!("normalized fixture must dispatch to version 1.1");
        };
        document
    }

    fn owner() -> (RepositoryId, GenerationId, FactId, SourceRef, FactRef) {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let provenance = FactId::from_bytes([3; 20]);
        let file = FileId::from_bytes([4; 20]);
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 7, 23).expect("fixture span is ordered"),
            content_hash(b"source file"),
            None,
        );
        let subject = FactRef::Entity(SymbolId::from_bytes([5; 20]));
        (repository, generation, provenance, source, subject)
    }

    fn signature(text: &str) -> LexicalEvidenceV1 {
        let (_, _, _, _, subject) = owner();
        LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::Signature,
            subject,
            LexicalEvidenceFormat::SourceText,
            text,
        )
        .expect("signature fixture is valid")
    }

    #[test]
    fn summary_batch_reconstructs_children_and_rejects_corruption() {
        let (repository, generation, provenance, source, _) = owner();
        let children = (0..3)
            .map(|index| {
                let subject = FactRef::Fact(FactId::from_bytes([index + 10; 20]));
                let evidence = LexicalEvidenceV1::from_complete_text(
                    LexicalEvidenceKind::CommentSummary,
                    subject,
                    LexicalEvidenceFormat::PlainText,
                    &format!("comment {index}"),
                )
                .unwrap();
                new_lexical_evidence_envelope(
                    repository,
                    generation,
                    provenance,
                    source.clone(),
                    &evidence,
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        let batch = new_lexical_summary_batch_envelope(&children).unwrap();
        assert_eq!(batch.version, LEXICAL_SUMMARY_BATCH_VERSION);
        let mut reconstructed = decode_lexical_summary_batch_envelope(&batch).unwrap();
        reconstructed.sort_unstable_by_key(|entry| entry.id);
        let mut expected = children.clone();
        expected.sort_unstable_by_key(|entry| entry.id);
        assert_eq!(reconstructed, expected);
        assert_eq!(
            new_lexical_summary_batch_envelope(&vec![children[0].clone(); 129]),
            Err(LexicalExtensionError::MalformedPayload)
        );

        let mut changed_payload = batch.clone();
        changed_payload.payload.push(' ');
        assert_eq!(
            decode_lexical_summary_batch_envelope(&changed_payload),
            Err(LexicalExtensionError::NoncanonicalPayload)
        );
        let mut changed_derivation = batch.clone();
        changed_derivation.evidence.derivation.pop();
        assert_eq!(
            decode_lexical_summary_batch_envelope(&changed_derivation),
            Err(LexicalExtensionError::EnvelopeIdentityMismatch)
        );
        let mut changed_source = batch;
        changed_source.evidence.source = None;
        assert_eq!(
            decode_lexical_summary_batch_envelope(&changed_source),
            Err(LexicalExtensionError::EnvelopeIdentityMismatch)
        );

        let signature = new_lexical_evidence_envelope(
            repository,
            generation,
            provenance,
            source,
            &signature("fn counted()"),
        )
        .unwrap();
        assert_eq!(
            new_lexical_summary_batch_envelope(&[signature]),
            Err(LexicalExtensionError::IncompatibleFormat)
        );
    }

    #[test]
    fn signature_batch_preserves_complete_children_and_legacy_summary_contract() {
        let document = normalized_fixture();
        let source = document.entities[0].evidence.source.clone().unwrap();
        let children = [
            "fn header()",
            "(defun header (value))",
            "fn header(value: f32)",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let evidence = LexicalEvidenceV1::from_complete_text(
                LexicalEvidenceKind::Signature,
                FactRef::Entity(SymbolId::from_bytes([u8::try_from(index + 1).unwrap(); 20])),
                LexicalEvidenceFormat::SourceText,
                text,
            )
            .unwrap();
            new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                document.entities[0].provenance,
                source.clone(),
                &evidence,
            )
            .unwrap()
        })
        .collect::<Vec<_>>();
        let batch = new_lexical_signature_batch_envelope(&children).unwrap();
        assert_eq!(batch.version, "5");
        let mut expected = children.clone();
        expected.sort_unstable_by_key(|child| child.id);
        assert_eq!(
            decode_lexical_signature_batch_envelope(&batch).unwrap(),
            expected
        );
        let mut reordered = children.clone();
        reordered.reverse();
        assert_eq!(
            new_lexical_signature_batch_envelope(&reordered).unwrap(),
            batch
        );
        assert!(new_lexical_summary_batch_envelope(&children).is_err());
        assert!(decode_lexical_evidence_envelope(&batch).is_err());
        assert!(decode_lexical_summary_batch_envelope(&batch).is_err());
        assert!(new_lexical_signature_batch_envelope(&vec![children[0].clone(); 129]).is_err());
        assert!(
            new_lexical_signature_batch_envelope(&[children[0].clone(), children[0].clone()])
                .is_err()
        );
        let mut wrong = batch.clone();
        wrong.evidence.derivation.pop();
        assert!(decode_lexical_signature_batch_envelope(&wrong).is_err());
        let mut wrong = batch.clone();
        wrong.evidence.source = None;
        assert!(decode_lexical_signature_batch_envelope(&wrong).is_err());
        assert_eq!(
            decode_lexical_signature_batch_envelope_with_checkpoint(&batch, || false),
            Err(LexicalExtensionError::Interrupted)
        );
        let mut calls = 0;
        assert_eq!(
            decode_lexical_signature_batch_envelope_with_checkpoint(&batch, || {
                calls += 1;
                calls < 3
            }),
            Err(LexicalExtensionError::Interrupted)
        );
    }

    #[test]
    fn signature_batch_preserves_projection_and_encoded_mapping_and_rejects_hidden_corruption() {
        let document = normalized_fixture();
        let file = document.files[0].id;
        let provenance = document.entities[0].provenance;
        let raw = b"fn first() { body }\nfn second()";
        let source = SourceRef::new(
            document.repository,
            document.generation,
            crate::SourceSpan::new(file, 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            content_hash(raw),
            None,
        );
        let projection =
            SourceTextProjection::from_source(std::str::from_utf8(raw).unwrap(), &[0..10, 20..31])
                .unwrap();
        let projected = LexicalEvidenceV1::from_source_projection(
            FactRef::Entity(SymbolId::from_bytes([41; 20])),
            projection,
        );
        let first = new_lexical_evidence_envelope(
            document.repository,
            document.generation,
            provenance,
            source.clone(),
            &projected,
        )
        .unwrap();
        let batch = new_lexical_signature_batch_envelope(&[first]).unwrap();
        for child in decode_lexical_signature_batch_envelope(&batch).unwrap() {
            decode_lexical_evidence_envelope(&child)
                .unwrap()
                .verify_source_bytes(raw)
                .unwrap();
        }
        let mut payload: serde_json::Value = serde_json::from_str(&batch.payload).unwrap();
        payload["entries"][0]["payload"] = serde_json::json!("{}");
        let mut wrong = batch.clone();
        wrong.payload = serde_json::to_string(&payload).unwrap();
        assert!(decode_lexical_signature_batch_envelope(&wrong).is_err());
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let (encoded_raw, expected, child) = encoded_projection_fixture(encoding);
            let encoded_batch =
                new_lexical_signature_batch_envelope(std::slice::from_ref(&child)).unwrap();
            let children = decode_lexical_signature_batch_envelope(&encoded_batch).unwrap();
            assert_eq!(children, [child]);
            let evidence = decode_lexical_evidence_envelope(&children[0]).unwrap();
            assert_eq!(evidence, expected);
            evidence.verify_source_bytes(&encoded_raw).unwrap();
        }
        let mut oversized = batch;
        oversized.payload = "x".repeat(MAX_LEXICAL_SIGNATURE_BATCH_PAYLOAD_BYTES + 1);
        assert!(lexical_signature_batch_decode_workspace_bytes(&oversized).is_err());
    }

    #[test]
    fn projected_signatures_preserve_exact_unicode_parts_and_versioned_identity() {
        let text = "fn f <# documentation #> (雪)";
        let tail = text.find("(雪)").unwrap();
        let projection =
            SourceTextProjection::from_source(text, &[0..4, tail..text.len()]).unwrap();
        let (repository, generation, provenance, original, subject) = owner();
        let source = SourceRef::new(
            repository,
            generation,
            crate::SourceSpan::new(
                original.span().file(),
                100,
                100 + u64::try_from(text.len()).unwrap(),
            )
            .unwrap(),
            original.content_hash(),
            None,
        );
        let evidence = LexicalEvidenceV1::from_source_projection(subject, projection);
        assert_eq!(evidence.text(), "fn f (雪)");
        assert!(!evidence.is_truncated());
        assert_eq!(evidence.verify_source_text(text), Ok(()));
        assert!(evidence.verify_source_text("changed").is_err());
        let envelope =
            new_lexical_evidence_envelope(repository, generation, provenance, source, &evidence)
                .unwrap();
        assert_eq!(envelope.version, "2");
        assert_eq!(
            decode_lexical_evidence_envelope(&envelope),
            Ok(evidence.clone())
        );
        let mut wrong_version = envelope.clone();
        wrong_version.version = "1".to_owned();
        assert!(decode_lexical_evidence_envelope(&wrong_version).is_err());
        let mut outside_source = envelope.clone();
        outside_source.evidence.source = Some(original);
        assert!(decode_lexical_evidence_envelope(&outside_source).is_err());
        let mut corrupted: serde_json::Value = serde_json::from_str(&envelope.payload).unwrap();
        corrupted["source_parts"][1]["text_start"] = serde_json::json!(4);
        assert!(decode_lexical_evidence(&serde_json::to_string(&corrupted).unwrap()).is_err());
        corrupted["source_parts"][1]["text_start"] = serde_json::json!(5);
        corrupted["truncated"] = serde_json::json!(true);
        assert!(decode_lexical_evidence(&serde_json::to_string(&corrupted).unwrap()).is_err());
    }

    #[test]
    fn projected_signatures_reject_invalid_ranges_without_partial_output() {
        for ranges in [
            vec![],
            std::iter::once(0..0).collect(),
            vec![0..4, 3..5],
            vec![5..6, 0..1],
            std::iter::once(0..999).collect(),
            std::iter::once(1..2).collect(),
        ] {
            assert!(
                SourceTextProjection::from_source("雪 token", &ranges).is_err(),
                "{ranges:?}"
            );
        }
        let oversized = "x".repeat(MAX_LEXICAL_SIGNATURE_BYTES + 1);
        assert!(
            SourceTextProjection::from_source(
                &oversized,
                std::slice::from_ref(&(0..oversized.len()))
            )
            .is_err()
        );
        let source = "x".repeat(crate::MAX_SIGNATURE_SOURCE_PARTS + 1);
        let ranges: Vec<_> = (0..source.len()).map(|offset| offset..offset + 1).collect();
        assert!(SourceTextProjection::from_source(&source, &ranges).is_err());
    }

    #[test]
    fn projected_signature_mappings_share_the_existing_payload_budget() {
        let source = "\0".repeat(4_032);
        let ranges: Vec<_> = (0..crate::MAX_SIGNATURE_SOURCE_PARTS)
            .map(|index| index * 63..(index + 1) * 63)
            .collect();
        let projection = SourceTextProjection::from_source(&source, &ranges).unwrap();
        let (_, _, _, _, subject) = owner();
        let evidence = LexicalEvidence::from_source_projection(subject, projection);
        assert_eq!(evidence.text().len(), 4_095);
        assert!(!evidence.is_truncated());
        assert!(matches!(
            encode_lexical_evidence(&evidence),
            Err(LexicalExtensionError::PayloadTooLarge {
                limit: MAX_LEXICAL_PAYLOAD_BYTES,
                ..
            })
        ));
    }

    fn encoded_projection_fixture(
        encoding: SourceTextEncoding,
    ) -> (Vec<u8>, LexicalEvidenceV1, ExtensionEnvelope) {
        let text = "\u{feff}fn λ\r\n <# 雪 comment #> (😀)";
        let raw: Vec<_> = text
            .encode_utf16()
            .flat_map(|unit| match encoding {
                SourceTextEncoding::Utf16Le => unit.to_le_bytes(),
                SourceTextEncoding::Utf16Be => unit.to_be_bytes(),
            })
            .collect();
        let first_end = 2 * text[..text.find(" <#").unwrap()].encode_utf16().count();
        let tail = 2 * text[..text.find("(😀)").unwrap()].encode_utf16().count();
        let projection = EncodedSourceTextProjection::from_source(
            &raw,
            encoding,
            &[2..first_end, tail..raw.len()],
        )
        .unwrap();
        let (repository, generation, provenance, original, subject) = owner();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(original.span().file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            content_hash(&raw),
            None,
        );
        let evidence = LexicalEvidenceV1::from_encoded_source_projection(subject, projection);
        let envelope =
            new_lexical_evidence_envelope(repository, generation, provenance, source, &evidence)
                .unwrap();
        (raw, evidence, envelope)
    }

    #[test]
    fn encoded_projection_retains_original_source_and_utf8_hash_in_version_four() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let (raw, evidence, envelope) = encoded_projection_fixture(encoding);
            assert_eq!(envelope.version, LEXICAL_ENCODED_PROJECTION_VERSION);
            assert_eq!(evidence.text(), "fn λ\r\n (😀)");
            assert_eq!(evidence.source_encoding(), Some(encoding));
            assert_eq!(
                evidence.complete_text_hash(),
                content_hash(evidence.text().as_bytes())
            );
            assert_eq!(
                envelope.evidence.source.as_ref().unwrap().content_hash(),
                content_hash(&raw)
            );
            assert_ne!(evidence.complete_text_hash(), content_hash(&raw));
            assert_eq!(evidence.verify_source_bytes(&raw), Ok(()));
            assert_eq!(
                evidence.verify_source_text(evidence.text()),
                Err(LexicalExtensionError::IncompatibleFormat)
            );
            assert_eq!(
                decode_lexical_evidence_envelope(&envelope),
                Ok(evidence.clone())
            );
            assert_eq!(
                encode_lexical_evidence(&evidence),
                Ok(envelope.payload.clone())
            );
            assert!(!format!("{evidence:?}").contains("fn λ"));
            let payload: serde_json::Value = serde_json::from_str(&envelope.payload).unwrap();
            assert_eq!(payload["source_encoding"], encoding.as_str());
        }
    }

    #[test]
    fn encoded_projection_rejects_corrupted_raw_bytes_and_malformed_selected_tails() {
        for encoding in [SourceTextEncoding::Utf16Le, SourceTextEncoding::Utf16Be] {
            let (raw, evidence, _) = encoded_projection_fixture(encoding);
            let mut corrupted = raw.clone();
            corrupted[2] ^= 1;
            assert_eq!(
                evidence.verify_source_bytes(&corrupted),
                Err(LexicalExtensionError::SourceMismatch)
            );
            let mut malformed = raw.clone();
            let unit = match encoding {
                SourceTextEncoding::Utf16Le => 0xd800_u16.to_le_bytes(),
                SourceTextEncoding::Utf16Be => 0xd800_u16.to_be_bytes(),
            };
            malformed[2..4].copy_from_slice(&unit);
            assert_eq!(
                evidence.verify_source_bytes(&malformed),
                Err(LexicalExtensionError::SourceMismatch)
            );
            assert_eq!(
                evidence.verify_source_bytes(&raw[..raw.len() - 1]),
                Err(LexicalExtensionError::SourceMismatch)
            );
            assert_eq!(
                evidence.verify_source_bytes(&raw[..4]),
                Err(LexicalExtensionError::SourceMismatch)
            );
        }
    }

    #[test]
    fn encoded_projection_rejects_wrong_endian_versions_and_widths() {
        let (little_raw, evidence, envelope) =
            encoded_projection_fixture(SourceTextEncoding::Utf16Le);
        let (big_raw, _, _) = encoded_projection_fixture(SourceTextEncoding::Utf16Be);
        assert_eq!(
            evidence.verify_source_bytes(&big_raw),
            Err(LexicalExtensionError::SourceMismatch)
        );
        assert_eq!(evidence.verify_source_bytes(&little_raw), Ok(()));
        for version in [
            LEXICAL_EXTENSION_VERSION,
            LEXICAL_PROJECTION_VERSION,
            LEXICAL_SUMMARY_BATCH_VERSION,
        ] {
            let mut downgraded = envelope.clone();
            downgraded.version = version.to_owned();
            assert_eq!(
                decode_lexical_evidence_envelope(&downgraded),
                Err(LexicalExtensionError::UnsupportedVersion)
            );
        }
        let original: serde_json::Value = serde_json::from_str(&envelope.payload).unwrap();
        for (field, value) in [
            ("source_start", serde_json::json!(3)),
            ("source_end", serde_json::json!(16)),
            ("text_end", serde_json::json!(2)),
        ] {
            let mut corrupted = original.clone();
            corrupted["source_parts"][0][field] = value;
            assert!(decode_lexical_evidence(&serde_json::to_string(&corrupted).unwrap()).is_err());
        }
        let mut missing_encoding = original.clone();
        missing_encoding
            .as_object_mut()
            .unwrap()
            .remove("source_encoding");
        assert!(
            decode_lexical_evidence(&serde_json::to_string(&missing_encoding).unwrap()).is_err()
        );
        let mut unknown_encoding = original.clone();
        unknown_encoding["source_encoding"] = serde_json::json!("utf-16");
        assert_eq!(
            decode_lexical_evidence(&serde_json::to_string(&unknown_encoding).unwrap()),
            Err(LexicalExtensionError::MalformedPayload)
        );
        let mut changed_hash = original;
        changed_hash["complete_text_hash"] =
            serde_json::json!(content_hash(b"synthetic changed text"));
        assert_eq!(
            decode_lexical_evidence(&serde_json::to_string(&changed_hash).unwrap()),
            Err(LexicalExtensionError::CompleteTextHashMismatch)
        );
    }

    #[test]
    fn encoded_projection_requires_original_absolute_source_alignment_and_bounds() {
        let (_, evidence, envelope) = encoded_projection_fixture(SourceTextEncoding::Utf16Le);
        let original = envelope.evidence.source.as_ref().unwrap();
        for range in [
            1..original.span().end_byte() + 1,
            0..original.span().end_byte() - 2,
        ] {
            let source = SourceRef::new(
                original.repository(),
                original.generation(),
                SourceSpan::new(original.span().file(), range.start, range.end).unwrap(),
                original.content_hash(),
                None,
            );
            assert_eq!(
                new_lexical_evidence_envelope(
                    envelope.repository,
                    envelope.generation,
                    envelope.provenance,
                    source,
                    &evidence
                ),
                Err(LexicalExtensionError::InvalidSourceProjection)
            );
        }
    }

    #[test]
    fn encoded_projection_shares_existing_canonical_payload_budget() {
        let text = "\0".repeat(4_032);
        let raw: Vec<_> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let ranges: Vec<_> = (0..crate::MAX_SIGNATURE_SOURCE_PARTS)
            .map(|index| index * 126..(index + 1) * 126)
            .collect();
        let projection =
            EncodedSourceTextProjection::from_source(&raw, SourceTextEncoding::Utf16Le, &ranges)
                .unwrap();
        let (_, _, _, _, subject) = owner();
        let evidence = LexicalEvidenceV1::from_encoded_source_projection(subject, projection);
        assert_eq!(evidence.text().len(), 4_095);
        assert!(matches!(
            encode_lexical_evidence(&evidence),
            Err(LexicalExtensionError::PayloadTooLarge {
                limit: MAX_LEXICAL_PAYLOAD_BYTES,
                ..
            })
        ));
    }

    #[test]
    fn encoded_projection_addition_keeps_legacy_projection_payload_exact() {
        let (_, _, _, _, subject) = owner();
        let projection =
            SourceTextProjection::from_source("fn f /* comment */ (雪)", &[0..4, 19..24]).unwrap();
        let evidence = LexicalEvidenceV1::from_source_projection(subject, projection);
        let expected = format!(
            "{{\"kind\":\"signature\",\"subject\":{},\"format\":\"source_projection\",\"text\":\"fn f (雪)\",\"complete_text_hash\":{},\"truncated\":false,\"source_parts\":[{{\"source_start\":0,\"source_end\":4,\"text_start\":0,\"text_end\":4}},{{\"source_start\":19,\"source_end\":24,\"text_start\":5,\"text_end\":10}}]}}",
            serde_json::to_string(&subject).unwrap(),
            serde_json::to_string(&content_hash("fn f (雪)".as_bytes())).unwrap(),
        );
        assert_eq!(encode_lexical_evidence(&evidence), Ok(expected));
        assert_eq!(evidence.source_encoding(), None);
        assert_eq!(
            evidence.verify_source_bytes("fn f /* comment */ (雪)".as_bytes()),
            Ok(())
        );
    }

    fn decode_test_value(
        value: serde_json::Value,
    ) -> Result<LexicalEvidenceV1, LexicalExtensionError> {
        let payload = serde_json::to_string(&value).expect("test value serializes");
        decode_lexical_evidence(&payload)
    }

    fn envelope(text: &str) -> ExtensionEnvelope {
        let (repository, generation, provenance, source, _) = owner();
        new_lexical_evidence_envelope(repository, generation, provenance, source, &signature(text))
            .expect("envelope fixture is valid")
    }

    #[test]
    fn every_kind_round_trips_with_deterministic_identity() {
        let (repository, generation, provenance, source, subject) = owner();
        let cases = [
            (
                LexicalEvidenceKind::Signature,
                LexicalEvidenceFormat::SourceText,
                "fn fixture(value: usize) -> usize",
            ),
            (
                LexicalEvidenceKind::DocumentationSummary,
                LexicalEvidenceFormat::PlainText,
                "Returns the bounded fixture value.",
            ),
            (
                LexicalEvidenceKind::CommentSummary,
                LexicalEvidenceFormat::PlainText,
                "Safety invariant for the following branch.",
            ),
        ];

        for (kind, format, text) in cases {
            let evidence = LexicalEvidenceV1::from_complete_text(kind, subject, format, text)
                .expect("case constructs");
            let first = new_lexical_evidence_envelope(
                repository,
                generation,
                provenance,
                source.clone(),
                &evidence,
            )
            .expect("case envelopes");
            let second = new_lexical_evidence_envelope(
                repository,
                generation,
                provenance,
                source.clone(),
                &evidence,
            )
            .expect("case envelopes deterministically");

            assert_eq!(first, second);
            assert_eq!(
                decode_lexical_evidence_envelope(&first),
                Ok(evidence.clone())
            );
            assert_eq!(
                validate_lexical_evidence_envelope(&first, subject, &source, provenance),
                Ok(evidence)
            );
        }
    }

    #[test]
    fn truncates_at_utf8_boundaries_and_hashes_complete_text() {
        let (_, _, _, _, subject) = owner();
        let signature_text = format!("{}é", "a".repeat(MAX_LEXICAL_SIGNATURE_BYTES - 1));
        let signature = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::Signature,
            subject,
            LexicalEvidenceFormat::SourceText,
            &signature_text,
        )
        .expect("signature truncates");
        assert!(signature.is_truncated());
        assert_eq!(signature.text().len(), MAX_LEXICAL_SIGNATURE_BYTES - 1);
        assert_eq!(
            signature.complete_text_hash(),
            content_hash(signature_text.as_bytes())
        );
        let encoded = encode_lexical_evidence(&signature).expect("truncated signature encodes");
        assert_eq!(decode_lexical_evidence(&encoded), Ok(signature));

        let summary_text = format!("{}🦀", "b".repeat(MAX_LEXICAL_SUMMARY_BYTES - 1));
        let summary = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::DocumentationSummary,
            subject,
            LexicalEvidenceFormat::PlainText,
            &summary_text,
        )
        .expect("summary truncates");
        assert!(summary.is_truncated());
        assert_eq!(summary.text().len(), MAX_LEXICAL_SUMMARY_BYTES - 1);
        assert_eq!(
            summary.complete_text_hash(),
            content_hash(summary_text.as_bytes())
        );
    }

    #[test]
    fn exact_caps_remain_untruncated() {
        let (_, _, _, _, subject) = owner();
        for (kind, format, limit) in [
            (
                LexicalEvidenceKind::Signature,
                LexicalEvidenceFormat::SourceText,
                MAX_LEXICAL_SIGNATURE_BYTES,
            ),
            (
                LexicalEvidenceKind::DocumentationSummary,
                LexicalEvidenceFormat::PlainText,
                MAX_LEXICAL_SUMMARY_BYTES,
            ),
            (
                LexicalEvidenceKind::CommentSummary,
                LexicalEvidenceFormat::PlainText,
                MAX_LEXICAL_SUMMARY_BYTES,
            ),
        ] {
            let text = "x".repeat(limit);
            let evidence = LexicalEvidenceV1::from_complete_text(kind, subject, format, &text)
                .expect("exact cap is accepted");
            assert!(!evidence.is_truncated());
            assert_eq!(evidence.text(), text);
        }
    }

    #[test]
    fn incompatible_formats_empty_text_and_overlong_wire_values_reject() {
        let (_, _, _, _, subject) = owner();
        assert_eq!(
            LexicalEvidenceV1::from_complete_text(
                LexicalEvidenceKind::Signature,
                subject,
                LexicalEvidenceFormat::PlainText,
                "fn fixture()"
            ),
            Err(LexicalExtensionError::IncompatibleFormat)
        );
        assert_eq!(
            LexicalEvidenceV1::from_complete_text(
                LexicalEvidenceKind::CommentSummary,
                subject,
                LexicalEvidenceFormat::PlainText,
                ""
            ),
            Err(LexicalExtensionError::EmptyText)
        );

        let mut signature_value =
            serde_json::to_value(signature("valid")).expect("fixture serializes");
        signature_value["text"] = serde_json::json!("x".repeat(MAX_LEXICAL_SIGNATURE_BYTES + 1));
        signature_value["truncated"] = serde_json::json!(true);
        assert!(decode_test_value(signature_value).is_err());

        let summary = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::DocumentationSummary,
            subject,
            LexicalEvidenceFormat::PlainText,
            "valid",
        )
        .expect("summary fixture");
        let mut summary_value = serde_json::to_value(summary).expect("fixture serializes");
        summary_value["text"] = serde_json::json!("x".repeat(MAX_LEXICAL_SUMMARY_BYTES + 1));
        summary_value["truncated"] = serde_json::json!(true);
        assert!(decode_test_value(summary_value).is_err());

        let mut unknown_subject_field =
            serde_json::to_value(signature("valid")).expect("fixture serializes");
        unknown_subject_field["subject"]["unknown"] = serde_json::json!(true);
        assert!(decode_test_value(unknown_subject_field).is_err());

        let mut impossible_truncation =
            serde_json::to_value(signature("valid")).expect("fixture serializes");
        impossible_truncation["truncated"] = serde_json::json!(true);
        assert!(decode_test_value(impossible_truncation).is_err());
    }

    #[test]
    fn bounded_decoder_rejects_oversized_input_before_json_decoding() {
        let observed = MAX_LEXICAL_PAYLOAD_BYTES + 1;
        let oversized_malformed = "{".repeat(observed);
        assert_eq!(
            decode_lexical_evidence(&oversized_malformed),
            Err(LexicalExtensionError::PayloadTooLarge {
                observed,
                limit: MAX_LEXICAL_PAYLOAD_BYTES,
            })
        );
    }

    #[test]
    fn payload_decoder_rejects_malformed_noncanonical_and_wrong_hash() {
        assert_eq!(
            decode_lexical_evidence("{"),
            Err(LexicalExtensionError::MalformedPayload)
        );

        let canonical =
            encode_lexical_evidence(&signature("fixture")).expect("fixture encodes canonically");
        assert_eq!(
            decode_lexical_evidence(&format!(" {canonical}")),
            Err(LexicalExtensionError::NoncanonicalPayload)
        );

        let mut wrong_hash: serde_json::Value =
            serde_json::from_str(&canonical).expect("canonical fixture parses");
        wrong_hash["complete_text_hash"] = serde_json::json!(content_hash(b"other"));
        let wrong_hash = serde_json::to_string(&wrong_hash).expect("mutated fixture serializes");
        assert_eq!(
            decode_lexical_evidence(&wrong_hash),
            Err(LexicalExtensionError::CompleteTextHashMismatch)
        );
    }

    #[test]
    fn worst_case_json_escaping_stays_inside_payload_bound() {
        let text = "\0".repeat(MAX_LEXICAL_SIGNATURE_BYTES);
        let encoded =
            encode_lexical_evidence(&signature(&text)).expect("worst-case escaped signature fits");
        assert!(encoded.len() <= MAX_LEXICAL_PAYLOAD_BYTES);
    }

    #[test]
    fn envelope_decoder_rejects_version_criticality_identity_and_evidence_mismatches() {
        let mut wrong_namespace = envelope("fixture");
        wrong_namespace.namespace = "third.party".to_owned();
        assert_eq!(
            decode_lexical_evidence_envelope(&wrong_namespace),
            Err(LexicalExtensionError::UnsupportedNamespace)
        );

        let mut wrong_version = envelope("fixture");
        wrong_version.version = "2".to_owned();
        assert_eq!(
            decode_lexical_evidence_envelope(&wrong_version),
            Err(LexicalExtensionError::UnsupportedVersion)
        );

        let mut critical = envelope("fixture");
        critical.criticality = ExtensionCriticality::Critical;
        assert_eq!(
            decode_lexical_evidence_envelope(&critical),
            Err(LexicalExtensionError::WrongCriticality)
        );

        let mut missing_source = envelope("fixture");
        missing_source.evidence.source = None;
        assert_eq!(
            decode_lexical_evidence_envelope(&missing_source),
            Err(LexicalExtensionError::MissingDirectSourceEvidence)
        );

        let mut missing_subject = envelope("fixture");
        missing_subject.evidence.derivation.clear();
        assert_eq!(
            decode_lexical_evidence_envelope(&missing_subject),
            Err(LexicalExtensionError::SubjectEvidenceMismatch)
        );

        let mut wrong_subject = envelope("fixture");
        wrong_subject.evidence.derivation[0] = FactRef::File(FileId::from_bytes([9; 20]));
        assert_eq!(
            decode_lexical_evidence_envelope(&wrong_subject),
            Err(LexicalExtensionError::SubjectEvidenceMismatch)
        );

        let mut wrong_identity = envelope("fixture");
        wrong_identity.id = FactId::from_bytes([10; 20]);
        assert_eq!(
            decode_lexical_evidence_envelope(&wrong_identity),
            Err(LexicalExtensionError::EnvelopeIdentityMismatch)
        );
    }

    #[test]
    fn source_owner_file_subject_and_expected_metadata_are_checked() {
        let (repository, generation, provenance, source, subject) = owner();
        let evidence = signature("fixture");

        let foreign_source = SourceRef::new(
            RepositoryId::from_bytes([11; 16]),
            generation,
            source.span(),
            source.content_hash(),
            None,
        );
        assert_eq!(
            new_lexical_evidence_envelope(
                repository,
                generation,
                provenance,
                foreign_source,
                &evidence
            ),
            Err(LexicalExtensionError::RepositoryMismatch)
        );

        let stale_source = SourceRef::new(
            repository,
            GenerationId::from_bytes([15; 20]),
            source.span(),
            source.content_hash(),
            None,
        );
        assert_eq!(
            new_lexical_evidence_envelope(
                repository,
                generation,
                provenance,
                stale_source,
                &evidence
            ),
            Err(LexicalExtensionError::GenerationMismatch)
        );

        let file_subject = FactRef::File(FileId::from_bytes([12; 20]));
        let file_evidence = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::Signature,
            file_subject,
            LexicalEvidenceFormat::SourceText,
            "fixture",
        )
        .expect("file evidence constructs");
        assert_eq!(
            new_lexical_evidence_envelope(
                repository,
                generation,
                provenance,
                source.clone(),
                &file_evidence
            ),
            Err(LexicalExtensionError::FileSubjectSourceMismatch)
        );

        let envelope = new_lexical_evidence_envelope(
            repository,
            generation,
            provenance,
            source.clone(),
            &evidence,
        )
        .expect("fixture envelope");
        assert_eq!(
            validate_lexical_evidence_envelope(
                &envelope,
                FactRef::Entity(SymbolId::from_bytes([13; 20])),
                &source,
                provenance
            ),
            Err(LexicalExtensionError::SubjectMismatch)
        );
        let different_source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(source.span().file(), 8, 23).expect("fixture span is ordered"),
            source.content_hash(),
            None,
        );
        assert_eq!(
            validate_lexical_evidence_envelope(&envelope, subject, &different_source, provenance),
            Err(LexicalExtensionError::SourceMismatch)
        );
        assert_eq!(
            validate_lexical_evidence_envelope(
                &envelope,
                subject,
                &source,
                FactId::from_bytes([14; 20])
            ),
            Err(LexicalExtensionError::ProvenanceMismatch)
        );
    }

    #[test]
    fn skipping_lexical_extension_preserves_valid_common_ir() {
        let mut document = normalized_fixture();
        let subject = FactRef::Entity(document.entities[0].id);
        let source = document.entities[0]
            .evidence
            .source
            .clone()
            .expect("fixture entity has direct source");
        let provenance = document.entities[0].provenance;
        let evidence = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::Signature,
            subject,
            LexicalEvidenceFormat::SourceText,
            "fixture",
        )
        .expect("fixture lexical evidence");
        document.extensions = vec![
            new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                provenance,
                source,
                &evidence,
            )
            .expect("fixture lexical envelope"),
        ];

        let support = ExtensionSupport {
            unknown_noncritical: UnknownNoncriticalExtensionPolicy::Skip,
            ..ExtensionSupport::default()
        };
        let canonical = canonicalize_ir_document(document, &IrLimits::default(), &support)
            .expect("common IR remains valid when lexical evidence is skipped");
        assert!(canonical.extensions.is_empty());
        assert_eq!(canonical.entities.len(), 1);
    }

    #[test]
    fn frozen_extension_fixture_matches_constructor() {
        let document = normalized_fixture();
        let subject = FactRef::Entity(document.entities[0].id);
        let source = document.entities[0]
            .evidence
            .source
            .clone()
            .expect("fixture entity has direct source");
        let evidence = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::Signature,
            subject,
            LexicalEvidenceFormat::SourceText,
            "fixture",
        )
        .expect("fixture lexical evidence");
        let envelope = new_lexical_evidence_envelope(
            document.repository,
            document.generation,
            document.entities[0].provenance,
            source,
            &evidence,
        )
        .expect("fixture lexical envelope");
        let frozen = crate::decode_extension_envelope(
            include_bytes!(
                "../../../tests/fixtures/compatibility/extensions/rootlight.lexical/1/envelope.json"
            ),
            &crate::IrLimits::default(),
        )
        .expect("frozen lexical extension fixture decodes");

        assert_eq!(frozen, envelope);
        assert_eq!(decode_lexical_evidence_envelope(&frozen), Ok(evidence));
    }
}
