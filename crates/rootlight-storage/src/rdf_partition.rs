//! Identity-verified, file-affine RDF payloads outside an owned generation.
//!
//! The parent remains the authority for files and provenance. A partition is
//! independently bounded and is not a published generation or coverage claim.

use std::io::{self, Write};

use rootlight_ids::{ContentHash, FileId};
use rootlight_ir::{
    ExtensionEnvelope, IrDocumentUsage, IrDocumentUsageError, IrLimits, RDF_STATEMENT_NAMESPACE,
    decode_rdf_statement_envelope,
};

use crate::{
    GenerationContext, GenerationControlError, GenerationMetadata, GenerationResource,
    IdentityVerifiedGeneration,
};

/// Exact durable RDF partition format version.
pub const RDF_PARTITION_FORMAT_VERSION: u16 = 1;

/// Trusted identity and content commitment for one independently bounded RDF file.
///
/// This value can only be obtained from verified admission. It does not grant
/// publication authority; a publisher must authenticate the complete partition set.
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
}

impl RdfPartitionDescriptor {
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

impl VerifiedRdfPartition {
    /// Admits one file's RDF records without materializing the combined generation.
    ///
    /// Source bytes must already have been bound by the producer to the parent's
    /// immutable source frame. Typed decoding authenticates canonical records;
    /// it does not independently acquire or reparse that source frame.
    ///
    /// # Errors
    /// Rejects missing/overlapping parent ownership, unknown provenance, malformed
    /// or duplicate identities, unchanged IR quota violations, or interruption.
    pub fn verify(
        parent: &IdentityVerifiedGeneration,
        file: FileId,
        mut records: Vec<ExtensionEnvelope>,
        limits: &IrLimits,
        context: &GenerationContext<'_>,
    ) -> Result<Self, RdfPartitionError> {
        context.check()?;
        let anchor = parent
            .snapshot()
            .find_file(file)
            .ok_or(RdfPartitionError::MissingFile)?;
        if parent.document().extensions.iter().any(|record| {
            record.namespace == RDF_STATEMENT_NAMESPACE
                && record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file)
        }) {
            return Err(RdfPartitionError::OverlappingFile);
        }
        let mut usage = IrDocumentUsage::default();
        for record in &records {
            context.check()?;
            usage =
                usage.checked_add(IrDocumentUsage::measure_extension(record, limits)?, limits)?;
            let statement = decode_rdf_statement_envelope(record)
                .map_err(|_| RdfPartitionError::InvalidRecord)?;
            let source = statement.source();
            if record.repository != parent.metadata().repository()
                || record.generation != parent.metadata().generation()
                || source.span().file() != file
                || source.content_hash() != anchor.content_hash
                || source.span().end_byte() > anchor.byte_length
            {
                return Err(RdfPartitionError::InvalidRecord);
            }
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
        let count = u64::try_from(records.len()).map_err(|_| RdfPartitionError::Encoding)?;
        context.require(GenerationResource::Rows, count)?;
        context.require(GenerationResource::SourceReferences, count)?;
        let payload_bytes = u64::try_from(usage.total_extension_bytes())
            .map_err(|_| RdfPartitionError::Encoding)?;
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
        let descriptor = RdfPartitionDescriptor {
            format_version: RDF_PARTITION_FORMAT_VERSION,
            metadata: parent.metadata(),
            file,
            source_hash: anchor.content_hash,
            source_bytes: anchor.byte_length,
            record_count: count,
            payload_bytes,
            content_hash: ContentHash::from_bytes(*writer.hasher.finalize().as_bytes()),
        };
        Ok(Self {
            descriptor,
            records,
        })
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
