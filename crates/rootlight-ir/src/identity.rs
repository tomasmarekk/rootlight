//! Versioned semantic-identity recipes and producer-neutral claim envelopes.

use std::io::{self, BufReader, Read, Write};

use rootlight_ids::{
    ContentHash, FactId, FileId, FileIdentity, GenerationId, RepositoryId, SymbolId,
    SymbolIdentity, derive_fact, derive_file, derive_symbol,
};
use serde::{Deserialize, Serialize};

use crate::{
    ContainerRef, CoverageRecord, DiagnosticRecord, EntityKind, ExtensionCriticality,
    ExtensionEnvelope, FactEvidence, FactRef, OccurrenceRecord, ProvenanceRecord, RelationRecord,
    SkippedRegion, SourceMappingRecord, SourceRef,
};

/// Namespace carrying one unverified structured file-identity claim.
pub const FILE_IDENTITY_CLAIM_NAMESPACE: &str = "dev.rootlight.identity.file";
/// Namespace carrying one unverified structured symbol-identity claim.
pub const SYMBOL_IDENTITY_CLAIM_NAMESPACE: &str = "dev.rootlight.identity.symbol";
/// Version of the producer-neutral identity-claim payload.
pub const IDENTITY_CLAIM_VERSION: &str = "1.0";

const PROVENANCE_FACT_DOMAIN: &str = "rootlight.provenance/v2";
const OCCURRENCE_FACT_DOMAIN: &str = "rootlight.occurrence/v2";
const RELATION_FACT_DOMAIN: &str = "rootlight.relation/v2";
const SOURCE_MAPPING_FACT_DOMAIN: &str = "rootlight.source-mapping/v2";
const COVERAGE_FACT_DOMAIN: &str = "rootlight.coverage/v2";
const SKIPPED_REGION_FACT_DOMAIN: &str = "rootlight.skipped-region/v2";
const DIAGNOSTIC_FACT_DOMAIN: &str = "rootlight.diagnostic/v2";

/// Unverified structured inputs from which a consumer can recompute a [`FileId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentityClaim {
    /// Claimed file identity.
    pub file: FileId,
    /// Repository owning the file.
    pub repository: RepositoryId,
    /// Canonical repository-relative presentation path.
    pub path: String,
    /// Lossless platform path identity bytes used by the VFS.
    pub path_identity: Vec<u8>,
    /// Immutable content hash bound to the manifest entry.
    pub content_hash: ContentHash,
    /// Immutable file size bound to the manifest entry.
    pub byte_length: u64,
}

impl FileIdentityClaim {
    /// Recomputes the file identity from the claim inputs.
    #[must_use]
    pub fn derived_file(&self) -> FileId {
        derive_file(FileIdentity {
            repository: self.repository,
            path_identity: &self.path_identity,
        })
        .id()
    }
}

/// Unverified structured inputs from which a consumer can recompute a [`SymbolId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SymbolIdentityClaim {
    /// Claimed symbol identity.
    pub symbol: SymbolId,
    /// Repository owning the symbol.
    pub repository: RepositoryId,
    /// Canonical language identity.
    pub language: String,
    /// Closed semantic kind retained by normalized IR.
    pub kind: EntityKind,
    /// Structured semantic container retained by normalized IR.
    pub container: Option<ContainerRef>,
    /// Canonical container discriminator used by the symbol recipe.
    pub container_identity: Vec<u8>,
    /// Canonical declared identity used by the symbol recipe.
    pub declared_identity: String,
    /// Canonical overload or signature discriminator.
    pub signature_discriminator: Vec<u8>,
    /// Canonical build-context discriminator.
    pub build_context_discriminator: Vec<u8>,
}

impl SymbolIdentityClaim {
    /// Recomputes the symbol identity from the claim inputs.
    #[must_use]
    pub fn derived_symbol(&self) -> SymbolId {
        derive_symbol(SymbolIdentity {
            repository: self.repository,
            language: &self.language,
            semantic_kind: entity_kind_identity_label(self.kind),
            container_identity: &self.container_identity,
            declared_identity: &self.declared_identity,
            signature_discriminator: &self.signature_discriminator,
            build_context_discriminator: &self.build_context_discriminator,
        })
        .id()
    }
}

