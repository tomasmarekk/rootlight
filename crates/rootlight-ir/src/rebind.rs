//! Generation-neutral normalized chunks and bounded generation rebinding.
//!
//! Canonical chunks are process-local reuse artifacts. They never change the
//! durable IR format and accept only extension namespaces with identity recipes
//! that this module can completely reconstruct.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_ids::{ContentHash, FactId, FileId, GenerationId};

use crate::lexical::rebind_lexical_evidence_subject;
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
        require_supported_extensions(document)?;
        let canonical = canonical_preserving_extensions(document.clone(), limits, extensions)?;
        let [file] = canonical.files.as_slice() else {
            return Err(NormalizedRebindError::NotSingleFile);
        };
        let file = file.id;
        let document = rebind_document(canonical, GENERATION_NEUTRAL_ID, limits, extensions)?;
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
}

impl From<IrDocumentValidationError> for NormalizedRebindError {
    fn from(_: IrDocumentValidationError) -> Self {
        Self::InvalidDocument
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
    const fn id(&self) -> FactId {
        match self {
            Self::Occurrence(record) => record.id,
            Self::Relation(record) => record.id,
            Self::Provenance(record) => record.id,
            Self::SourceMapping(record) => record.id,
            Self::Coverage(record) => record.id,
            Self::Skipped(record) => record.id,
            Self::Diagnostic(record) => record.id,
            Self::Extension(record) => record.id,
        }
    }

    fn dependencies(&self) -> Result<BTreeSet<FactId>, NormalizedRebindError> {
        let mut dependencies = BTreeSet::new();
        match self {
            Self::Occurrence(record) => {
                dependencies.insert(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
            }
            Self::Relation(record) => {
                dependencies.insert(record.provenance);
                collect_endpoint_dependency(record.subject, &mut dependencies);
                collect_endpoint_dependency(record.object, &mut dependencies);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
            }
            Self::Provenance(record) => {
                collect_fact_ref_dependencies(&record.derivation_parents, &mut dependencies);
            }
            Self::SourceMapping(record) => {
                dependencies.insert(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
            }
            Self::Coverage(record) => {
                dependencies.insert(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
            }
            Self::Skipped(record) => {
                dependencies.insert(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
            }
            Self::Diagnostic(record) => {
                dependencies.insert(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
            }
            Self::Extension(record) => {
                dependencies.insert(record.provenance);
                collect_evidence_dependencies(&record.evidence, &mut dependencies);
                if record.namespace == LEXICAL_EXTENSION_NAMESPACE {
                    let evidence = decode_lexical_evidence_envelope(record)
                        .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
                    collect_fact_ref_dependency(evidence.subject(), &mut dependencies);
                }
            }
        }
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
    if document.extensions.iter().all(|extension| {
        matches!(
            extension.namespace.as_str(),
            FILE_IDENTITY_CLAIM_NAMESPACE
                | SYMBOL_IDENTITY_CLAIM_NAMESPACE
                | LEXICAL_EXTENSION_NAMESPACE
        )
    }) {
        Ok(())
    } else {
        Err(NormalizedRebindError::UnsupportedExtension)
    }
}

fn rebind_document(
    document: NormalizedIrDocument,
    generation: GenerationId,
    limits: &IrLimits,
    extensions: &ExtensionSupport,
) -> Result<NormalizedIrDocument, NormalizedRebindError> {
    require_supported_extensions(&document)?;
    let mut nodes = BTreeMap::new();
    for node in document
        .occurrences
        .iter()
        .cloned()
        .map(FactNode::Occurrence)
        .chain(document.relations.iter().cloned().map(FactNode::Relation))
        .chain(
            document
                .provenance
                .iter()
                .cloned()
                .map(FactNode::Provenance),
        )
        .chain(
            document
                .source_mappings
                .iter()
                .cloned()
                .map(FactNode::SourceMapping),
        )
        .chain(
            document
                .coverage_records
                .iter()
                .cloned()
                .map(FactNode::Coverage),
        )
        .chain(
            document
                .skipped_regions
                .iter()
                .cloned()
                .map(FactNode::Skipped),
        )
        .chain(
            document
                .diagnostics
                .iter()
                .cloned()
                .map(FactNode::Diagnostic),
        )
        .chain(document.extensions.iter().cloned().map(FactNode::Extension))
    {
        if nodes.insert(node.id(), node).is_some() {
            return Err(NormalizedRebindError::InvalidDocument);
        }
    }

    let known_ids = nodes.keys().copied().collect::<BTreeSet<_>>();
    let mut dependents = BTreeMap::<FactId, Vec<FactId>>::new();
    let mut indegrees = BTreeMap::new();
    for (id, node) in &nodes {
        let dependencies = node.dependencies()?;
        if dependencies
            .iter()
            .any(|dependency| !known_ids.contains(dependency))
        {
            return Err(NormalizedRebindError::ExternalFactReference);
        }
        indegrees.insert(*id, dependencies.len());
        for dependency in dependencies {
            dependents.entry(dependency).or_default().push(*id);
        }
    }

    let mut rebound = NormalizedIrDocument::empty(document.repository, generation);
    let mut ids = BTreeMap::new();
    let mut ready = indegrees
        .iter()
        .filter_map(|(id, indegree)| (*indegree == 0).then_some(*id))
        .collect::<BTreeSet<_>>();
    while let Some(old_id) = ready.pop_first() {
        let node = nodes
            .remove(&old_id)
            .ok_or(NormalizedRebindError::IdentityRecipe)?;
        let new_id = rebind_node(node, document.repository, generation, &ids, &mut rebound)?;
        if ids.insert(old_id, new_id).is_some() {
            return Err(NormalizedRebindError::IdentityRecipe);
        }
        for dependent in dependents.remove(&old_id).unwrap_or_default() {
            let indegree = indegrees
                .get_mut(&dependent)
                .ok_or(NormalizedRebindError::IdentityRecipe)?;
            *indegree = indegree
                .checked_sub(1)
                .ok_or(NormalizedRebindError::IdentityRecipe)?;
            if *indegree == 0 {
                ready.insert(dependent);
            }
        }
    }
    if !nodes.is_empty() {
        return Err(NormalizedRebindError::CyclicFactReferences);
    }

    rebound.files = document
        .files
        .into_iter()
        .map(|record| rebind_file(record, generation, &ids))
        .collect::<Result<_, _>>()?;
    rebound.entities = document
        .entities
        .into_iter()
        .map(|mut record| {
            record.generation = generation;
            record.provenance = remap_id(record.provenance, &ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, &ids)?;
            Ok(record)
        })
        .collect::<Result<_, NormalizedRebindError>>()?;
    canonical_preserving_extensions(rebound, limits, extensions)
}

fn rebind_node(
    node: FactNode,
    repository: rootlight_ids::RepositoryId,
    generation: GenerationId,
    ids: &BTreeMap<FactId, FactId>,
    target: &mut NormalizedIrDocument,
) -> Result<FactId, NormalizedRebindError> {
    match node {
        FactNode::Occurrence(mut record) => {
            record.generation = generation;
            record.source = rebind_source(record.source, generation);
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids)?;
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
            record.evidence = rebind_evidence(record.evidence, generation, ids)?;
            record.id = derive_relation_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.relations.push(record);
            Ok(id)
        }
        FactNode::Provenance(mut record) => {
            record.generation = generation;
            record.input_sources = record
                .input_sources
                .into_iter()
                .map(|source| rebind_source(source, generation))
                .collect();
            record.evidence_sources = record
                .evidence_sources
                .into_iter()
                .map(|source| rebind_source(source, generation))
                .collect();
            record.derivation_parents = record
                .derivation_parents
                .into_iter()
                .map(|reference| rebind_fact_ref(reference, ids))
                .collect::<Result<_, _>>()?;
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
            record.evidence = rebind_evidence(record.evidence, generation, ids)?;
            record.id = derive_source_mapping_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.source_mappings.push(record);
            Ok(id)
        }
        FactNode::Coverage(mut record) => {
            record.generation = generation;
            record.provenance = remap_id(record.provenance, ids)?;
            record.evidence = rebind_evidence(record.evidence, generation, ids)?;
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
            record.evidence = rebind_evidence(record.evidence, generation, ids)?;
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
            record.evidence = rebind_evidence(record.evidence, generation, ids)?;
            record.id = derive_diagnostic_record_id(&record)
                .map_err(|_| NormalizedRebindError::IdentityRecipe)?;
            let id = record.id;
            target.diagnostics.push(record);
            Ok(id)
        }
        FactNode::Extension(record) => {
            let extension = rebind_extension(record, repository, generation, ids)?;
            let id = extension.id;
            target.extensions.push(extension);
            Ok(id)
        }
    }
}

fn rebind_extension(
    extension: ExtensionEnvelope,
    repository: rootlight_ids::RepositoryId,
    generation: GenerationId,
    ids: &BTreeMap<FactId, FactId>,
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
    ids: &BTreeMap<FactId, FactId>,
) -> Result<FileRecord, NormalizedRebindError> {
    record.generation = generation;
    record.provenance = remap_id(record.provenance, ids)?;
    record.evidence = rebind_evidence(record.evidence, generation, ids)?;
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
    evidence: FactEvidence,
    generation: GenerationId,
    ids: &BTreeMap<FactId, FactId>,
) -> Result<FactEvidence, NormalizedRebindError> {
    Ok(FactEvidence {
        source: evidence
            .source
            .map(|source| rebind_source(source, generation)),
        derivation: evidence
            .derivation
            .into_iter()
            .map(|reference| rebind_fact_ref(reference, ids))
            .collect::<Result<_, _>>()?,
    })
}

fn rebind_fact_ref(
    reference: FactRef,
    ids: &BTreeMap<FactId, FactId>,
) -> Result<FactRef, NormalizedRebindError> {
    match reference {
        FactRef::File(file) => Ok(FactRef::File(file)),
        FactRef::Entity(entity) => Ok(FactRef::Entity(entity)),
        FactRef::Fact(id) => remap_id(id, ids).map(FactRef::Fact),
    }
}

fn rebind_endpoint(
    endpoint: RelationEndpoint,
    ids: &BTreeMap<FactId, FactId>,
) -> Result<RelationEndpoint, NormalizedRebindError> {
    match endpoint {
        RelationEndpoint::Occurrence(id) => remap_id(id, ids).map(RelationEndpoint::Occurrence),
        stable => Ok(stable),
    }
}

fn remap_id(id: FactId, ids: &BTreeMap<FactId, FactId>) -> Result<FactId, NormalizedRebindError> {
    ids.get(&id)
        .copied()
        .ok_or(NormalizedRebindError::ExternalFactReference)
}

fn collect_evidence_dependencies(evidence: &FactEvidence, target: &mut BTreeSet<FactId>) {
    collect_fact_ref_dependencies(&evidence.derivation, target);
}

fn collect_fact_ref_dependencies(references: &[FactRef], target: &mut BTreeSet<FactId>) {
    for reference in references {
        collect_fact_ref_dependency(*reference, target);
    }
}

fn collect_fact_ref_dependency(reference: FactRef, target: &mut BTreeSet<FactId>) {
    if let FactRef::Fact(id) = reference {
        target.insert(id);
    }
}

fn collect_endpoint_dependency(endpoint: RelationEndpoint, target: &mut BTreeSet<FactId>) {
    if let RelationEndpoint::Occurrence(id) = endpoint {
        target.insert(id);
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
