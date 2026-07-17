//! Bounded first-party lexical evidence carried by normalized IR extensions.
//!
//! The common IR never depends on this noncritical payload. Typed consumers can
//! validate exact ownership, source, provenance, and subject derivation.

use rootlight_ids::{
    ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId, content_hash, derive_fact,
};
use serde::{Deserialize, Serialize};

use crate::{ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef};

/// Namespace of the first-party lexical evidence extension.
pub const LEXICAL_EXTENSION_NAMESPACE: &str = "rootlight.lexical";
/// Exact payload version of the first-party lexical evidence extension.
pub const LEXICAL_EXTENSION_VERSION: &str = "1";
/// Maximum retained UTF-8 bytes in one signature.
pub const MAX_LEXICAL_SIGNATURE_BYTES: usize = 4 * 1024;
/// Maximum retained UTF-8 bytes in one documentation or comment summary.
pub const MAX_LEXICAL_SUMMARY_BYTES: usize = 512;
/// Maximum canonical JSON bytes in one lexical extension payload.
///
/// The bound covers worst-case JSON escaping of a 4 KiB signature plus all
/// fixed metadata while remaining far below the generic IR extension ceiling.
pub const MAX_LEXICAL_PAYLOAD_BYTES: usize = 25 * 1024;

const LEXICAL_FACT_DOMAIN: &str = "rootlight.lexical/v1";
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

/// Closed text formats understood by lexical evidence version 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum LexicalEvidenceFormat {
    /// Exact source-language text without prose generation.
    SourceText,
    /// Adapter-supplied plain text without markup semantics.
    PlainText,
}

/// One bounded, hash-linked lexical evidence payload.
///
/// Fields are private so every constructed or decoded value satisfies the
/// version 1 byte, format, truncation, and hash invariants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LexicalEvidenceV1 {
    kind: LexicalEvidenceKind,
    subject: FactRef,
    format: LexicalEvidenceFormat,
    text: String,
    complete_text_hash: ContentHash,
    truncated: bool,
}

impl LexicalEvidenceV1 {
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

    /// Returns the bounded retained text prefix.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Returns the hash of the complete text before truncation.
    #[must_use]
    pub const fn complete_text_hash(&self) -> ContentHash {
        self.complete_text_hash
    }

    /// Reports whether the complete text exceeded the retained byte limit.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }

    fn from_wire(wire: WireLexicalEvidenceV1) -> Result<Self, LexicalExtensionError> {
        let evidence = Self {
            kind: wire.kind,
            subject: wire.subject.into(),
            format: wire.format,
            text: wire.text,
            complete_text_hash: wire.complete_text_hash,
            truncated: wire.truncated,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    fn validate(&self) -> Result<(), LexicalExtensionError> {
        if self.text.is_empty() {
            return Err(LexicalExtensionError::EmptyText);
        }
        if self.format != self.kind.required_format() {
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
        Ok(())
    }
}

impl<'de> Deserialize<'de> for LexicalEvidenceV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = WireLexicalEvidenceV1::deserialize(deserializer)?;
        Self::from_wire(wire).map_err(serde::de::Error::custom)
    }
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
            "description": "Bounded source-derived lexical evidence. Runtime decoding additionally enforces UTF-8 byte limits, canonical JSON, and complete-text hashes.",
            "oneOf": [signature, documentation, comment]
        })
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
    version: &'static str,
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

/// Decodes an exact version 1 lexical payload from canonical JSON.
///
/// # Errors
///
/// Rejects oversized, malformed, noncanonical, hash-inconsistent, or otherwise
/// invalid payloads with [`LexicalExtensionError`].
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
    let payload = encode_lexical_evidence(evidence)?;
    let id = derive_envelope_id(
        repository,
        generation,
        evidence.subject,
        &source,
        provenance,
        &payload,
    )?;
    Ok(ExtensionEnvelope {
        id,
        repository,
        generation,
        namespace: LEXICAL_EXTENSION_NAMESPACE.to_owned(),
        version: LEXICAL_EXTENSION_VERSION.to_owned(),
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
    let source = envelope
        .evidence
        .source
        .as_ref()
        .ok_or(LexicalExtensionError::MissingDirectSourceEvidence)?;
    validate_source_owner(envelope.repository, envelope.generation, source)?;
    validate_file_subject_source(evidence.subject, source)?;
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
    if envelope.version != LEXICAL_EXTENSION_VERSION {
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
) -> Result<FactId, LexicalExtensionError> {
    let identity = LexicalEnvelopeIdentity {
        namespace: LEXICAL_EXTENSION_NAMESPACE,
        version: LEXICAL_EXTENSION_VERSION,
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
    /// Payload was not valid strict version 1 JSON.
    #[error("lexical evidence payload is malformed")]
    MalformedPayload,
    /// Payload JSON was semantically valid but not canonical.
    #[error("lexical evidence payload is not canonical JSON")]
    NoncanonicalPayload,
    /// Envelope namespace was not the first-party lexical namespace.
    #[error("lexical extension namespace is unsupported")]
    UnsupportedNamespace,
    /// Envelope payload version was not exactly version 1.
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
        ExtensionSupport, IrLimits, NormalizedIrDocument, SourceSpan,
        UnknownNoncriticalExtensionPolicy, canonicalize_ir_document,
    };

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
        assert!(serde_json::from_value::<LexicalEvidenceV1>(signature_value).is_err());

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
        assert!(serde_json::from_value::<LexicalEvidenceV1>(summary_value).is_err());

        let mut unknown_subject_field =
            serde_json::to_value(signature("valid")).expect("fixture serializes");
        unknown_subject_field["subject"]["unknown"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LexicalEvidenceV1>(unknown_subject_field).is_err());

        let mut impossible_truncation =
            serde_json::to_value(signature("valid")).expect("fixture serializes");
        impossible_truncation["truncated"] = serde_json::json!(true);
        assert!(serde_json::from_value::<LexicalEvidenceV1>(impossible_truncation).is_err());
    }

    #[test]
    fn payload_decoder_rejects_malformed_noncanonical_oversized_and_wrong_hash() {
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
        let oversized = "x".repeat(MAX_LEXICAL_PAYLOAD_BYTES + 1);
        assert!(matches!(
            decode_lexical_evidence(&oversized),
            Err(LexicalExtensionError::PayloadTooLarge { .. })
        ));

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
        let mut document: NormalizedIrDocument = serde_json::from_str(include_str!(
            "../../../tests/fixtures/compatibility/ir/1.1/document.json"
        ))
        .expect("frozen normalized IR fixture decodes");
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
        let document: NormalizedIrDocument = serde_json::from_str(include_str!(
            "../../../tests/fixtures/compatibility/ir/1.1/document.json"
        ))
        .expect("frozen normalized IR fixture decodes");
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
        let frozen: ExtensionEnvelope = serde_json::from_str(include_str!(
            "../../../tests/fixtures/compatibility/extensions/rootlight.lexical/1/envelope.json"
        ))
        .expect("frozen lexical extension fixture decodes");

        assert_eq!(frozen, envelope);
        assert_eq!(decode_lexical_evidence_envelope(&frozen), Ok(evidence));
    }
}