/// Identity-claim envelope construction or decoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityClaimError {
    /// The payload could not be encoded or decoded.
    #[error("identity claim payload is malformed")]
    MalformedPayload,
    /// The payload is not the unique canonical JSON encoding.
    #[error("identity claim payload is not canonical")]
    NoncanonicalPayload,
    /// The namespace, version, or criticality is not the claim contract.
    #[error("identity claim envelope contract is unsupported")]
    UnsupportedEnvelope,
    /// The envelope owner or evidence does not match the claim.
    #[error("identity claim envelope ownership or evidence does not match")]
    EnvelopeMismatch,
    /// Cooperative payload decoding or canonical re-encoding was interrupted.
    #[error("identity claim processing was interrupted")]
    Interrupted,
}

/// Typed fact recipe encoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("typed fact identity could not be encoded")]
pub struct FactIdentityRecipeError;

/// Builds a noncritical envelope carrying one unverified file claim.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] when the claim and direct source disagree or
/// when canonical payload encoding fails.
pub fn new_file_identity_claim_envelope(
    claim: &FileIdentityClaim,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    if claim.repository != source.repository()
        || claim.file != source.span().file()
        || claim.content_hash != source.content_hash()
        || claim.byte_length != source.span().end_byte()
        || source.span().start_byte() != 0
        || source.generation() != generation
        || claim.derived_file() != claim.file
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    let payload = serde_json::to_string(claim).map_err(|_| IdentityClaimError::MalformedPayload)?;
    identity_claim_envelope(
        claim.repository,
        generation,
        FILE_IDENTITY_CLAIM_NAMESPACE,
        payload,
        provenance,
        source,
        FactRef::File(claim.file),
    )
}

/// Builds a noncritical envelope carrying one unverified symbol claim.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] when the claim and direct source disagree or
/// when canonical payload encoding fails.
pub fn new_symbol_identity_claim_envelope(
    claim: &SymbolIdentityClaim,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    if claim.repository != source.repository()
        || source.generation() != generation
        || claim.derived_symbol() != claim.symbol
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    let payload = serde_json::to_string(claim).map_err(|_| IdentityClaimError::MalformedPayload)?;
    identity_claim_envelope(
        claim.repository,
        generation,
        SYMBOL_IDENTITY_CLAIM_NAMESPACE,
        payload,
        provenance,
        source,
        FactRef::Entity(claim.symbol),
    )
}

/// Decodes and validates one file identity-claim envelope.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for a malformed, noncanonical, mismatched, or
/// unsupported claim envelope.
pub fn decode_file_identity_claim_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<FileIdentityClaim, IdentityClaimError> {
    decode_file_identity_claim_envelope_with_checkpoint(envelope, || true)
}

/// Decodes a file claim with bounded cooperative payload checkpoints.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for an interrupted, malformed,
/// noncanonical, mismatched, or unsupported claim envelope.
pub fn decode_file_identity_claim_envelope_with_checkpoint(
    envelope: &ExtensionEnvelope,
    checkpoint: impl FnMut() -> bool,
) -> Result<FileIdentityClaim, IdentityClaimError> {
    require_claim_envelope(envelope, FILE_IDENTITY_CLAIM_NAMESPACE)?;
    let claim: FileIdentityClaim = decode_canonical_payload(&envelope.payload, checkpoint)?;
    let source = require_claim_evidence(envelope, FactRef::File(claim.file))?;
    if claim.repository != envelope.repository
        || claim.file != source.span().file()
        || claim.content_hash != source.content_hash()
        || claim.byte_length != source.span().end_byte()
        || source.span().start_byte() != 0
        || claim.derived_file() != claim.file
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    require_envelope_id(envelope)?;
    Ok(claim)
}

