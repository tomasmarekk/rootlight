//! Identity-verified, file-affine RDF payloads outside an owned generation.
//!
//! The parent remains the authority for files and provenance. A partition is
//! independently bounded and is not a published generation or coverage claim.

use std::{
    collections::BTreeMap,
    io::{self, Write},
};

use rootlight_ids::{ContentHash, FileId, derive_fact};
use rootlight_ir::{
    ExtensionCriticality, ExtensionEnvelope, ExtensionIdentifier, ExtensionSupport, FactEvidence,
    FactRef, FileRecord, IrDocumentUsage, IrDocumentUsageError, IrLimits, RDF_STATEMENT_NAMESPACE,
    decode_rdf_statement_envelope,
};
use serde::{Deserialize, Serialize};

use crate::{
    GenerationContext, GenerationControlError, GenerationMetadata, GenerationResource,
    GenerationSnapshot, IdentityVerifiedGeneration,
};

/// Exact durable RDF partition format version.
pub const RDF_PARTITION_FORMAT_VERSION: u16 = 1;

/// Critical identity commitment for every member of a logical RDF partition set.
pub const RDF_PARTITION_SET_NAMESPACE: &str = "rootlight.rdf_partition_set";

/// Explicit support for the bounded critical RDF set member format.
///
/// Other critical extensions retain the caller's normal rejection policy.
#[must_use]
pub fn rdf_partition_extension_support() -> ExtensionSupport {
    let mut support = ExtensionSupport::default();
    support
        .supported_critical
        .insert(ExtensionIdentifier::new(RDF_PARTITION_SET_NAMESPACE, "1"));
    support
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionCommitment {
    format_version: u16,
    file: FileId,
    source_hash: ContentHash,
    source_bytes: u64,
    record_count: u64,
    payload_bytes: u64,
    content_hash: ContentHash,
}

impl From<&RdfPartitionDescriptor> for PartitionCommitment {
    fn from(value: &RdfPartitionDescriptor) -> Self {
        Self {
            format_version: value.format_version,
            file: value.file,
            source_hash: value.source_hash,
            source_bytes: value.source_bytes,
            record_count: value.record_count,
            payload_bytes: value.payload_bytes,
            content_hash: value.content_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PartitionSetCarrier {
    Root { members: u64, set_hash: ContentHash },
    Member { partition: PartitionCommitment },
}

/// Commits an exact set using one bounded critical envelope per member.
///
/// A root envelope binds the ordered-set digest and count. Removing one member,
/// duplicating file ownership, or mixing independently committed sets fails
/// identity verification. No all-members extension payload is constructed.
///
/// # Errors
/// Rejects foreign descriptors, repeated files, inline ownership, invalid parent
/// anchors, encoding failure, or interruption. Empty input produces no set.
pub fn new_rdf_partition_set_envelopes(
    parent: &IdentityVerifiedGeneration,
    descriptors: &[RdfPartitionDescriptor],
    context: &GenerationContext<'_>,
) -> Result<Vec<ExtensionEnvelope>, RdfPartitionError> {
    let mut ordered = BTreeMap::new();
    for descriptor in descriptors {
        context.check()?;
        let file = parent
            .snapshot()
            .find_file(descriptor.file)
            .ok_or(RdfPartitionError::MissingFile)?;
        validate_partition_anchor(&PartitionCommitment::from(descriptor), file)?;
        if descriptor.metadata != parent.metadata()
            || ordered.insert(descriptor.file, descriptor).is_some()
        {
            return Err(RdfPartitionError::InvalidSet);
        }
    }
    for record in &parent.document().extensions {
        context.check()?;
        if record.namespace == RDF_STATEMENT_NAMESPACE
            && record
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| ordered.contains_key(&source.span().file()))
        {
            return Err(RdfPartitionError::OverlappingFile);
        }
    }
    let members = u64::try_from(ordered.len()).map_err(|_| RdfPartitionError::Encoding)?;
    if members == 0 {
        return Ok(Vec::new());
    }
    context.require(
        GenerationResource::Rows,
        members.checked_add(1).ok_or(RdfPartitionError::Encoding)?,
    )?;
    let commitments = ordered
        .values()
        .map(|descriptor| PartitionCommitment::from(*descriptor))
        .collect::<Vec<_>>();
    let set_hash = partition_set_hash(&commitments, context)?;
    let mut envelopes = Vec::new();
    envelopes
        .try_reserve_exact(
            commitments
                .len()
                .checked_add(1)
                .ok_or(RdfPartitionError::Encoding)?,
        )
        .map_err(|_| RdfPartitionError::Encoding)?;
    let first = parent
        .snapshot()
        .find_file(commitments[0].file)
        .ok_or(RdfPartitionError::MissingFile)?;
    envelopes.push(partition_member_envelope(
        parent.metadata(),
        first,
        &PartitionSetCarrier::Root { members, set_hash },
    )?);
    for partition in commitments {
        context.check()?;
        let file = parent
            .snapshot()
            .find_file(partition.file)
            .ok_or(RdfPartitionError::MissingFile)?;
        validate_partition_anchor(&partition, file)?;
        envelopes.push(partition_member_envelope(
            parent.metadata(),
            file,
            &PartitionSetCarrier::Member { partition },
        )?);
    }
    Ok(envelopes)
}

fn partition_member_envelope(
    metadata: GenerationMetadata,
    file: &FileRecord,
    member: &PartitionSetCarrier,
) -> Result<ExtensionEnvelope, RdfPartitionError> {
    let payload = serde_json::to_string(member).map_err(|_| RdfPartitionError::Encoding)?;
    if payload.len() > 4096 {
        return Err(RdfPartitionError::InvalidSet);
    }
    let evidence = FactEvidence {
        source: file.evidence.source.clone(),
        derivation: vec![FactRef::File(file.id)],
    };
    let identity = serde_json::to_vec(&(
        RDF_PARTITION_SET_NAMESPACE,
        "1",
        metadata.repository(),
        metadata.generation(),
        file.provenance,
        &evidence,
        member,
    ))
    .map_err(|_| RdfPartitionError::Encoding)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.rdf_partition_set/v1", &identity).id(),
        repository: metadata.repository(),
        generation: metadata.generation(),
        namespace: RDF_PARTITION_SET_NAMESPACE.to_owned(),
        version: "1".to_owned(),
        criticality: ExtensionCriticality::Critical,
        provenance: file.provenance,
        evidence,
        payload,
    })
}

fn validate_partition_anchor(
    partition: &PartitionCommitment,
    file: &FileRecord,
) -> Result<(), RdfPartitionError> {
    let limits = IrLimits::default();
    if partition.format_version != RDF_PARTITION_FORMAT_VERSION
        || partition.file != file.id
        || partition.source_hash != file.content_hash
        || partition.source_bytes != file.byte_length
        || partition.record_count
            > u64::try_from(limits.max_extensions).map_err(|_| RdfPartitionError::Encoding)?
        || partition.payload_bytes
            > u64::try_from(limits.max_total_extension_bytes)
                .map_err(|_| RdfPartitionError::Encoding)?
        || (partition.record_count == 0) != (partition.payload_bytes == 0)
    {
        return Err(RdfPartitionError::InvalidSet);
    }
    Ok(())
}

fn partition_set_hash(
    commitments: &[PartitionCommitment],
    context: &GenerationContext<'_>,
) -> Result<ContentHash, RdfPartitionError> {
    let mut writer = DigestWriter {
        hasher: blake3::Hasher::new(),
        bytes: 0,
        context,
        control: None,
    };
    if serde_json::to_writer(&mut writer, commitments).is_err() {
        return Err(writer
            .control
            .map_or(RdfPartitionError::Encoding, RdfPartitionError::Control));
    }
    Ok(ContentHash::from_bytes(
        *writer.hasher.finalize().as_bytes(),
    ))
}

pub(crate) fn verify_rdf_partition_set(
    snapshot: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<Vec<RdfPartitionDescriptor>, RdfPartitionError> {
    let mut ordered = BTreeMap::new();
    let mut root = None;
    for envelope in &snapshot.document().extensions {
        context.check()?;
        if envelope.namespace != RDF_PARTITION_SET_NAMESPACE {
            continue;
        }
        if envelope.payload.len() > 4096 {
            return Err(RdfPartitionError::InvalidSet);
        }
        let carrier: PartitionSetCarrier =
            serde_json::from_str(&envelope.payload).map_err(|_| RdfPartitionError::InvalidSet)?;
        let PartitionSetCarrier::Member { partition } = carrier else {
            if root.replace((envelope, carrier)).is_some() {
                return Err(RdfPartitionError::InvalidSet);
            }
            continue;
        };
        let file = snapshot
            .find_file(partition.file)
            .ok_or(RdfPartitionError::MissingFile)?;
        validate_partition_anchor(&partition, file)?;
        if *envelope
            != partition_member_envelope(
                snapshot.metadata(),
                file,
                &PartitionSetCarrier::Member {
                    partition: partition.clone(),
                },
            )?
            || ordered.insert(partition.file, partition).is_some()
        {
            return Err(RdfPartitionError::InvalidSet);
        }
    }
    for record in &snapshot.document().extensions {
        context.check()?;
        if record.namespace == RDF_STATEMENT_NAMESPACE
            && record
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| ordered.contains_key(&source.span().file()))
        {
            return Err(RdfPartitionError::OverlappingFile);
        }
    }
    let commitments = ordered.into_values().collect::<Vec<_>>();
    match root {
        Some((envelope, PartitionSetCarrier::Root { members, set_hash })) => {
            let first = commitments.first().ok_or(RdfPartitionError::InvalidSet)?;
            let file = snapshot
                .find_file(first.file)
                .ok_or(RdfPartitionError::MissingFile)?;
            if members
                != u64::try_from(commitments.len()).map_err(|_| RdfPartitionError::Encoding)?
                || set_hash != partition_set_hash(&commitments, context)?
                || *envelope
                    != partition_member_envelope(
                        snapshot.metadata(),
                        file,
                        &PartitionSetCarrier::Root { members, set_hash },
                    )?
            {
                return Err(RdfPartitionError::InvalidSet);
            }
        }
        None if commitments.is_empty() => {}
        _ => return Err(RdfPartitionError::InvalidSet),
    }
    Ok(commitments
        .into_iter()
        .map(|partition| RdfPartitionDescriptor {
            metadata: snapshot.metadata(),
            format_version: partition.format_version,
            file: partition.file,
            source_hash: partition.source_hash,
            source_bytes: partition.source_bytes,
            record_count: partition.record_count,
            payload_bytes: partition.payload_bytes,
            content_hash: partition.content_hash,
            requires_parent_authentication: true,
        })
        .collect())
}

/// Identity and content commitment for one independently bounded RDF file.
///
/// Verified record admission permits standalone cold authentication. A descriptor
/// restored from a base commitment additionally requires that final parent's
/// provenance check. Neither origin grants publication of an incomplete set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RdfPartitionDescriptor {
    format_version: u16,
    metadata: GenerationMetadata,
    file: FileId,
    source_hash: ContentHash,
    source_bytes: u64,
    record_count: u64,
    payload_bytes: u64,
    content_hash: ContentHash,
    requires_parent_authentication: bool,
}

