//! Generation-neutral normalized chunks and bounded generation rebinding.
//!
//! Canonical chunks are process-local reuse artifacts. They never change the
//! durable IR format and accept only extension namespaces with identity recipes
//! that this module can completely reconstruct.

#[cfg(test)]
use std::collections::BTreeSet;

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId};
use serde::Serialize;

use crate::lexical::rebind_lexical_evidence_subject;
use crate::validation::validate_canonical_ir_document;
use crate::{
    CoverageRecord, DiagnosticRecord, ExtensionEnvelope, ExtensionSupport,
    FILE_IDENTITY_CLAIM_NAMESPACE, FactEvidence, FactRef, FileRecord, IrDocumentValidationError,
    IrLimits, LEXICAL_EXTENSION_NAMESPACE, NormalizedIrDocument, OccurrenceRecord,
    ProvenanceRecord, RelationEndpoint, RelationRecord, SYMBOL_IDENTITY_CLAIM_NAMESPACE,
    SkippedRegion, SourceMappingRecord, SourceRef, UnknownNoncriticalExtensionPolicy,
    canonicalize_ir_document, decode_file_identity_claim_envelope,
    decode_lexical_evidence_envelope, decode_symbol_identity_claim_envelope,
    derive_coverage_record_id, derive_diagnostic_record_id, derive_occurrence_record_id,
    derive_provenance_record_id, derive_relation_record_id, derive_skipped_region_id,
    derive_source_mapping_record_id, new_file_identity_claim_envelope,
    new_lexical_evidence_envelope, new_symbol_identity_claim_envelope,
};

const GENERATION_NEUTRAL_ID: GenerationId = GenerationId::from_bytes([0; 20]);
const CHUNK_DIGEST_DOMAIN: &str = "rootlight.normalized-file-chunk/v1";

/// A canonical generation-neutral normalized chunk for one source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalNormalizedFileChunk {
    document: NormalizedIrDocument,
    file: FileId,
    digest: ContentHash,
    encoded_bytes: usize,
}

/// A complete canonical IR document with generation-owned identity neutralized.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CanonicalGenerationNeutralDocument {
    document: NormalizedIrDocument,
    fact_id_map: FactIdMap,
}

/// Owned canonical IR carrying the validation proof required by fast projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalNormalizedIrDocument {
    document: NormalizedIrDocument,
}

impl CanonicalNormalizedIrDocument {
    /// Validates and canonicalizes one normalized document.
    ///
    /// # Errors
    ///
    /// Returns [`IrDocumentValidationError`] for the same contract, quota,
    /// ownership, reference, and extension failures as
    /// [`canonicalize_ir_document`].
    pub fn new(
        document: NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
    ) -> Result<Self, IrDocumentValidationError> {
        canonicalize_ir_document(document, limits, extensions).map(|document| Self { document })
    }

    /// Returns the canonical validated document.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        &self.document
    }

    /// Consumes the proof wrapper into its canonical document.
    #[must_use]
    pub fn into_document(self) -> NormalizedIrDocument {
        self.document
    }

    /// Streams generation-neutral digests without repeating validation.
    ///
    /// # Errors
    ///
    /// Returns [`NormalizedRebindError`] for unsupported identity recipes,
    /// resource exhaustion, or caller interruption.
    pub fn generation_neutral_digests(
        &self,
        checkpoint: impl FnMut() -> bool,
    ) -> Result<CanonicalGenerationNeutralDigests, NormalizedRebindError> {
        canonical_generation_neutral_digests_for_prevalidated_document(&self.document, checkpoint)
    }
}

/// Compact digests of a complete generation-neutral canonical IR projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CanonicalGenerationNeutralDigests {
    complete: ContentHash,
    coverage: ContentHash,
    provenance: ContentHash,
    stable_identities: ContentHash,
    records: u64,
    coverage_records: u64,
    provenance_records: u64,
    stable_identities_records: u64,
}

impl CanonicalGenerationNeutralDigests {
    /// Returns the digest binding every normalized record and document header.
    #[must_use]
    pub const fn complete(self) -> ContentHash {
        self.complete
    }

    /// Returns the digest binding coverage, skipped-region, and diagnostic records.
    #[must_use]
    pub const fn coverage(self) -> ContentHash {
        self.coverage
    }

    /// Returns the digest binding every provenance record.
    #[must_use]
    pub const fn provenance(self) -> ContentHash {
        self.provenance
    }

    /// Returns the digest binding stable and reconstructed fact identities.
    #[must_use]
    pub const fn stable_identities(self) -> ContentHash {
        self.stable_identities
    }

    /// Returns the number of records bound by the complete digest.
    #[must_use]
    pub const fn records(self) -> u64 {
        self.records
    }

    /// Returns the number of coverage-domain records.
    #[must_use]
    pub const fn coverage_records(self) -> u64 {
        self.coverage_records
    }

    /// Returns the number of provenance records.
    #[must_use]
    pub const fn provenance_records(self) -> u64 {
        self.provenance_records
    }

    /// Returns the number of stable identities.
    #[must_use]
    pub const fn stable_identities_records(self) -> u64 {
        self.stable_identities_records
    }
}

impl CanonicalGenerationNeutralDocument {
    /// Canonicalizes and rebinds a complete document to the neutral generation.
    ///
    /// Repository-, file-, and symbol-scoped identities remain unchanged.
    /// Generation-dependent fact identities and every reference to them are
    /// reconstructed through the same path used by incremental chunk reuse.
    ///
    /// # Errors
    ///
    /// Returns [`NormalizedRebindError`] when the input violates IR limits,
    /// contains an unsupported extension, has an incomplete or cyclic fact
    /// graph, or cannot reconstruct every typed identity.
    fn new(
        document: &NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
    ) -> Result<Self, NormalizedRebindError> {
        require_supported_extensions(document)?;
        let canonical = canonical_preserving_extensions(document.clone(), limits, extensions)?;
        let (document, fact_id_map) =
            rebind_document_with_map(canonical, GENERATION_NEUTRAL_ID, limits, extensions)?;
        Ok(Self {
            document,
            fact_id_map,
        })
    }

    /// Returns the reconstructed identity for one source-generation fact.
    #[must_use]
    #[cfg(test)]
    fn rebind_fact_id(&self, fact: FactId) -> Option<FactId> {
        self.fact_id_map.get(fact)
    }
}