/// Decodes and validates one symbol identity-claim envelope.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for a malformed, noncanonical, mismatched, or
/// unsupported claim envelope.
pub fn decode_symbol_identity_claim_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<SymbolIdentityClaim, IdentityClaimError> {
    decode_symbol_identity_claim_envelope_with_checkpoint(envelope, || true)
}

/// Decodes a symbol claim with bounded cooperative payload checkpoints.
///
/// # Errors
///
/// Returns [`IdentityClaimError`] for an interrupted, malformed,
/// noncanonical, mismatched, or unsupported claim envelope.
pub fn decode_symbol_identity_claim_envelope_with_checkpoint(
    envelope: &ExtensionEnvelope,
    checkpoint: impl FnMut() -> bool,
) -> Result<SymbolIdentityClaim, IdentityClaimError> {
    require_claim_envelope(envelope, SYMBOL_IDENTITY_CLAIM_NAMESPACE)?;
    let claim: SymbolIdentityClaim = decode_canonical_payload(&envelope.payload, checkpoint)?;
    let _source = require_claim_evidence(envelope, FactRef::Entity(claim.symbol))?;
    if claim.repository != envelope.repository || claim.derived_symbol() != claim.symbol {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    require_envelope_id(envelope)?;
    Ok(claim)
}

/// Derives a provenance ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_provenance_record_id(
    record: &ProvenanceRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(PROVENANCE_FACT_DOMAIN, record)
}

/// Derives an occurrence ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_occurrence_record_id(
    record: &OccurrenceRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(OCCURRENCE_FACT_DOMAIN, record)
}

/// Derives a relation ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_relation_record_id(
    record: &RelationRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(RELATION_FACT_DOMAIN, record)
}

/// Derives a source-mapping ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_source_mapping_record_id(
    record: &SourceMappingRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(SOURCE_MAPPING_FACT_DOMAIN, record)
}

/// Derives a coverage ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_coverage_record_id(
    record: &CoverageRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(COVERAGE_FACT_DOMAIN, record)
}

/// Derives a skipped-region ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_skipped_region_id(record: &SkippedRegion) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(SKIPPED_REGION_FACT_DOMAIN, record)
}

/// Derives a diagnostic ID from every typed semantic field except `id`.
///
/// # Errors
///
/// Returns [`FactIdentityRecipeError`] if canonical JSON encoding fails.
pub fn derive_diagnostic_record_id(
    record: &DiagnosticRecord,
) -> Result<FactId, FactIdentityRecipeError> {
    derive_typed_fact_id(DIAGNOSTIC_FACT_DOMAIN, record)
}

/// Stable identity label for one closed common entity kind.
#[must_use]
pub const fn entity_kind_identity_label(kind: EntityKind) -> &'static str {
    match kind {
        EntityKind::Repository => "repository",
        EntityKind::Worktree => "worktree",
        EntityKind::Package => "package",
        EntityKind::BuildTarget => "build-target",
        EntityKind::Directory => "directory",
        EntityKind::File => "file",
        EntityKind::Module => "module",
        EntityKind::Namespace => "namespace",
        EntityKind::Class => "class",
        EntityKind::Struct => "struct",
        EntityKind::Enum => "enum",
        EntityKind::Union => "union",
        EntityKind::TypeAlias => "type-alias",
        EntityKind::Trait => "trait",
        EntityKind::Interface => "interface",
        EntityKind::Protocol => "protocol",
        EntityKind::Function => "function",
        EntityKind::Method => "method",
        EntityKind::Constructor => "constructor",
        EntityKind::Closure => "closure",
        EntityKind::Field => "field",
        EntityKind::Property => "property",
        EntityKind::Constant => "constant",
        EntityKind::Variable => "variable",
        EntityKind::Parameter => "parameter",
        EntityKind::TypeParameter => "type-parameter",
        EntityKind::Import => "import",
        EntityKind::Export => "export",
        EntityKind::Route => "route",
        EntityKind::Service => "service",
        EntityKind::MessageTopic => "message-topic",
        EntityKind::DatabaseObject => "database-object",
        EntityKind::Test => "test",
        EntityKind::ConfigurationKey => "configuration-key",
        EntityKind::Commit => "commit",
        EntityKind::Change => "change",
        EntityKind::CommunityView => "community-view",
        EntityKind::ExternalSymbol => "external-symbol",
    }
}