impl RdfPartitionDescriptor {
    /// Returns whether this commitment still needs the final parent's anchors.
    ///
    /// This is runtime authority, excluded from durable v1 identity and headers.
    #[must_use]
    pub const fn requires_parent_authentication(&self) -> bool {
        self.requires_parent_authentication
    }
    /// Returns the exact durable partition format bound by this descriptor.
    #[must_use]
    pub const fn format_version(&self) -> u16 {
        self.format_version
    }

    /// Returns the verified parent identity.
    #[must_use]
    pub const fn metadata(&self) -> GenerationMetadata {
        self.metadata
    }

    /// Returns the only file whose RDF statements may occur in this partition.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the immutable parent file's content hash.
    #[must_use]
    pub const fn source_hash(&self) -> ContentHash {
        self.source_hash
    }

    /// Returns the immutable parent file's byte length.
    #[must_use]
    pub const fn source_bytes(&self) -> u64 {
        self.source_bytes
    }

    /// Returns the exact number of persisted RDF statements.
    #[must_use]
    pub const fn record_count(&self) -> u64 {
        self.record_count
    }

    /// Returns canonical RDF payload bytes, excluding envelope metadata.
    #[must_use]
    pub const fn payload_bytes(&self) -> u64 {
        self.payload_bytes
    }