/// Streams compact generation-neutral digests for one complete canonical document.
///
/// The implementation retains bounded compact graph workspace proportional to
/// the document's facts and dependency edges, plus one rebound record at a
/// time. `checkpoint` is called throughout graph traversal and record hashing
/// so callers can preserve their cancellation contract without adding a
/// transport dependency to this crate.
///
/// Identity ordering uses in-place sorts over that pre-admitted workspace.
/// Each sort is one atomic bounded operation, with cancellation checkpoints
/// immediately before and after it.
///
/// # Errors
///
/// Returns [`NormalizedRebindError`] when the document is invalid, contains an
/// unsupported extension, cannot reconstruct every identity, exceeds checked
/// record accounting, or the caller checkpoint returns `false`.
pub fn canonical_generation_neutral_digests(
    document: &NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<CanonicalGenerationNeutralDigests, NormalizedRebindError> {
    validate_canonical_ir_document(document, limits, extensions)?;
    canonical_generation_neutral_digests_for_prevalidated_document(document, &mut checkpoint)
}

/// Streams neutral digests for a document already proven canonical and valid.
///
/// This is the publication entry point for a caller retaining an external
/// identity-verification proof. It deliberately avoids repeating the
/// allocation-heavy general IR validator.
///
/// # Errors
///
/// Returns [`NormalizedRebindError`] when unsupported extensions or identity
/// reconstruction prevent a complete projection, resource accounting fails,
/// or the caller checkpoint returns `false`.
fn canonical_generation_neutral_digests_for_prevalidated_document(
    document: &NormalizedIrDocument,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<CanonicalGenerationNeutralDigests, NormalizedRebindError> {
    require_supported_extensions_with_checkpoint(document, &mut checkpoint)?;
    let ids = derive_fact_id_map(document, GENERATION_NEUTRAL_ID, &mut checkpoint)?;
    let mut hashers = LogicalProjectionHashers::new();
    hashers
        .complete
        .record(&(document.version, document.repository, GENERATION_NEUTRAL_ID))?;
    hashers.stable.record(&document.repository)?;

    hashers.complete.category(1, document.files.len())?;
    hashers.stable.category(1, document.files.len())?;
    for record in &document.files {
        ensure_projection_continues(&mut checkpoint)?;
        let record = rebind_file(record.clone(), GENERATION_NEUTRAL_ID, &ids, &mut checkpoint)?;
        hashers.complete.record(&record)?;
        ensure_projection_continues(&mut checkpoint)?;
        hashers.stable.record(&record.id)?;
    }
    hashers.complete.category(2, document.entities.len())?;
    hashers.stable.category(2, document.entities.len())?;
    for record in &document.entities {
        ensure_projection_continues(&mut checkpoint)?;
        let record = rebind_entity(record.clone(), GENERATION_NEUTRAL_ID, &ids, &mut checkpoint)?;
        hashers.complete.record(&record)?;
        ensure_projection_continues(&mut checkpoint)?;
        hashers.stable.record(&record.id)?;
    }

    type FactCategory = (u8, usize, fn(usize) -> FactLocation);
    let fact_categories: [FactCategory; 8] = [
        (3, document.occurrences.len(), FactLocation::Occurrence),
        (4, document.relations.len(), FactLocation::Relation),
        (5, document.provenance.len(), FactLocation::Provenance),
        (
            6,
            document.source_mappings.len(),
            FactLocation::SourceMapping,
        ),
        (7, document.coverage_records.len(), FactLocation::Coverage),
        (8, document.skipped_regions.len(), FactLocation::Skipped),
        (9, document.diagnostics.len(), FactLocation::Diagnostic),
        (10, document.extensions.len(), FactLocation::Extension),
    ];
    for (category, length, constructor) in fact_categories {
        hashers.complete.category(category, length)?;
        hashers.stable.category(category, length)?;
        match constructor(0) {
            FactLocation::Provenance(_) => hashers.provenance.category(category, length)?,
            FactLocation::Coverage(_) | FactLocation::Skipped(_) | FactLocation::Diagnostic(_) => {
                hashers.coverage.category(category, length)?
            }
            _ => {}
        }
        let mut neutral_order = Vec::new();
        neutral_order
            .try_reserve_exact(length)
            .map_err(|_| NormalizedRebindError::ResourceLimit)?;
        for index in 0..length {
            ensure_projection_continues(&mut checkpoint)?;
            let location = constructor(index);
            let old_id = location.id(document);
            let new_id = ids
                .get(old_id)
                .ok_or(NormalizedRebindError::IdentityRecipe)?;
            neutral_order.push((new_id, index));
        }
        ensure_projection_continues(&mut checkpoint)?;
        // Rust's in-place sort is an atomic bounded step. The admitted compact
        // workspace bounds its input, while checkpoints bracket the operation.
        neutral_order.sort_unstable();
        ensure_projection_continues(&mut checkpoint)?;
        for pair in neutral_order.windows(2) {
            ensure_projection_continues(&mut checkpoint)?;
            if pair[0].0 == pair[1].0 {
                return Err(NormalizedRebindError::IdentityRecipe);
            }
        }
        for (_, index) in neutral_order {
            ensure_projection_continues(&mut checkpoint)?;
            hash_rebound_fact(
                constructor(index),
                document,
                &ids,
                &mut hashers,
                &mut checkpoint,
            )?;
        }
    }

    let records = checked_record_count(document)?;
    let coverage_records = document
        .coverage_records
        .len()
        .checked_add(document.skipped_regions.len())
        .and_then(|count| count.checked_add(document.diagnostics.len()))
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(NormalizedRebindError::ResourceLimit)?;
    let provenance_records = u64::try_from(document.provenance.len())
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    Ok(CanonicalGenerationNeutralDigests {
        complete: hashers.complete.finish(),
        coverage: hashers.coverage.finish(),
        provenance: hashers.provenance.finish(),
        stable_identities: hashers.stable.finish(),
        records,
        coverage_records,
        provenance_records,
        stable_identities_records: records,
    })
}

/// Estimates temporary graph workspace for streamed neutral projection.
///
/// The estimate covers only compact graph storage. Callers must separately
/// admit the retained document and conservative bounds for the source-record
/// clone and rebound-record buffers that can coexist with this workspace.
///
/// # Errors
///
/// Returns [`NormalizedRebindError::ResourceLimit`] when checked accounting is
/// not representable, or [`NormalizedRebindError::Interrupted`] when the
/// caller checkpoint returns `false`.
pub fn generation_neutral_workspace_bytes(
    document: &NormalizedIrDocument,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<u64, NormalizedRebindError> {
    ensure_projection_continues(&mut checkpoint)?;
    let facts = checked_fact_record_count(document)?;
    let dependencies = checked_dependency_count(document, &mut checkpoint)?;
    let per_fact = u64::try_from(
        std::mem::size_of::<(FactId, FactLocation)>()
            + std::mem::size_of::<(FactId, Option<FactId>)>()
            + std::mem::size_of::<usize>()
            + std::mem::size_of::<usize>()
            + std::mem::size_of::<(FactId, usize)>(),
    )
    .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    let per_dependency = u64::try_from(std::mem::size_of::<(usize, usize)>())
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    let bytes = facts
        .checked_mul(per_fact)
        .and_then(|bytes| {
            dependencies
                .checked_mul(per_dependency)
                .and_then(|edges| bytes.checked_add(edges))
        })
        .ok_or(NormalizedRebindError::ResourceLimit)?;
    ensure_projection_continues(&mut checkpoint)?;
    Ok(bytes)
}

impl CanonicalNormalizedFileChunk {
    /// Creates a canonical chunk from one complete file-scoped normalized document.
    ///
    /// The returned digest excludes the source generation by first rebinding
    /// every generation-dependent identity and reference to a fixed neutral
    /// generation. Opaque extensions are rejected because their payloads may
    /// hide generation-bound identities the core cannot rewrite.
    ///
    /// # Errors
    ///
    /// Returns [`NormalizedRebindError`] when the input violates IR limits,
    /// spans multiple file records, contains unsupported extensions, has a
    /// cyclic fact-reference graph, or cannot reconstruct every typed identity.
    pub fn new(
        document: &NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
    ) -> Result<Self, NormalizedRebindError> {
        let neutral = CanonicalGenerationNeutralDocument::new(document, limits, extensions)?;
        let [file] = neutral.document.files.as_slice() else {
            return Err(NormalizedRebindError::NotSingleFile);
        };
        let file = file.id;
        let document = neutral.document;
        let encoded =
            serde_json::to_vec(&document).map_err(|_| NormalizedRebindError::IdentityRecipe)?;
        let encoded_bytes = encoded.len();
        let mut hasher = blake3::Hasher::new_derive_key(CHUNK_DIGEST_DOMAIN);
        let length =
            u64::try_from(encoded_bytes).map_err(|_| NormalizedRebindError::ResourceLimit)?;
        hasher.update(&length.to_be_bytes());
        hasher.update(&encoded);
        Ok(Self {
            document,
            file,
            digest: ContentHash::from_bytes(*hasher.finalize().as_bytes()),
            encoded_bytes,
        })
    }

    /// Returns the single file owned by this chunk.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the generation-independent canonical chunk digest.
    #[must_use]
    pub const fn digest(&self) -> ContentHash {
        self.digest
    }

    /// Returns the canonical encoded byte charge used by bounded retention.
    #[must_use]
    pub const fn encoded_bytes(&self) -> usize {
        self.encoded_bytes
    }

    /// Rebinds the complete chunk to one child generation.
    ///
    /// # Errors
    ///
    /// Returns [`NormalizedRebindError`] if current limits or extension policy
    /// no longer accept the chunk, or if a complete identity reconstruction
    /// cannot be proven.
    pub fn rebind(
        &self,
        generation: GenerationId,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
    ) -> Result<NormalizedIrDocument, NormalizedRebindError> {
        rebind_document(self.document.clone(), generation, limits, extensions)
    }
}

/// Fail-closed normalized-chunk construction or rebind failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NormalizedRebindError {
    /// The document did not contain exactly one owned file record.
    #[error("normalized reuse chunk is not file scoped")]
    NotSingleFile,
    /// An extension namespace has no complete core-owned rebind recipe.
    #[error("normalized reuse chunk contains an unsupported extension")]
    UnsupportedExtension,
    /// Fact references form a cycle and therefore have no deterministic rebind order.
    #[error("normalized reuse chunk contains cyclic fact references")]
    CyclicFactReferences,
    /// A referenced fact was missing from the bounded chunk.
    #[error("normalized reuse chunk contains an external fact reference")]
    ExternalFactReference,
    /// A typed fact or extension identity could not be reconstructed.
    #[error("normalized reuse identity reconstruction failed")]
    IdentityRecipe,
    /// The document violated the normalized IR contract or configured limits.
    #[error("normalized reuse chunk is invalid")]
    InvalidDocument,
    /// Checked byte or record accounting exceeded the platform representation.
    #[error("normalized reuse chunk exceeded its resource limit")]
    ResourceLimit,
    /// A caller checkpoint interrupted generation-neutral projection.
    #[error("normalized generation-neutral projection was interrupted")]
    Interrupted,
}

impl From<IrDocumentValidationError> for NormalizedRebindError {
    fn from(_: IrDocumentValidationError) -> Self {
        Self::InvalidDocument
    }
}

struct ProjectionHasher {
    hasher: blake3::Hasher,
}

struct LogicalProjectionHashers {
    complete: ProjectionHasher,
    coverage: ProjectionHasher,
    provenance: ProjectionHasher,
    stable: ProjectionHasher,
}

impl LogicalProjectionHashers {
    fn new() -> Self {
        Self {
            complete: ProjectionHasher::new("rootlight.ir.logical.complete/1"),
            coverage: ProjectionHasher::new("rootlight.ir.logical.coverage/1"),
            provenance: ProjectionHasher::new("rootlight.ir.logical.provenance/1"),
            stable: ProjectionHasher::new("rootlight.ir.logical.stable-identities/1"),
        }
    }
}

impl ProjectionHasher {
    fn new(context: &'static str) -> Self {
        Self {
            hasher: blake3::Hasher::new_derive_key(context),
        }
    }

    fn category(&mut self, discriminator: u8, records: usize) -> Result<(), NormalizedRebindError> {
        let records = u64::try_from(records).map_err(|_| NormalizedRebindError::ResourceLimit)?;
        self.hasher.update(&[discriminator]);
        self.hasher.update(&records.to_be_bytes());
        Ok(())
    }

    fn record(&mut self, record: &impl Serialize) -> Result<(), NormalizedRebindError> {
        let mut record_hasher = RecordHasher {
            hasher: blake3::Hasher::new_derive_key("rootlight.ir.logical.record/1"),
            bytes: 0,
        };
        serde_json::to_writer(&mut record_hasher, record)
            .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
        self.hasher.update(&record_hasher.bytes.to_be_bytes());
        self.hasher
            .update(record_hasher.hasher.finalize().as_bytes());
        Ok(())
    }

    fn finish(self) -> ContentHash {
        ContentHash::from_bytes(*self.hasher.finalize().as_bytes())
    }
}

struct RecordHasher {
    hasher: blake3::Hasher,
    bytes: u64,
}

impl std::io::Write for RecordHasher {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| std::io::Error::other("record byte length is not representable"))?;
        self.bytes = self
            .bytes
            .checked_add(length)
            .ok_or_else(|| std::io::Error::other("record byte accounting overflowed"))?;
        self.hasher.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
enum FactNode {
    Occurrence(OccurrenceRecord),
    Relation(RelationRecord),
    Provenance(ProvenanceRecord),
    SourceMapping(SourceMappingRecord),
    Coverage(CoverageRecord),
    Skipped(SkippedRegion),
    Diagnostic(DiagnosticRecord),
    Extension(ExtensionEnvelope),
}

impl FactNode {
    fn dependencies(
        &self,
        checkpoint: &mut impl FnMut() -> bool,
    ) -> Result<Vec<FactId>, NormalizedRebindError> {
        ensure_projection_continues(checkpoint)?;
        let mut dependencies = Vec::new();
        let capacity = match self {
            Self::Occurrence(record) => 1_usize.checked_add(record.evidence.derivation.len()),
            Self::Relation(record) => 3_usize.checked_add(record.evidence.derivation.len()),
            Self::Provenance(record) => Some(record.derivation_parents.len()),
            Self::SourceMapping(record) => 1_usize.checked_add(record.evidence.derivation.len()),
            Self::Coverage(record) => 1_usize.checked_add(record.evidence.derivation.len()),
            Self::Skipped(record) => 1_usize.checked_add(record.evidence.derivation.len()),
            Self::Diagnostic(record) => 1_usize.checked_add(record.evidence.derivation.len()),
            Self::Extension(record) => 2_usize.checked_add(record.evidence.derivation.len()),
        }
        .ok_or(NormalizedRebindError::ResourceLimit)?;
        dependencies
            .try_reserve_exact(capacity)
            .map_err(|_| NormalizedRebindError::ResourceLimit)?;
        match self {
            Self::Occurrence(record) => {
                dependencies.push(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
            }
            Self::Relation(record) => {
                dependencies.push(record.provenance);
                collect_endpoint_dependency(record.subject, &mut dependencies);
                collect_endpoint_dependency(record.object, &mut dependencies);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
            }
            Self::Provenance(record) => {
                collect_fact_ref_dependencies(
                    &record.derivation_parents,
                    &mut dependencies,
                    checkpoint,
                )?;
            }
            Self::SourceMapping(record) => {
                dependencies.push(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
            }
            Self::Coverage(record) => {
                dependencies.push(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
            }
            Self::Skipped(record) => {
                dependencies.push(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
            }
            Self::Diagnostic(record) => {
                dependencies.push(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
            }
            Self::Extension(record) => {
                dependencies.push(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies, checkpoint)?;
                if record.namespace == LEXICAL_EXTENSION_NAMESPACE {
                    let evidence = decode_lexical_evidence_envelope(record)
                        .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
                    collect_fact_ref_dependency(evidence.subject(), &mut dependencies);
                }
            }
        }
        ensure_projection_continues(checkpoint)?;
        // A single fact can carry a bounded derivation fan-in. Sorting is
        // in-place and therefore allocation-free; checkpoints bracket it.
        dependencies.sort_unstable();
        ensure_projection_continues(checkpoint)?;
        dependencies.dedup();
        Ok(dependencies)
    }
}

fn canonical_preserving_extensions(
    document: NormalizedIrDocument,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<NormalizedIrDocument, NormalizedRebindError> {
    let mut preservation = extensions.clone();
    preservation.unknown_noncritical = UnknownNoncriticalExtensionPolicy::Preserve;
    canonicalize_ir_document(document, limits, &preservation).map_err(Into::into)
}

fn require_supported_extensions(
    document: &NormalizedIrDocument,
) -> Result<(), NormalizedRebindError> {
    require_supported_extensions_with_checkpoint(document, &mut || true)
}

fn require_supported_extensions_with_checkpoint(
    document: &NormalizedIrDocument,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), NormalizedRebindError> {
    for extension in &document.extensions {
        ensure_projection_continues(checkpoint)?;
        if !matches!(
            extension.namespace.as_str(),
            FILE_IDENTITY_CLAIM_NAMESPACE
                | SYMBOL_IDENTITY_CLAIM_NAMESPACE
                | LEXICAL_EXTENSION_NAMESPACE
        ) {
            return Err(NormalizedRebindError::UnsupportedExtension);
        }
    }
    ensure_projection_continues(checkpoint)
}

#[derive(Clone, Copy)]
enum FactLocation {
    Occurrence(usize),
    Relation(usize),
    Provenance(usize),
    SourceMapping(usize),
    Coverage(usize),
    Skipped(usize),
    Diagnostic(usize),
    Extension(usize),
}

impl FactLocation {
    fn id(self, document: &NormalizedIrDocument) -> FactId {
        match self {
            Self::Occurrence(index) => document.occurrences[index].id,
            Self::Relation(index) => document.relations[index].id,
            Self::Provenance(index) => document.provenance[index].id,
            Self::SourceMapping(index) => document.source_mappings[index].id,
            Self::Coverage(index) => document.coverage_records[index].id,
            Self::Skipped(index) => document.skipped_regions[index].id,
            Self::Diagnostic(index) => document.diagnostics[index].id,
            Self::Extension(index) => document.extensions[index].id,
        }
    }

    fn node(self, document: &NormalizedIrDocument) -> FactNode {
        match self {
            Self::Occurrence(index) => FactNode::Occurrence(document.occurrences[index].clone()),
            Self::Relation(index) => FactNode::Relation(document.relations[index].clone()),
            Self::Provenance(index) => FactNode::Provenance(document.provenance[index].clone()),
            Self::SourceMapping(index) => {
                FactNode::SourceMapping(document.source_mappings[index].clone())
            }
            Self::Coverage(index) => FactNode::Coverage(document.coverage_records[index].clone()),
            Self::Skipped(index) => FactNode::Skipped(document.skipped_regions[index].clone()),
            Self::Diagnostic(index) => FactNode::Diagnostic(document.diagnostics[index].clone()),
            Self::Extension(index) => FactNode::Extension(document.extensions[index].clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FactIdMap {
    entries: Vec<(FactId, Option<FactId>)>,
}

impl FactIdMap {
    fn new(locations: &[(FactId, FactLocation)]) -> Result<Self, NormalizedRebindError> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(locations.len())
            .map_err(|_| NormalizedRebindError::ResourceLimit)?;
        entries.extend(locations.iter().map(|(id, _)| (*id, None)));
        Ok(Self { entries })
    }

    fn index(&self, id: FactId) -> Result<usize, NormalizedRebindError> {
        self.entries
            .binary_search_by_key(&id, |(candidate, _)| *candidate)
            .map_err(|_| NormalizedRebindError::ExternalFactReference)
    }

    fn get(&self, id: FactId) -> Option<FactId> {
        self.entries
            .binary_search_by_key(&id, |(candidate, _)| *candidate)
            .ok()
            .and_then(|index| self.entries[index].1)
    }

    fn set(&mut self, index: usize, id: FactId) -> Result<(), NormalizedRebindError> {
        let slot = self
            .entries
            .get_mut(index)
            .ok_or(NormalizedRebindError::IdentityRecipe)?;
        if slot.1.replace(id).is_some() {
            return Err(NormalizedRebindError::IdentityRecipe);
        }
        Ok(())
    }
}

fn fact_locations(
    document: &NormalizedIrDocument,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<Vec<(FactId, FactLocation)>, NormalizedRebindError> {
    let total = usize::try_from(checked_fact_record_count(document)?)
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    let mut locations = Vec::new();
    locations
        .try_reserve_exact(total)
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    type FactCategory = (usize, fn(usize) -> FactLocation);
    let categories: [FactCategory; 8] = [
        (document.occurrences.len(), FactLocation::Occurrence),
        (document.relations.len(), FactLocation::Relation),
        (document.provenance.len(), FactLocation::Provenance),
        (document.source_mappings.len(), FactLocation::SourceMapping),
        (document.coverage_records.len(), FactLocation::Coverage),
        (document.skipped_regions.len(), FactLocation::Skipped),
        (document.diagnostics.len(), FactLocation::Diagnostic),
        (document.extensions.len(), FactLocation::Extension),
    ];
    for (length, constructor) in categories {
        for index in 0..length {
            ensure_projection_continues(checkpoint)?;
            let location = constructor(index);
            locations.push((location.id(document), location));
        }
    }
    ensure_projection_continues(checkpoint)?;
    // Sorting by persisted identity is an in-place bounded step. Checkpoints
    // immediately before and after keep cancellation latency explicit.
    locations.sort_unstable_by_key(|(id, _)| *id);
    ensure_projection_continues(checkpoint)?;
    for pair in locations.windows(2) {
        ensure_projection_continues(checkpoint)?;
        if pair[0].0 == pair[1].0 {
            return Err(NormalizedRebindError::InvalidDocument);
        }
    }
    Ok(locations)
}

fn derive_fact_id_map(
    document: &NormalizedIrDocument,
    generation: GenerationId,
    mut checkpoint: impl FnMut() -> bool,
) -> Result<FactIdMap, NormalizedRebindError> {
    let locations = fact_locations(document, &mut checkpoint)?;
    let mut ids = FactIdMap::new(&locations)?;
    let mut indegrees = Vec::new();
    indegrees
        .try_reserve_exact(locations.len())
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    indegrees.resize(locations.len(), 0_usize);
    let mut edges = Vec::<(usize, usize)>::new();
    let dependency_capacity = usize::try_from(checked_dependency_count(document, &mut checkpoint)?)
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    edges
        .try_reserve_exact(dependency_capacity)
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    for (node_index, (_, location)) in locations.iter().enumerate() {
        ensure_projection_continues(&mut checkpoint)?;
        let dependencies = location.node(document).dependencies(&mut checkpoint)?;
        for dependency in dependencies {
            ensure_projection_continues(&mut checkpoint)?;
            let dependency_index = ids.index(dependency)?;
            edges.push((dependency_index, node_index));
            indegrees[node_index] = indegrees[node_index]
                .checked_add(1)
                .ok_or(NormalizedRebindError::ResourceLimit)?;
        }
    }
    ensure_projection_continues(&mut checkpoint)?;
    // Edge sorting is in-place over the pre-admitted compact graph buffer.
    edges.sort_unstable();
    ensure_projection_continues(&mut checkpoint)?;

    let mut ready = Vec::new();
    ready
        .try_reserve_exact(locations.len())
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    for (index, indegree) in indegrees.iter().enumerate() {
        ensure_projection_continues(&mut checkpoint)?;
        if *indegree == 0 {
            ready.push(index);
        }
    }
    let mut processed = 0_usize;
    while let Some(node_index) = ready.pop() {
        ensure_projection_continues(&mut checkpoint)?;
        let location = locations
            .get(node_index)
            .map(|(_, location)| *location)
            .ok_or(NormalizedRebindError::IdentityRecipe)?;
        let mut scratch = NormalizedIrDocument::empty(document.repository, generation);
        let new_id = rebind_node(
            location.node(document),
            document.repository,
            generation,
            &ids,
            &mut scratch,
            &mut checkpoint,
        )?;
        ids.set(node_index, new_id)?;
        processed = processed
            .checked_add(1)
            .ok_or(NormalizedRebindError::ResourceLimit)?;
        let start = edges.partition_point(|(dependency, _)| *dependency < node_index);
        let end = edges.partition_point(|(dependency, _)| *dependency <= node_index);
        for (_, dependent) in &edges[start..end] {
            ensure_projection_continues(&mut checkpoint)?;
            let indegree = indegrees
                .get_mut(*dependent)
                .ok_or(NormalizedRebindError::IdentityRecipe)?;
            *indegree = indegree
                .checked_sub(1)
                .ok_or(NormalizedRebindError::IdentityRecipe)?;
            if *indegree == 0 {
                ready.push(*dependent);
            }
        }
    }
    if processed != locations.len() {
        return Err(NormalizedRebindError::CyclicFactReferences);
    }
    Ok(ids)
}

fn rebind_document(
    document: NormalizedIrDocument,
    generation: GenerationId,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<NormalizedIrDocument, NormalizedRebindError> {
    rebind_document_with_map(document, generation, limits, extensions).map(|(document, _)| document)
}

fn rebind_document_with_map(
    document: NormalizedIrDocument,
    generation: GenerationId,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<(NormalizedIrDocument, FactIdMap), NormalizedRebindError> {
    require_supported_extensions(&document)?;
    let mut rebound = NormalizedIrDocument::empty(document.repository, generation);
    let mut checkpoint = || true;
    let ids = derive_fact_id_map(&document, generation, &mut checkpoint)?;
    for (_, location) in fact_locations(&document, &mut checkpoint)? {
        rebind_node(
            location.node(&document),
            document.repository,
            generation,
            &ids,
            &mut rebound,
            &mut checkpoint,
        )?;
    }

    rebound
        .files
        .try_reserve_exact(document.files.len())
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    for record in document.files {
        rebound
            .files
            .push(rebind_file(record, generation, &ids, &mut checkpoint)?);
    }
    rebound
        .entities
        .try_reserve_exact(document.entities.len())
        .map_err(|_| NormalizedRebindError::ResourceLimit)?;
    for record in document.entities {
        rebound
            .entities
            .push(rebind_entity(record, generation, &ids, &mut checkpoint)?);
    }
    let rebound = canonical_preserving_extensions(rebound, limits, extensions)?;
    Ok((rebound, ids))
}

fn rebind_node(
    node: FactNode,
    repository: rootlight_ids::RepositoryId,
    generation: GenerationId,
    ids: &FactIdMap,
    target: &mut NormalizedIrDocument,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<FactId, NormalizedRebindError> {
    ensure_projection_continues(checkpoint)?;
    match node {
        FactNode::Occurrence(mut record) => {
            record.generation = generation;
            record.source = rebind_source(record.source, generation);
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
            record.id = derive_occurrence_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.occurrences.push(record);
            Ok(id)
        }
        FactNode::Relation(mut record) => {
            record.generation = generation;
            record.subject = rebind_endpoint(record.subject, ids)?;
            record.object = rebind_endpoint(record.object, ids)?;
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
            record.id = derive_relation_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.relations.push(record);
            Ok(id)
        }
        FactNode::Provenance(mut record) => {
            record.generation = generation;
            for source in &mut record.input_sources {
                ensure_projection_continues(checkpoint)?;
                *source = rebind_source(source.clone(), generation);
            }
            for source in &mut record.evidence_sources {
                ensure_projection_continues(checkpoint)?;
                *source = rebind_source(source.clone(), generation);
            }
            for reference in &mut record.derivation_parents {
                ensure_projection_continues(checkpoint)?;
                *reference = rebind_fact_ref(*reference, ids)?;
            }
            ensure_projection_continues(checkpoint)?;
            record.derivation_parents.sort_unstable();
            ensure_projection_continues(checkpoint)?;
            record.derivation_parents.dedup();
            record.id = derive_provenance_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.provenance.push(record);
            Ok(id)
        }
        FactNode::SourceMapping(mut record) => {
            record.generation = generation;
            record.from = rebind_source(record.from, generation);
            record.to = rebind_source(record.to, generation);
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
            record.id = derive_source_mapping_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.source_mappings.push(record);
            Ok(id)
        }
        FactNode::Coverage(mut record) => {
            record.generation = generation;
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
            record.id = derive_coverage_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.coverage_records.push(record);
            Ok(id)
        }
        FactNode::Skipped(mut record) => {
            record.generation = generation;
            record.source = rebind_source(record.source, generation);
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
            record.id = derive_skipped_region_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.skipped_regions.push(record);
            Ok(id)
        }
        FactNode::Diagnostic(mut record) => {
            record.generation = generation;
            record.source = record
                .source
                .map(|source| rebind_source(source, generation));
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
            record.id = derive_diagnostic_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.diagnostics.push(record);
            Ok(id)
        }
        FactNode::Extension(record) => {
            ensure_projection_continues(checkpoint)?;
            let extension = rebind_extension(record, repository, generation, ids)?;
            ensure_projection_continues(checkpoint)?;
            let id = extension.id;
            target.extensions.push(extension);
            Ok(id)
        }
    }
}

fn hash_rebound_fact(
    location: FactLocation,
    document: &NormalizedIrDocument,
    ids: &FactIdMap,
    hashers: &mut LogicalProjectionHashers,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), NormalizedRebindError> {
    ensure_projection_continues(checkpoint)?;
    let mut scratch = NormalizedIrDocument::empty(document.repository, GENERATION_NEUTRAL_ID);
    let id = rebind_node(
        location.node(document),
        document.repository,
        GENERATION_NEUTRAL_ID,
        ids,
        &mut scratch,
        checkpoint,
    )?;
    ensure_projection_continues(checkpoint)?;
    match location {
        FactLocation::Occurrence(_) => hashers.complete.record(&scratch.occurrences[0])?,
        FactLocation::Relation(_) => hashers.complete.record(&scratch.relations[0])?,
        FactLocation::Provenance(_) => {
            hashers.complete.record(&scratch.provenance[0])?;
            ensure_projection_continues(checkpoint)?;
            hashers.provenance.record(&scratch.provenance[0])?;
        }
        FactLocation::SourceMapping(_) => hashers.complete.record(&scratch.source_mappings[0])?,
        FactLocation::Coverage(_) => {
            hashers.complete.record(&scratch.coverage_records[0])?;
            ensure_projection_continues(checkpoint)?;
            hashers.coverage.record(&scratch.coverage_records[0])?;
        }
        FactLocation::Skipped(_) => {
            hashers.complete.record(&scratch.skipped_regions[0])?;
            ensure_projection_continues(checkpoint)?;
            hashers.coverage.record(&scratch.skipped_regions[0])?;
        }
        FactLocation::Diagnostic(_) => {
            hashers.complete.record(&scratch.diagnostics[0])?;
            ensure_projection_continues(checkpoint)?;
            hashers.coverage.record(&scratch.diagnostics[0])?;
        }
        FactLocation::Extension(_) => hashers.complete.record(&scratch.extensions[0])?,
    }
    ensure_projection_continues(checkpoint)?;
    hashers.stable.record(&id)
}

fn checked_record_count(document: &NormalizedIrDocument) -> Result<u64, NormalizedRebindError> {
    [
        document.files.len(),
        document.entities.len(),
        document.occurrences.len(),
        document.relations.len(),
        document.provenance.len(),
        document.source_mappings.len(),
        document.coverage_records.len(),
        document.skipped_regions.len(),
        document.diagnostics.len(),
        document.extensions.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, length| {
        total
            .checked_add(length)
            .ok_or(NormalizedRebindError::ResourceLimit)
    })
    .and_then(|records| u64::try_from(records).map_err(|_| NormalizedRebindError::ResourceLimit))
}

fn checked_fact_record_count(
    document: &NormalizedIrDocument,
) -> Result<u64, NormalizedRebindError> {
    [
        document.occurrences.len(),
        document.relations.len(),
        document.provenance.len(),
        document.source_mappings.len(),
        document.coverage_records.len(),
        document.skipped_regions.len(),
        document.diagnostics.len(),
        document.extensions.len(),
    ]
    .into_iter()
    .try_fold(0_usize, |total, length| {
        total
            .checked_add(length)
            .ok_or(NormalizedRebindError::ResourceLimit)
    })
    .and_then(|records| u64::try_from(records).map_err(|_| NormalizedRebindError::ResourceLimit))
}

fn checked_dependency_count(
    document: &NormalizedIrDocument,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<u64, NormalizedRebindError> {
    let mut dependencies = 0_usize;
    let mut add = |fixed: usize, derivations: usize| -> Result<(), NormalizedRebindError> {
        dependencies = dependencies
            .checked_add(fixed)
            .and_then(|total| total.checked_add(derivations))
            .ok_or(NormalizedRebindError::ResourceLimit)?;
        Ok(())
    };
    for record in &document.occurrences {
        ensure_projection_continues(checkpoint)?;
        add(1, record.evidence.derivation.len())?;
    }
    for record in &document.relations {
        ensure_projection_continues(checkpoint)?;
        add(3, record.evidence.derivation.len())?;
    }
    for record in &document.provenance {
        ensure_projection_continues(checkpoint)?;
        add(0, record.derivation_parents.len())?;
    }
    for record in &document.source_mappings {
        ensure_projection_continues(checkpoint)?;
        add(1, record.evidence.derivation.len())?;
    }
    for record in &document.coverage_records {
        ensure_projection_continues(checkpoint)?;
        add(1, record.evidence.derivation.len())?;
    }
    for record in &document.skipped_regions {
        ensure_projection_continues(checkpoint)?;
        add(1, record.evidence.derivation.len())?;
    }
    for record in &document.diagnostics {
        ensure_projection_continues(checkpoint)?;
        add(1, record.evidence.derivation.len())?;
    }
    for record in &document.extensions {
        ensure_projection_continues(checkpoint)?;
        add(2, record.evidence.derivation.len())?;
    }
    ensure_projection_continues(checkpoint)?;
    u64::try_from(dependencies).map_err(|_| NormalizedRebindError::ResourceLimit)
}

fn rebind_extension(
    extension: ExtensionEnvelope,
    repository: rootlight_ids::RepositoryId,
    generation: GenerationId,
    ids: &FactIdMap,
) -> Result<ExtensionEnvelope, NormalizedRebindError> {
    let provenance = remap_id(extension.provenance, ids)?;
    let source = extension
        .evidence
        .source
        .clone()
        .map(|source| rebind_source(source, generation))
        .ok_or(NormalizedRebindError::IdentityRecipe)?;
    match extension.namespace.as_str() {
        FILE_IDENTITY_CLAIM_NAMESPACE => {
            let claim = decode_file_identity_claim_envelope(&extension)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            new_file_identity_claim_envelope(&claim, generation, provenance, source)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)
        }
        SYMBOL_IDENTITY_CLAIM_NAMESPACE => {
            let claim = decode_symbol_identity_claim_envelope(&extension)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            new_symbol_identity_claim_envelope(&claim, generation, provenance, source)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)
        }
        LEXICAL_EXTENSION_NAMESPACE => {
            let evidence = decode_lexical_evidence_envelope(&extension)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let subject = rebind_fact_ref(evidence.subject(), ids)?;
            let evidence = rebind_lexical_evidence_subject(&evidence, subject)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            new_lexical_evidence_envelope(repository, generation, provenance, source, &evidence)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)
        }
        _ => Err(NormalizedRebindError::UnsupportedExtension),
    }
}

fn rebind_file(
    mut record: FileRecord,
    generation: GenerationId,
    ids: &FactIdMap,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<FileRecord, NormalizedRebindError> {
    record.generation = generation;
    record.provenance = remap_id(record.provenance, ids)?;
    record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
    Ok(record)
}

fn rebind_entity(
    mut record: crate::EntityRecord,
    generation: GenerationId,
    ids: &FactIdMap,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<crate::EntityRecord, NormalizedRebindError> {
    record.generation = generation;
    record.provenance = remap_id(record.provenance, ids)?;
    record.evidence = rebind_evidence(record.evidence, generation, ids, checkpoint)?;
    Ok(record)
}

fn rebind_source(source: SourceRef, generation: GenerationId) -> SourceRef {
    SourceRef::new(
        source.repository(),
        generation,
        source.span(),
        source.content_hash(),
        source.line_hint(),
    )
}

fn rebind_evidence(
    mut evidence: FactEvidence,
    generation: GenerationId,
    ids: &FactIdMap,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<FactEvidence, NormalizedRebindError> {
    evidence.source = evidence
        .source
        .map(|source| rebind_source(source, generation));
    for reference in &mut evidence.derivation {
        ensure_projection_continues(checkpoint)?;
        *reference = rebind_fact_ref(*reference, ids)?;
    }
    ensure_projection_continues(checkpoint)?;
    evidence.derivation.sort_unstable();
    ensure_projection_continues(checkpoint)?;
    evidence.derivation.dedup();
    Ok(evidence)
}

fn rebind_fact_ref(reference: FactRef, ids: &FactIdMap) -> Result<FactRef, NormalizedRebindError> {
    match reference {
        FactRef::File(file) => Ok(FactRef::File(file)),
        FactRef::Entity(entity) => Ok(FactRef::Entity(entity)),
        FactRef::Fact(id) => remap_id(id, ids).map(FactRef::Fact),
    }
}

fn rebind_endpoint(
    endpoint: RelationEndpoint,
    ids: &FactIdMap,
) -> Result<RelationEndpoint, NormalizedRebindError> {
    match endpoint {
        RelationEndpoint::Occurrence(id) => remap_id(id, ids).map(RelationEndpoint::Occurrence),
        stable => Ok(stable),
    }
}

fn remap_id(id: FactId, ids: &FactIdMap) -> Result<FactId, NormalizedRebindError> {
    ids.get(id)
        .ok_or(NormalizedRebindError::ExternalFactReference)
}

fn collect_evidence_dependencies(
    evidence: &FactEvidence,
    target: &mut Vec<FactId>,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), NormalizedRebindError> {
    collect_fact_ref_dependencies(&evidence.derivation, target, checkpoint)
}

fn collect_fact_ref_dependencies(
    references: &[FactRef],
    target: &mut Vec<FactId>,
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), NormalizedRebindError> {
    for reference in references {
        ensure_projection_continues(checkpoint)?;
        collect_fact_ref_dependency(*reference, target);
    }
    Ok(())
}

fn collect_fact_ref_dependency(reference: FactRef, target: &mut Vec<FactId>) {
    if let FactRef::Fact(id) = reference {
        target.push(id);
    }
}

fn collect_endpoint_dependency(endpoint: RelationEndpoint, target: &mut Vec<FactId>) {
    if let RelationEndpoint::Occurrence(id) = endpoint {
        target.push(id);
    }
}

fn ensure_projection_continues(
    checkpoint: &mut impl FnMut() -> bool,
) -> Result<(), NormalizedRebindError> {
    if checkpoint() {
        Ok(())
    } else {
        Err(NormalizedRebindError::Interrupted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ContainerRef, EntityKind, FileIdentityClaim, IrDocument, LEXICAL_EXTENSION_NAMESPACE,
        LexicalEvidenceFormat, LexicalEvidenceKind, LexicalEvidenceV1, SymbolIdentityClaim,
        validate_ir_document,
    };

    fn fixture_with_lexical_extension() -> NormalizedIrDocument {
        let decoded = crate::decode_ir_document(
            include_bytes!("../../../tests/fixtures/compatibility/ir/1.1/document.json"),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("normalized fixture decodes");
        let IrDocument::NormalizedV1_1(mut document) = decoded else {
            panic!("fixture uses normalized IR");
        };
        document.extensions.clear();
        let occurrence = document
            .occurrences
            .first()
            .expect("fixture contains an occurrence");
        let evidence = LexicalEvidenceV1::from_complete_text(
            LexicalEvidenceKind::Signature,
            FactRef::Fact(occurrence.id),
            LexicalEvidenceFormat::SourceText,
            "fn fixture(value: usize) -> usize",
        )
        .expect("lexical evidence builds");
        let source = occurrence.source.clone();
        document.extensions.push(
            new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                occurrence.provenance,
                source,
                &evidence,
            )
            .expect("lexical envelope builds"),
        );
        canonical_preserving_extensions(
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("fixture canonicalizes")
    }

    fn fact_ids(document: &NormalizedIrDocument) -> BTreeSet<FactId> {
        document
            .occurrences
            .iter()
            .map(|record| record.id)
            .chain(document.relations.iter().map(|record| record.id))
            .chain(document.provenance.iter().map(|record| record.id))
            .chain(document.source_mappings.iter().map(|record| record.id))
            .chain(document.coverage_records.iter().map(|record| record.id))
            .chain(document.skipped_regions.iter().map(|record| record.id))
            .chain(document.diagnostics.iter().map(|record| record.id))
            .chain(document.extensions.iter().map(|record| record.id))
            .collect()
    }

    fn fixture_with_identity_extensions()
    -> (NormalizedIrDocument, FileIdentityClaim, SymbolIdentityClaim) {
        let fixture = fixture_with_lexical_extension();
        let repository = fixture.repository;
        let generation = fixture.generation;
        let template_file = fixture.files[0].clone();
        let mut file_claim = FileIdentityClaim {
            file: rootlight_ids::FileId::from_bytes([0; 20]),
            repository,
            path: "src/identity.rs".to_owned(),
            path_identity: b"src/identity.rs".to_vec(),
            content_hash: template_file.content_hash,
            byte_length: template_file.byte_length,
        };
        file_claim.file = file_claim.derived_file();
        let source = SourceRef::new(
            repository,
            generation,
            crate::SourceSpan::new(file_claim.file, 0, file_claim.byte_length)
                .expect("identity source span builds"),
            file_claim.content_hash,
            None,
        );
        let mut provenance = fixture.provenance[0].clone();
        provenance.generation = generation;
        provenance.input_sources = vec![source.clone()];
        provenance.evidence_sources = vec![source.clone()];
        provenance.derivation_parents.clear();
        provenance.id =
            derive_provenance_record_id(&provenance).expect("identity provenance derives");

        let mut file = template_file;
        file.id = file_claim.file;
        file.path = file_claim.path.clone();
        file.provenance = provenance.id;
        file.evidence = FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        };
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.provenance.push(provenance);
        document.files.push(file);
        document.extensions.push(
            new_file_identity_claim_envelope(
                &file_claim,
                generation,
                document.provenance[0].id,
                source.clone(),
            )
            .expect("file identity envelope builds"),
        );

        let mut container_identity = vec![0];
        container_identity.extend_from_slice(file_claim.file.as_bytes());
        let symbol_claim = SymbolIdentityClaim {
            symbol: rootlight_ids::SymbolId::from_bytes([0; 20]),
            repository,
            language: "rust".to_owned(),
            kind: EntityKind::Function,
            container: Some(ContainerRef::File(file_claim.file)),
            container_identity,
            declared_identity: "rebound_fixture".to_owned(),
            signature_discriminator: b"fn rebound_fixture()".to_vec(),
            build_context_discriminator: b"rebind-test-context".to_vec(),
        };
        let symbol_claim = SymbolIdentityClaim {
            symbol: symbol_claim.derived_symbol(),
            ..symbol_claim
        };
        let mut entity = fixture.entities[0].clone();
        entity.id = symbol_claim.symbol;
        entity.generation = generation;
        entity.canonical_name = symbol_claim.declared_identity.clone();
        entity.display_name = symbol_claim.declared_identity.clone();
        entity.qualified_name = format!("crate::{}", symbol_claim.declared_identity);
        entity.container = symbol_claim.container;
        entity.provenance = document.provenance[0].id;
        entity.evidence = FactEvidence {
            source: Some(source.clone()),
            derivation: Vec::new(),
        };
        document.entities.push(entity);
        document.extensions.push(
            new_symbol_identity_claim_envelope(
                &symbol_claim,
                generation,
                document.provenance[0].id,
                source,
            )
            .expect("symbol identity envelope builds"),
        );

        let document = canonical_preserving_extensions(
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("identity fixture canonicalizes");
        (document, file_claim, symbol_claim)
    }

    fn assert_evidence_rebound(
        evidence: &FactEvidence,
        generation: GenerationId,
        current_ids: &BTreeSet<FactId>,
        previous_ids: &BTreeSet<FactId>,
    ) {
        if let Some(source) = &evidence.source {
            assert_eq!(source.generation(), generation);
        }
        for reference in &evidence.derivation {
            if let FactRef::Fact(id) = reference {
                assert!(current_ids.contains(id));
                assert!(!previous_ids.contains(id));
            }
        }
    }

    #[test]
    fn chunk_digest_is_generation_independent() {
        let first = fixture_with_lexical_extension();
        let first_chunk = CanonicalNormalizedFileChunk::new(
            &first,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("first chunk builds");
        let second_generation = GenerationId::from_bytes([91; 20]);
        let second = first_chunk
            .rebind(
                second_generation,
                &IrLimits::default(),
                &ExtensionSupport::default(),
            )
            .expect("chunk rebinds");
        let second_chunk = CanonicalNormalizedFileChunk::new(
            &second,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("successor chunk builds");

        assert_ne!(first.generation, second.generation);
        assert_eq!(first_chunk.file(), second_chunk.file());
        assert_eq!(first_chunk.digest(), second_chunk.digest());
        assert_eq!(first_chunk.encoded_bytes(), second_chunk.encoded_bytes());
    }

    #[test]
    fn streamed_logical_digests_are_generation_independent_across_fact_order_flip() {
        let (original, _, _) = fixture_with_identity_extensions();
        let original_digests = canonical_generation_neutral_digests(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .expect("original logical digests build");
        let chunk = CanonicalNormalizedFileChunk::new(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("fixture chunk builds");
        let mut flipped = None;
        for discriminator in 1..=u8::MAX {
            let candidate = chunk
                .rebind(
                    GenerationId::from_bytes([discriminator; 20]),
                    &IrLimits::default(),
                    &ExtensionSupport::default(),
                )
                .expect("candidate generation rebinds");
            let neutral = CanonicalGenerationNeutralDocument::new(
                &candidate,
                &IrLimits::default(),
                &ExtensionSupport::default(),
            )
            .expect("candidate neutralizes");
            let neutral_ids = candidate
                .extensions
                .iter()
                .map(|record| {
                    neutral
                        .rebind_fact_id(record.id)
                        .expect("extension identity is mapped")
                })
                .collect::<Vec<_>>();
            if !neutral_ids.windows(2).all(|pair| pair[0] < pair[1]) {
                flipped = Some(candidate);
                break;
            }
        }
        let flipped = flipped.expect("a generation-bound fact order flip is found");
        let flipped_digests = canonical_generation_neutral_digests(
            &flipped,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .expect("flipped logical digests build");

        assert_eq!(flipped_digests, original_digests);
        assert_eq!(
            original_digests.records(),
            u64::try_from(
                fact_ids(&original).len() + original.files.len() + original.entities.len()
            )
            .expect("record count fits")
        );
    }

    #[test]
    fn streamed_logical_digests_bind_semantics_and_fail_closed() {
        let (original, _, _) = fixture_with_identity_extensions();
        let original_digests = canonical_generation_neutral_digests(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .expect("original logical digests build");

        let mut semantic_edit = original.clone();
        semantic_edit.files[0].language = "go".to_owned();
        let semantic_digests = canonical_generation_neutral_digests(
            &semantic_edit,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .expect("semantic edit remains valid");
        assert_ne!(semantic_digests.complete(), original_digests.complete());
        assert_eq!(
            semantic_digests.stable_identities(),
            original_digests.stable_identities()
        );

        let mut noncanonical = original.clone();
        noncanonical.entities[0].flags =
            vec![crate::EntityFlag::Test, crate::EntityFlag::Generated];
        assert!(matches!(
            canonical_generation_neutral_digests(
                &noncanonical,
                &IrLimits::default(),
                &ExtensionSupport::default(),
                || true
            ),
            Err(NormalizedRebindError::InvalidDocument)
        ));

        let mut opaque = original.clone();
        opaque.extensions[0].namespace = "dev.rootlight.opaque".to_owned();
        let preservation = ExtensionSupport {
            unknown_noncritical: UnknownNoncriticalExtensionPolicy::Preserve,
            ..ExtensionSupport::default()
        };
        assert!(matches!(
            canonical_generation_neutral_digests(
                &opaque,
                &IrLimits::default(),
                &preservation,
                || true
            ),
            Err(NormalizedRebindError::UnsupportedExtension)
        ));
        assert!(matches!(
            canonical_generation_neutral_digests(
                &original,
                &IrLimits::default(),
                &ExtensionSupport::default(),
                || false
            ),
            Err(NormalizedRebindError::Interrupted)
        ));
        let mut workspace_checkpoints = 0_u8;
        assert!(matches!(
            generation_neutral_workspace_bytes(&original, || {
                workspace_checkpoints = workspace_checkpoints.saturating_add(1);
                workspace_checkpoints < 2
            }),
            Err(NormalizedRebindError::Interrupted)
        ));
        assert_eq!(workspace_checkpoints, 2);
    }

    #[test]
    fn streamed_logical_digests_match_the_canonical_materialized_projection() {
        let (original, _, _) = fixture_with_identity_extensions();
        let streamed = canonical_generation_neutral_digests(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .expect("streamed logical digests build");
        let materialized = CanonicalGenerationNeutralDocument::new(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("canonical neutral document builds");
        let materialized_digests = canonical_generation_neutral_digests(
            &materialized.document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            || true,
        )
        .expect("materialized logical digests build");
        assert_eq!(streamed, materialized_digests);

        let proof = CanonicalNormalizedIrDocument::new(
            original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("canonical proof builds");
        assert_eq!(
            proof
                .generation_neutral_digests(|| true)
                .expect("proof-bound logical digests build"),
            streamed
        );
        assert!(matches!(
            proof.generation_neutral_digests(|| false),
            Err(NormalizedRebindError::Interrupted)
        ));
    }

    #[test]
    fn rebind_rewrites_complete_fact_graph_and_builtin_lexical_envelope() {
        let original = fixture_with_lexical_extension();
        let old_ids = fact_ids(&original);
        let generation = GenerationId::from_bytes([92; 20]);
        let rebound = CanonicalNormalizedFileChunk::new(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("chunk builds")
        .rebind(
            generation,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("chunk rebinds");

        validate_ir_document(&rebound, &IrLimits::default(), &ExtensionSupport::default())
            .expect("rebound document remains valid");
        assert_eq!(rebound.generation, generation);
        assert!(
            rebound
                .files
                .iter()
                .all(|record| record.generation == generation)
        );
        assert!(
            rebound
                .entities
                .iter()
                .all(|record| record.generation == generation)
        );
        assert!(
            fact_ids(&rebound).iter().all(|id| !old_ids.contains(id)),
            "every generation-derived fact identity must change"
        );
        let current_ids = fact_ids(&rebound);
        for evidence in rebound
            .files
            .iter()
            .map(|record| &record.evidence)
            .chain(rebound.entities.iter().map(|record| &record.evidence))
            .chain(rebound.occurrences.iter().map(|record| &record.evidence))
            .chain(rebound.relations.iter().map(|record| &record.evidence))
            .chain(
                rebound
                    .source_mappings
                    .iter()
                    .map(|record| &record.evidence),
            )
            .chain(
                rebound
                    .coverage_records
                    .iter()
                    .map(|record| &record.evidence),
            )
            .chain(
                rebound
                    .skipped_regions
                    .iter()
                    .map(|record| &record.evidence),
            )
            .chain(rebound.diagnostics.iter().map(|record| &record.evidence))
            .chain(rebound.extensions.iter().map(|record| &record.evidence))
        {
            assert_evidence_rebound(evidence, generation, &current_ids, &old_ids);
        }
        for provenance in &rebound.provenance {
            for reference in &provenance.derivation_parents {
                if let FactRef::Fact(id) = reference {
                    assert!(current_ids.contains(id));
                    assert!(!old_ids.contains(id));
                }
            }
        }
        let occurrence_ids = rebound
            .occurrences
            .iter()
            .map(|record| record.id)
            .collect::<BTreeSet<_>>();
        assert!(rebound.relations.iter().all(|relation| {
            [relation.subject, relation.object]
                .into_iter()
                .filter_map(|endpoint| match endpoint {
                    RelationEndpoint::Occurrence(id) => Some(id),
                    _ => None,
                })
                .all(|id| occurrence_ids.contains(&id))
        }));
        let lexical = rebound
            .extensions
            .iter()
            .find(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
            .expect("lexical extension survives");
        let lexical_evidence =
            decode_lexical_evidence_envelope(lexical).expect("rebound lexical envelope validates");
        let FactRef::Fact(lexical_subject) = lexical_evidence.subject() else {
            panic!("lexical subject remains fact-bound");
        };
        assert!(occurrence_ids.contains(&lexical_subject));
        assert!(!old_ids.contains(&lexical_subject));
        assert_eq!(lexical.generation, generation);
        assert_eq!(
            lexical
                .evidence
                .source
                .as_ref()
                .expect("lexical source survives")
                .generation(),
            generation
        );
        assert!(rebound.provenance.iter().all(|record| {
            record
                .input_sources
                .iter()
                .chain(&record.evidence_sources)
                .all(|source| source.generation() == generation)
        }));
    }

    #[test]
    fn rebind_reconstructs_file_and_symbol_identity_envelopes() {
        let (original, expected_file_claim, expected_symbol_claim) =
            fixture_with_identity_extensions();
        let original_file = original
            .extensions
            .iter()
            .find(|extension| extension.namespace == FILE_IDENTITY_CLAIM_NAMESPACE)
            .expect("file identity envelope exists");
        let original_symbol = original
            .extensions
            .iter()
            .find(|extension| extension.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
            .expect("symbol identity envelope exists");
        let generation = GenerationId::from_bytes([93; 20]);
        let rebound = CanonicalNormalizedFileChunk::new(
            &original,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("identity chunk builds")
        .rebind(
            generation,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("identity chunk rebinds");

        let rebound_file = rebound
            .extensions
            .iter()
            .find(|extension| extension.namespace == FILE_IDENTITY_CLAIM_NAMESPACE)
            .expect("file identity envelope survives");
        let rebound_symbol = rebound
            .extensions
            .iter()
            .find(|extension| extension.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
            .expect("symbol identity envelope survives");
        assert_eq!(
            decode_file_identity_claim_envelope(rebound_file)
                .expect("rebound file identity envelope decodes"),
            expected_file_claim
        );
        assert_eq!(
            decode_symbol_identity_claim_envelope(rebound_symbol)
                .expect("rebound symbol identity envelope decodes"),
            expected_symbol_claim
        );
        for (previous, current) in [
            (original_file, rebound_file),
            (original_symbol, rebound_symbol),
        ] {
            assert_ne!(current.id, previous.id);
            assert_ne!(current.provenance, previous.provenance);
            assert_eq!(current.generation, generation);
            let source = current
                .evidence
                .source
                .as_ref()
                .expect("identity envelope retains direct source");
            assert_eq!(source.generation(), generation);
            assert!(
                rebound
                    .provenance
                    .iter()
                    .any(|record| record.id == current.provenance)
            );
        }
        assert_eq!(
            decode_file_identity_claim_envelope(rebound_file)
                .expect("file claim remains stable")
                .file,
            expected_file_claim.file
        );
        assert_eq!(
            decode_symbol_identity_claim_envelope(rebound_symbol)
                .expect("symbol claim remains stable")
                .symbol,
            expected_symbol_claim.symbol
        );
    }

    #[test]
    fn cycles_and_opaque_extensions_refuse_chunk_reuse() {
        let mut opaque = fixture_with_lexical_extension();
        opaque.extensions[0].namespace = "dev.rootlight.opaque".to_owned();
        assert!(matches!(
            CanonicalNormalizedFileChunk::new(
                &opaque,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(NormalizedRebindError::UnsupportedExtension)
        ));

        let mut cyclic = fixture_with_lexical_extension();
        let occurrence = cyclic.occurrences[0].id;
        let relation = cyclic.relations[0].id;
        cyclic.occurrences[0]
            .evidence
            .derivation
            .push(FactRef::Fact(relation));
        cyclic.relations[0]
            .evidence
            .derivation
            .push(FactRef::Fact(occurrence));
        assert!(matches!(
            CanonicalNormalizedFileChunk::new(
                &cyclic,
                &IrLimits::default(),
                &ExtensionSupport::default()
            ),
            Err(NormalizedRebindError::CyclicFactReferences)
        ));
    }
}