fn identity_claim_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    namespace: &str,
    payload: String,
    provenance: FactId,
    source: SourceRef,
    subject: FactRef,
) -> Result<ExtensionEnvelope, IdentityClaimError> {
    let mut envelope = ExtensionEnvelope {
        id: FactId::from_bytes([0; 20]),
        repository,
        generation,
        namespace: namespace.to_owned(),
        version: IDENTITY_CLAIM_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    };
    envelope.id = derive_claim_envelope_id(&envelope)?;
    Ok(envelope)
}

fn require_claim_envelope(
    envelope: &ExtensionEnvelope,
    namespace: &str,
) -> Result<(), IdentityClaimError> {
    if envelope.namespace != namespace
        || envelope.version != IDENTITY_CLAIM_VERSION
        || envelope.criticality != ExtensionCriticality::Noncritical
    {
        return Err(IdentityClaimError::UnsupportedEnvelope);
    }
    Ok(())
}

fn require_claim_evidence(
    envelope: &ExtensionEnvelope,
    subject: FactRef,
) -> Result<&SourceRef, IdentityClaimError> {
    let source = envelope
        .evidence
        .source
        .as_ref()
        .ok_or(IdentityClaimError::EnvelopeMismatch)?;
    if source.repository() != envelope.repository
        || source.generation() != envelope.generation
        || envelope.evidence.derivation.as_slice() != [subject]
    {
        return Err(IdentityClaimError::EnvelopeMismatch);
    }
    Ok(source)
}

fn require_envelope_id(envelope: &ExtensionEnvelope) -> Result<(), IdentityClaimError> {
    if derive_claim_envelope_id(envelope)? == envelope.id {
        Ok(())
    } else {
        Err(IdentityClaimError::EnvelopeMismatch)
    }
}

fn derive_claim_envelope_id(envelope: &ExtensionEnvelope) -> Result<FactId, IdentityClaimError> {
    derive_typed_fact_id("rootlight.identity-claim-envelope/v1", envelope)
        .map_err(|_| IdentityClaimError::MalformedPayload)
}

fn decode_canonical_payload<T>(
    payload: &str,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<T, IdentityClaimError>
where
    T: Serialize + for<'de> Deserialize<'de>,
{
    let source = ClaimCheckpointReader::new(payload.as_bytes(), &mut checkpoint)?;
    let mut reader = BufReader::with_capacity(4 * 1024, source);
    let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
    let value = T::deserialize(&mut deserializer);
    let complete = value.is_ok() && deserializer.end().is_ok();
    drop(deserializer);
    let interrupted = reader.get_ref().interrupted;
    drop(reader);
    if interrupted {
        return Err(IdentityClaimError::Interrupted);
    }
    let value = value.map_err(|_| IdentityClaimError::MalformedPayload)?;
    if !complete {
        return Err(IdentityClaimError::MalformedPayload);
    }

    let mut canonical = Vec::with_capacity(payload.len());
    {
        let mut writer = ClaimCheckpointWriter::new(&mut canonical, &mut checkpoint)?;
        if serde_json::to_writer(&mut writer, &value).is_err() {
            return if writer.interrupted {
                Err(IdentityClaimError::Interrupted)
            } else {
                Err(IdentityClaimError::MalformedPayload)
            };
        }
        writer.check()?;
    }
    if canonical != payload.as_bytes() {
        return Err(IdentityClaimError::NoncanonicalPayload);
    }
    Ok(value)
}

struct ClaimCheckpointReader<'input, 'checkpoint, F> {
    input: &'input [u8],
    position: usize,
    checkpoint: &'checkpoint mut F,
    interrupted: bool,
}

impl<'input, 'checkpoint, F> ClaimCheckpointReader<'input, 'checkpoint, F>
where
    F: FnMut() -> bool,
{
    fn new(
        input: &'input [u8],
        checkpoint: &'checkpoint mut F,
    ) -> Result<Self, IdentityClaimError> {
        if !checkpoint() {
            return Err(IdentityClaimError::Interrupted);
        }
        Ok(Self {
            input,
            position: 0,
            checkpoint,
            interrupted: false,
        })
    }
}

impl<F> Read for ClaimCheckpointReader<'_, '_, F>
where
    F: FnMut() -> bool,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.interrupted {
            return Err(checkpoint_io_error());
        }
        if self.position >= self.input.len() {
            return Ok(0);
        }
        if !(self.checkpoint)() {
            self.interrupted = true;
            return Err(checkpoint_io_error());
        }
        let length = buffer
            .len()
            .min(4 * 1024)
            .min(self.input.len() - self.position);
        buffer[..length].copy_from_slice(&self.input[self.position..self.position + length]);
        self.position += length;
        Ok(length)
    }
}