    /// Returns BLAKE3 of the stable-ID-ordered canonical JSON envelope array.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }
}

/// A canonical RDF payload admitted against verified parent file/provenance anchors.
#[derive(Debug, PartialEq, Eq)]
pub struct VerifiedRdfPartition {
    descriptor: RdfPartitionDescriptor,
    records: Vec<ExtensionEnvelope>,
}

/// Locally canonical RDF awaiting authentication against a complete parent.
///
/// This type grants no verified descriptor, reader, or publication authority.
/// It owns at most one file's unchanged, quota-admitted RDF payload.
#[derive(Debug, PartialEq, Eq)]
pub struct UnpublishedRdfPartition {
    metadata: GenerationMetadata,
    file: FileId,
    source_hash: ContentHash,
    source_bytes: u64,
    records: Vec<ExtensionEnvelope>,
    usage: IrDocumentUsage,
    content_hash: ContentHash,
    encoded_bytes: u64,
}

impl UnpublishedRdfPartition {
    /// Admits canonical records against a claimed immutable file before parent verification.
    ///
    /// Claims remain untrusted until [`Self::authenticate`] succeeds. The producer
    /// binds source bytes to its captured source frame; no source is acquired here.
    ///
    /// # Errors
    /// Rejects malformed/duplicate identities, foreign claimed source ownership,
    /// unchanged IR quota violations, or interruption. Provenance is checked later.
    pub fn admit(
        metadata: GenerationMetadata,
        file: FileId,
        source_hash: ContentHash,
        source_bytes: u64,
        mut records: Vec<ExtensionEnvelope>,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<Self, RdfPartitionError> {
        context.check()?;
        let mut usage = IrDocumentUsage::default();
        for record in &records {
            context.check()?;
            // Packed v1 reconstructs the original triple envelope and cannot carry v2 roles.
            if record.version != "1" {
                return Err(RdfPartitionError::InvalidRecord);
            }
            usage =
                usage.checked_add(IrDocumentUsage::measure_extension(record, limits)?, limits)?;
            let statement = decode_rdf_statement_envelope(record)
                .map_err(|_| RdfPartitionError::InvalidRecord)?;
            let source = statement.source();
            if record.repository != metadata.repository()
                || record.generation != metadata.generation()
                || source.span().file() != file
                || source.content_hash() != source_hash
                || source.span().end_byte() > source_bytes
            {
                return Err(RdfPartitionError::InvalidRecord);
            }
        }
        let count = u64::try_from(records.len()).map_err(|_| RdfPartitionError::Encoding)?;
        context.require(GenerationResource::Rows, count)?;
        context.require(GenerationResource::SourceReferences, count)?;
        let text_bytes = usage
            .total_string_bytes()
            .checked_add(usage.total_extension_bytes())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(RdfPartitionError::Encoding)?;
        context.require(GenerationResource::TextBytes, text_bytes)?;
        records.sort_unstable_by_key(|record| record.id);
        if records.windows(2).any(|pair| pair[0].id == pair[1].id) {
            return Err(RdfPartitionError::DuplicateIdentity);
        }
        let mut writer = DigestWriter {
            hasher: blake3::Hasher::new(),
            bytes: 0,
            context,
            control: None,
        };
        if serde_json::to_writer(&mut writer, &records).is_err() {
            return Err(writer
                .control
                .map_or(RdfPartitionError::Encoding, RdfPartitionError::Control));
        }
        context.check()?;
        Ok(Self {
            metadata,
            file,
            source_hash,
            source_bytes,
            records,
            usage,
            content_hash: ContentHash::from_bytes(*writer.hasher.finalize().as_bytes()),
            encoded_bytes: writer.bytes,
        })
    }

    /// Requires the retained census to fit this operation's current resource budget.
    ///
    /// # Errors
    /// Returns typed cancellation or row, source-reference, text, or encoded-text
    /// budget failures before an admitted payload is handed to another operation.
    pub fn require_context(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<(), RdfPartitionError> {
        context.check()?;
        let count = u64::try_from(self.records.len()).map_err(|_| RdfPartitionError::Encoding)?;
        context.require(GenerationResource::Rows, count)?;
        context.require(GenerationResource::SourceReferences, count)?;
        let text_bytes = self
            .usage
            .total_string_bytes()
            .checked_add(self.usage.total_extension_bytes())
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(RdfPartitionError::Encoding)?;
        context.require(GenerationResource::TextBytes, text_bytes)?;
        context.require(GenerationResource::EncodedTextBytes, self.encoded_bytes)?;
        Ok(())
    }

    /// Authenticates the locally admitted file against the final complete parent.
    ///
    /// # Errors
    /// Rejects parent/file/hash drift, inline overlap, unknown provenance, duplicate
    /// parent identities, stricter quota violations, or interruption.
    pub fn authenticate(
        self,
        parent: &IdentityVerifiedGeneration,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<VerifiedRdfPartition, RdfPartitionError> {
        context.check()?;
        let anchor = parent
            .snapshot()
            .find_file(self.file)
            .ok_or(RdfPartitionError::MissingFile)?;
        if parent.metadata() != self.metadata
            || anchor.content_hash != self.source_hash
            || anchor.byte_length != self.source_bytes
        {
            return Err(RdfPartitionError::InvalidRecord);
        }
        if parent.document().extensions.iter().any(|record| {
            record.namespace == RDF_STATEMENT_NAMESPACE
                && record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == self.file)
        }) {
            return Err(RdfPartitionError::OverlappingFile);
        }
        self.usage
            .clone()
            .checked_add(IrDocumentUsage::default(), limits)?;
        let count = u64::try_from(self.records.len()).map_err(|_| RdfPartitionError::Encoding)?;
        self.require_context(context)?;
        for record in &self.records {
            context.check()?;
            if parent
                .document()
                .provenance
                .binary_search_by_key(&record.provenance, |p| p.id)
                .is_err()
            {
                return Err(RdfPartitionError::UnknownProvenance);
            }
            if parent
                .document()
                .extensions
                .binary_search_by_key(&record.id, |e| e.id)
                .is_ok()
            {
                return Err(RdfPartitionError::DuplicateIdentity);
            }
        }
        let descriptor = RdfPartitionDescriptor {
            format_version: RDF_PARTITION_FORMAT_VERSION,
            metadata: self.metadata,
            file: self.file,
            source_hash: self.source_hash,
            source_bytes: self.source_bytes,
            record_count: count,
            payload_bytes: u64::try_from(self.usage.total_extension_bytes())
                .map_err(|_| RdfPartitionError::Encoding)?,
            content_hash: self.content_hash,
            requires_parent_authentication: false,
        };
        Ok(VerifiedRdfPartition {
            descriptor,
            records: self.records,
        })
    }

    /// Returns the unverified claimed parent metadata.
    #[must_use]
    pub const fn metadata(&self) -> GenerationMetadata {
        self.metadata
    }
    /// Returns the unverified claimed file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }
    /// Returns the locally checked claimed source hash.
    #[must_use]
    pub const fn source_hash(&self) -> ContentHash {
        self.source_hash
    }
    /// Returns the locally checked claimed source byte length.
    #[must_use]
    pub const fn source_bytes(&self) -> u64 {
        self.source_bytes
    }
    /// Returns canonical records without granting a verified capability.
    #[must_use]
    pub fn records(&self) -> &[ExtensionEnvelope] {
        &self.records
    }
    /// Returns canonical payload bytes admitted under the unchanged quotas.
    #[must_use]
    pub fn payload_bytes(&self) -> usize {
        self.usage.total_extension_bytes()
    }
    /// Returns the local content commitment, which is not parent authentication.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }
}

impl VerifiedRdfPartition {
    /// Admits and authenticates one file without materializing the combined generation.
    ///
    /// # Errors
    /// Rejects invalid records, missing/overlapping ownership, unknown provenance,
    /// duplicate identities, unchanged quota violations, or interruption.
    pub fn verify(
        parent: &IdentityVerifiedGeneration,
        file: FileId,
        records: Vec<ExtensionEnvelope>,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<Self, RdfPartitionError> {
        let anchor = parent
            .snapshot()
            .find_file(file)
            .ok_or(RdfPartitionError::MissingFile)?;
        UnpublishedRdfPartition::admit(
            parent.metadata(),
            file,
            anchor.content_hash,
            anchor.byte_length,
            records,
            limits,
            context,
        )?
        .authenticate(parent, limits, context)
    }

    /// Returns the trusted content commitment for durable storage and cold reopen.
    #[must_use]
    pub const fn descriptor(&self) -> &RdfPartitionDescriptor {
        &self.descriptor
    }

    /// Borrows canonical records in strictly increasing stable-ID order.
    #[must_use]
    pub fn records(&self) -> &[ExtensionEnvelope] {
        &self.records
    }
}

struct DigestWriter<'a, 'b> {
    hasher: blake3::Hasher,
    bytes: u64,
    context: &'a GenerationContext<'b>,
    control: Option<GenerationControlError>,
}

impl Write for DigestWriter<'_, '_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let observed = self
            .bytes
            .checked_add(u64::try_from(bytes.len()).map_err(io::Error::other)?)
            .ok_or_else(|| io::Error::other("partition size overflow"))?;
        if let Err(error) = self
            .context
            .require(GenerationResource::EncodedTextBytes, observed)
        {
            self.control = Some(error);
            return Err(io::Error::other("partition control interrupted"));
        }
        self.hasher.update(bytes);
        self.bytes = observed;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Failed bounded RDF admission; no verified capability is returned.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum RdfPartitionError {
    /// The requested file is absent from the verified parent.
    #[error("missing verified partition file")]
    MissingFile,
    /// The parent already owns inline RDF for this file.
    #[error("overlapping inline rdf ownership")]
    OverlappingFile,
    /// A typed record or source identity is invalid.
    #[error("invalid rdf partition record")]
    InvalidRecord,
    /// A record references provenance absent from the verified parent.
    #[error("unknown rdf partition provenance")]
    UnknownProvenance,
    /// A stable identity is duplicated within the payload or parent.
    #[error("duplicate rdf partition identity")]
    DuplicateIdentity,
    /// A canonical logical-set commitment is incomplete or inconsistent.
    #[error("invalid rdf partition set")]
    InvalidSet,
    /// The unchanged IR quotas rejected the partition.
    #[error(transparent)]
    Quota(#[from] IrDocumentUsageError),
    /// Cancellation or an operation budget stopped admission.
    #[error(transparent)]
    Control(#[from] GenerationControlError),
    /// Canonical encoding or checked size accounting failed.
    #[error("rdf partition encoding failed")]
    Encoding,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GENERATION_CONTRACT_VERSION, GenerationBudget};
    use rootlight_cancel::Cancellation;
    use rootlight_ids::{
        FactId, GenerationIdentity, RepositoryId, content_hash, derive_generation,
    };
    use rootlight_ir::{RdfStatement, SourceRef, SourceSpan, new_rdf_statement_envelope};

    #[test]
    fn packed_v1_admission_refuses_named_and_default_graph_v2_statements() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let hash = content_hash(b"metadata");
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash: hash,
            config_hash: hash,
            provider_set_hash: hash,
            format_version: (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
                | u32::from(GENERATION_CONTRACT_VERSION.minor()),
        })
        .id();
        let metadata =
            GenerationMetadata::new(repository, generation, None, hash, hash, hash).unwrap();
        let file = FileId::from_bytes([2; 20]);
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for graph in [None, Some("<g:>"), Some("_:g")] {
            let text = format!("<s:><p:><o:>{}.", graph.unwrap_or(""));
            let source_bytes = u64::try_from(text.len()).unwrap();
            let source_hash = content_hash(text.as_bytes());
            let source = SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, source_bytes).unwrap(),
                source_hash,
                None,
            );
            let spans = [(0, 4), (4, 8), (8, 12)]
                .map(|(start, end)| SourceSpan::new(file, start, end).unwrap());
            let statement = RdfStatement::from_nquads(
                source.clone(),
                &text,
                spans,
                graph.map(|graph| {
                    SourceSpan::new(file, 12, 12 + u64::try_from(graph.len()).unwrap()).unwrap()
                }),
            )
            .unwrap();
            let envelope =
                new_rdf_statement_envelope(FactId::from_bytes([3; 20]), &statement).unwrap();
            assert_eq!(decode_rdf_statement_envelope(&envelope).unwrap(), statement);
            assert_eq!(
                UnpublishedRdfPartition::admit(
                    metadata,
                    file,
                    source_hash,
                    source_bytes,
                    vec![envelope],
                    &IrLimits::default(),
                    &context
                ),
                Err(RdfPartitionError::InvalidRecord)
            );
            if graph.is_none() {
                let triple = RdfStatement::from_ntriples(source, &text, spans).unwrap();
                let envelope =
                    new_rdf_statement_envelope(FactId::from_bytes([3; 20]), &triple).unwrap();
                assert!(
                    UnpublishedRdfPartition::admit(
                        metadata,
                        file,
                        source_hash,
                        source_bytes,
                        vec![envelope],
                        &IrLimits::default(),
                        &context
                    )
                    .is_ok()
                );
            }
        }
    }
}