struct ClaimCheckpointWriter<'output, 'checkpoint, F> {
    output: &'output mut Vec<u8>,
    checkpoint: &'checkpoint mut F,
    interrupted: bool,
}

impl<'output, 'checkpoint, F> ClaimCheckpointWriter<'output, 'checkpoint, F>
where
    F: FnMut() -> bool,
{
    fn new(
        output: &'output mut Vec<u8>,
        checkpoint: &'checkpoint mut F,
    ) -> Result<Self, IdentityClaimError> {
        let mut writer = Self {
            output,
            checkpoint,
            interrupted: false,
        };
        writer.check()?;
        Ok(writer)
    }

    fn check(&mut self) -> Result<(), IdentityClaimError> {
        if self.interrupted {
            return Err(IdentityClaimError::Interrupted);
        }
        if (self.checkpoint)() {
            Ok(())
        } else {
            self.interrupted = true;
            Err(IdentityClaimError::Interrupted)
        }
    }
}

impl<F> Write for ClaimCheckpointWriter<'_, '_, F>
where
    F: FnMut() -> bool,
{
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        for chunk in buffer.chunks(4 * 1024) {
            self.check().map_err(|_| checkpoint_io_error())?;
            self.output.extend_from_slice(chunk);
            written += chunk.len();
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checkpoint_io_error() -> io::Error {
    // Standard I/O adapters automatically retry `Interrupted`; cancellation
    // must escape serde so the public decoder can restore its typed error.
    io::Error::other("identity claim checkpoint")
}

fn derive_typed_fact_id<T>(domain: &str, record: &T) -> Result<FactId, FactIdentityRecipeError>
where
    T: Serialize,
{
    let mut value = serde_json::to_value(record).map_err(|_| FactIdentityRecipeError)?;
    let object = value.as_object_mut().ok_or(FactIdentityRecipeError)?;
    if object.remove("id").is_none() {
        return Err(FactIdentityRecipeError);
    }
    let bytes = serde_json::to_vec(&value).map_err(|_| FactIdentityRecipeError)?;
    Ok(derive_fact(domain, &bytes).id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SourceSpan;

    #[test]
    fn claim_decoder_stops_inside_a_large_payload() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let path_identity = vec![3; 16 * 1024];
        let file = derive_file(FileIdentity {
            repository,
            path_identity: &path_identity,
        })
        .id();
        let content_hash = rootlight_ids::content_hash(b"fixture");
        let claim = FileIdentityClaim {
            file,
            repository,
            path: "fixture.rs".to_owned(),
            path_identity,
            content_hash,
            byte_length: 7,
        };
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 7).expect("fixture source span is valid"),
            content_hash,
            None,
        );
        let envelope = new_file_identity_claim_envelope(
            &claim,
            generation,
            FactId::from_bytes([4; 20]),
            source,
        )
        .expect("large claim envelope is valid");
        let mut checkpoints = 0;
        let result = decode_file_identity_claim_envelope_with_checkpoint(&envelope, || {
            checkpoints += 1;
            checkpoints < 3
        });

        assert_eq!(result, Err(IdentityClaimError::Interrupted));
        assert_eq!(checkpoints, 3);
    }
}
