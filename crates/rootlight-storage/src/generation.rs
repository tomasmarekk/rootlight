//! Logical generation data shared by replaceable persistence backends.
//!
//! Values crossing this boundary own their records. Backend implementations
//! must enforce the supplied row, source-reference, text, and cancellation caps.

use std::{
    collections::{BTreeMap, BTreeSet},
    convert::Infallible,
    error::Error,
    io::{self, BufReader, Read, Write},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    ContentHash, GenerationId, GenerationIdentity, RepositoryId, SymbolId, derive_generation,
};
use rootlight_ir::{
    CanonicalGenerationNeutralDigests, CanonicalNormalizedIrDocument, ExtensionSupport,
    FILE_IDENTITY_CLAIM_NAMESPACE, FactEvidence, FactRef, FileIdentityClaim, FileRecord,
    IdentityClaimError, IrDocumentDecodeError, IrDocumentValidationError, IrLimits, IrVersion,
    LEXICAL_EXTENSION_NAMESPACE, LEXICAL_SUMMARY_BATCH_VERSION, NORMALIZED_IR_VERSION,
    NormalizedIrDocument, NormalizedRebindError, OccurrenceTarget, PERL_BINDING_NAMESPACE,
    RelationEndpoint, SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceRef,
    decode_file_identity_claim_envelope_with_checkpoint, decode_lexical_summary_batch_envelope,
    decode_normalized_ir_document_messagepack_reader_with_checkpoint,
    decode_normalized_ir_document_with_checkpoint, decode_perl_binding_envelope,
    decode_symbol_identity_claim_envelope_with_checkpoint,
    derive_coverage_record_id_with_checkpoint, derive_diagnostic_record_id_with_checkpoint,
    derive_occurrence_record_id_with_checkpoint, derive_provenance_record_id_with_checkpoint,
    derive_relation_record_id_with_checkpoint, derive_skipped_region_id_with_checkpoint,
    derive_source_mapping_record_id_with_checkpoint, validate_lexical_evidence_envelope,
};
use serde::Serialize;

use crate::source_catalog::{SourceFileCatalog, SourceFileCatalogError};

/// Current backend-neutral generation contract version.
pub const GENERATION_CONTRACT_VERSION: GenerationContractVersion =
    GenerationContractVersion::new(1, 2);

/// Version of the generation identity-claim recipe.
pub const PROPOSED_IDENTITY_CLAIM_VERSION: GenerationContractVersion =
    GenerationContractVersion::new(1, 0);

const LEGACY_GENERATION_CONTRACT_VERSION: GenerationContractVersion =
    GenerationContractVersion::new(1, 1);

/// Hard ceiling for physical rows written or materialized by one operation.
pub const HARD_MAX_GENERATION_ROWS: u64 = 50_000_000;
/// Hard ceiling for distinct source references in one generation operation.
pub const HARD_MAX_GENERATION_SOURCE_REFS: u64 = 20_000_000;
/// Hard ceiling for owned dynamic text crossing one generation operation.
pub const HARD_MAX_GENERATION_TEXT_BYTES: u64 = 256 * 1024 * 1024;
/// Hard ceiling for lossless locator and backend-encoded UTF-8 bytes.
pub const HARD_MAX_GENERATION_ENCODED_TEXT_BYTES: u64 = 16 * 1024 * 1024 * 1024;

const DEFAULT_MAX_GENERATION_ROWS: u64 = HARD_MAX_GENERATION_ROWS;
const DEFAULT_MAX_GENERATION_SOURCE_REFS: u64 = HARD_MAX_GENERATION_SOURCE_REFS;
const DEFAULT_MAX_GENERATION_TEXT_BYTES: u64 = HARD_MAX_GENERATION_TEXT_BYTES;
const ENCODED_TEXT_EXPANSION_FACTOR: u64 = 64;
const IDENTITY_RECIPE_CHECKPOINT_BYTES: usize = 4 * 1024;

/// Major/minor compatibility version for logical generation storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationContractVersion {
    major: u16,
    minor: u16,
}

impl GenerationContractVersion {
    /// Creates a generation contract version from numeric components.
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

/// Source-free semantic identity recorded for one immutable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationMetadata {
    contract_version: GenerationContractVersion,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    manifest_hash: ContentHash,
    configuration_hash: ContentHash,
    provider_set_hash: ContentHash,
}

impl GenerationMetadata {
    /// Creates generation metadata from stable semantic inputs.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError::SelfParent`] when a generation
    /// names itself as its parent, or
    /// [`GenerationValidationError::GenerationIdentityMismatch`] when the
    /// supplied identifier is not derived from the complete semantic inputs.
    pub fn new(
        repository: RepositoryId,
        generation: GenerationId,
        parent: Option<GenerationId>,
        manifest_hash: ContentHash,
        configuration_hash: ContentHash,
        provider_set_hash: ContentHash,
    ) -> Result<Self, GenerationValidationError> {
        Self::new_for_contract(
            GENERATION_CONTRACT_VERSION,
            repository,
            generation,
            parent,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        )
    }

    /// Reconstructs metadata for a supported persisted contract version.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError`] for unsupported versions,
    /// self-parenting, or a generation ID not bound to the supplied versioned
    /// semantic inputs.
    pub fn new_for_contract(
        contract_version: GenerationContractVersion,
        repository: RepositoryId,
        generation: GenerationId,
        parent: Option<GenerationId>,
        manifest_hash: ContentHash,
        configuration_hash: ContentHash,
        provider_set_hash: ContentHash,
    ) -> Result<Self, GenerationValidationError> {
        if contract_version != GENERATION_CONTRACT_VERSION
            && contract_version != LEGACY_GENERATION_CONTRACT_VERSION
        {
            return Err(GenerationValidationError::UnsupportedContractVersion);
        }
        if parent == Some(generation) {
            return Err(GenerationValidationError::SelfParent);
        }
        let expected = derive_generation(GenerationIdentity {
            repository,
            parent,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version: generation_format_version(contract_version),
        })
        .id();
        if generation != expected {
            return Err(GenerationValidationError::GenerationIdentityMismatch);
        }
        Ok(Self {
            contract_version,
            repository,
            generation,
            parent,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        })
    }

    /// Returns the additive logical storage contract version.
    #[must_use]
    pub const fn contract_version(&self) -> GenerationContractVersion {
        self.contract_version
    }

    /// Returns the baseline normalized IR version supported by this contract.
    ///
    /// Individual documents carry their own exact supported version; storage
    /// writers must persist that marker rather than this compatibility baseline.
    #[must_use]
    pub const fn ir_version(&self) -> IrVersion {
        NORMALIZED_IR_VERSION
    }

    /// Returns the owning repository.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the immutable generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the optional predecessor generation.
    #[must_use]
    pub const fn parent(&self) -> Option<GenerationId> {
        self.parent
    }

    /// Returns the canonical input-manifest hash.
    #[must_use]
    pub const fn manifest_hash(&self) -> ContentHash {
        self.manifest_hash
    }

    /// Returns the canonical configuration identity.
    #[must_use]
    pub const fn configuration_hash(&self) -> ContentHash {
        self.configuration_hash
    }

    /// Returns the canonical adapter or provider-set identity.
    #[must_use]
    pub const fn provider_set_hash(&self) -> ContentHash {
        self.provider_set_hash
    }
}

const fn generation_format_version(version: GenerationContractVersion) -> u32 {
    (version.major() as u32) << 16 | version.minor() as u32
}

/// Owned, canonical normalized IR for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationSnapshot {
    metadata: GenerationMetadata,
    document: CanonicalNormalizedIrDocument,
    source_files: SourceFileCatalog,
    coverage_ordinals_by_scope: Vec<usize>,
    symbol_explain_indexes: SymbolExplainIndexes,
}

impl GenerationSnapshot {
    /// Validates, canonicalizes, and binds normalized IR to generation metadata.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError`] for invalid IR or ownership
    /// mismatches.
    pub fn new(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
    ) -> Result<Self, GenerationValidationError> {
        Self::new_with_source_files(
            metadata,
            document,
            SourceFileCatalog::default(),
            limits,
            extensions,
        )
    }

    /// Validates normalized IR together with an exact file-only source catalog.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError`] for invalid IR, ownership drift,
    /// ambiguous file identities, or unavailable provenance.
    pub fn new_with_source_files(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
    ) -> Result<Self, GenerationValidationError> {
        match Self::new_with_source_files_and_checkpoint(
            metadata,
            document,
            source_files,
            limits,
            extensions,
            &mut || Ok::<(), Infallible>(()),
        ) {
            Ok(snapshot) => Ok(snapshot),
            Err(IndexConstructionError::Validation(error)) => Err(error),
            Err(IndexConstructionError::Checkpoint(never)) => match never {},
        }
    }

    fn new_with_source_files_and_checkpoint<E>(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, IndexConstructionError<E>> {
        checkpoint().map_err(IndexConstructionError::Checkpoint)?;
        let document =
            CanonicalNormalizedIrDocument::new(document, limits, extensions).map_err(|error| {
                IndexConstructionError::Validation(GenerationValidationError::InvalidIr(error))
            })?;
        if document.document().repository != metadata.repository()
            || document.document().generation != metadata.generation()
        {
            return Err(IndexConstructionError::Validation(
                GenerationValidationError::OwnershipMismatch,
            ));
        }
        validate_source_file_catalog(metadata, document.document(), &source_files).map_err(
            |error| {
                IndexConstructionError::Validation(GenerationValidationError::InvalidSourceCatalog(
                    error,
                ))
            },
        )?;
        let coverage_records = &document.document().coverage_records;
        let mut coverage_ordinals_by_scope = reserved_index_vec(coverage_records.len())?;
        for ordinal in 0..coverage_records.len() {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            coverage_ordinals_by_scope.push(ordinal);
        }
        sort_index_with_checkpoint(
            &mut coverage_ordinals_by_scope,
            |&ordinal| (coverage_records[ordinal].scope, ordinal),
            checkpoint,
        )?;
        let symbol_explain_indexes = SymbolExplainIndexes::build(document.document(), checkpoint)?;
        Ok(Self {
            metadata,
            document,
            source_files,
            coverage_ordinals_by_scope,
            symbol_explain_indexes,
        })
    }

    /// Validates and canonicalizes IR with cooperative collection checkpoints.
    ///
    /// Every top-level and nested record is checked before canonical sorting,
    /// and cancellation is checked again immediately after canonicalization.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError`] for invalid IR or ownership, and
    /// [`GenerationControlError`] when cancellation or a deadline stops work.
    pub fn new_with_context(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, GenerationSnapshotError> {
        Self::new_with_source_files_and_context(
            metadata,
            document,
            SourceFileCatalog::default(),
            limits,
            extensions,
            context,
        )
    }

    /// Validates normalized and file-only records with cooperative checkpoints.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::new_with_context`], including the
    /// source-catalog rows, source references, and text in the same budget.
    pub fn new_with_source_files_and_context(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, GenerationSnapshotError> {
        require_generation_budget(
            metadata.contract_version(),
            &document,
            &source_files,
            context,
        )
        .map_err(GenerationSnapshotError::Control)?;
        context.check().map_err(GenerationSnapshotError::Control)?;
        let snapshot = Self::new_with_source_files_and_checkpoint(
            metadata,
            document,
            source_files,
            limits,
            extensions,
            &mut || context.check(),
        )
        .map_err(|error| match error {
            IndexConstructionError::Checkpoint(error) => GenerationSnapshotError::Control(error),
            IndexConstructionError::Validation(error) => GenerationSnapshotError::Validation(error),
        })?;
        context.check().map_err(GenerationSnapshotError::Control)?;
        require_generation_budget(
            snapshot.metadata().contract_version(),
            snapshot.document(),
            snapshot.source_files(),
            context,
        )
        .map_err(GenerationSnapshotError::Control)?;
        Ok(snapshot)
    }

    /// Returns the generation metadata.
    #[must_use]
    pub const fn metadata(&self) -> GenerationMetadata {
        self.metadata
    }

    /// Returns the canonical normalized document.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        self.document.document()
    }

    /// Returns canonical coverage-record ordinals grouped by scope.
    ///
    /// Records within each scope retain their canonical fact-ID order. This
    /// auxiliary index is derived from the validated document and is not part
    /// of generation identity or persistence.
    #[must_use]
    pub fn coverage_ordinals_by_scope(&self) -> &[usize] {
        &self.coverage_ordinals_by_scope
    }

    /// Returns canonical relation ordinals with a direct entity endpoint.
    ///
    /// A self-relation appears once. Occurrence endpoints are not projected to
    /// their enclosing entities, matching the direct-endpoint explain contract.
    #[must_use]
    pub fn relation_ordinals_for_symbol(&self, symbol: SymbolId) -> &[usize] {
        self.symbol_explain_indexes.relations.ordinals(symbol)
    }

    /// Returns canonical occurrence ordinals targeting the symbol.
    ///
    /// Both resolved targets and every ambiguous candidate are retained across
    /// all files; an ambiguous occurrence remains ambiguous in the document.
    #[must_use]
    pub fn occurrence_ordinals_for_symbol(&self, symbol: SymbolId) -> &[usize] {
        self.symbol_explain_indexes.occurrences.ordinals(symbol)
    }

    /// Returns canonical lexical ordinals derived solely from the entity.
    ///
    /// Summary batches are excluded. Payload decoding and signature-conflict
    /// checks remain query responsibilities; this index does not infer a kind.
    #[must_use]
    pub fn lexical_ordinals_for_symbol(&self, symbol: SymbolId) -> &[usize] {
        self.symbol_explain_indexes.lexical.ordinals(symbol)
    }

    /// Returns actual heap-capacity bytes retained by the three explain indexes.
    ///
    /// This excludes fixed snapshot fields and the existing coverage index.
    /// Resident-memory admission must account for these generation-owned bytes.
    #[must_use]
    pub const fn retained_symbol_explain_index_bytes(&self) -> u64 {
        self.symbol_explain_indexes.retained_bytes
    }

    /// Estimates a conservative peak heap reservation for explain-index builds.
    ///
    /// Callers must separately admit the document and coverage index. Candidate
    /// fanout is counted with checkpoints; duplicates in noncanonical input can
    /// only increase this estimate. No source or payload is decoded.
    ///
    /// # Errors
    ///
    /// Returns control errors for cancellation or the existing row budget, and
    /// validation errors when checked workspace size cannot be represented.
    pub fn estimate_symbol_explain_index_memory_bytes(
        document: &NormalizedIrDocument,
        context: &GenerationContext<'_>,
    ) -> Result<u64, GenerationSnapshotError> {
        let counts =
            SymbolPostingCounts::count(document, &mut || context.check()).map_err(|error| {
                match error {
                    IndexConstructionError::Checkpoint(error) => {
                        GenerationSnapshotError::Control(error)
                    }
                    IndexConstructionError::Validation(error) => {
                        GenerationSnapshotError::Validation(error)
                    }
                }
            })?;
        let total = counts
            .total()
            .map_err(GenerationSnapshotError::Validation)?;
        context
            .require(
                GenerationResource::Rows,
                u64::try_from(total).map_err(|_| {
                    GenerationSnapshotError::Validation(
                        GenerationValidationError::IndexResourceUnavailable,
                    )
                })?,
            )
            .map_err(GenerationSnapshotError::Control)?;
        // Each posting can own one sort tuple, one range tuple and one ordinal.
        // Charging all three builders together also covers sequential conversion.
        let tuples = index_capacity_bytes::<(SymbolId, usize)>(total)
            .map_err(GenerationSnapshotError::Validation)?;
        let ordinals =
            index_capacity_bytes::<usize>(total).map_err(GenerationSnapshotError::Validation)?;
        tuples
            .checked_mul(2)
            .and_then(|bytes| bytes.checked_add(ordinals))
            .ok_or(GenerationSnapshotError::Validation(
                GenerationValidationError::IndexResourceUnavailable,
            ))
    }

    /// Returns the canonical file-only source catalog.
    #[must_use]
    pub const fn source_files(&self) -> &SourceFileCatalog {
        &self.source_files
    }

    /// Finds a normalized or file-only record by stable identity.
    #[must_use]
    pub fn find_file(&self, file: rootlight_ids::FileId) -> Option<&FileRecord> {
        self.document
            .document()
            .files
            .binary_search_by_key(&file, |record| record.id)
            .ok()
            .and_then(|index| self.document.document().files.get(index))
            .or_else(|| self.source_files.find(file))
    }

    /// Returns the total number of normalized and file-only records.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.document
            .document()
            .files
            .len()
            .saturating_add(self.source_files.len())
    }

    /// Attaches a restored file-only catalog before identity verification.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError`] when the snapshot already carries
    /// a catalog or restored records disagree with its ownership and provenance.
    pub fn with_source_files(
        mut self,
        source_files: SourceFileCatalog,
    ) -> Result<Self, GenerationValidationError> {
        if !self.source_files.is_empty() {
            return Err(GenerationValidationError::InvalidSourceCatalog(
                SourceFileCatalogError::AlreadyAttached,
            ));
        }
        validate_source_file_catalog(self.metadata, self.document(), &source_files)
            .map_err(GenerationValidationError::InvalidSourceCatalog)?;
        self.source_files = source_files;
        Ok(self)
    }

    /// Consumes the snapshot into its canonical normalized document.
    #[must_use]
    pub fn into_document(self) -> NormalizedIrDocument {
        self.document.into_document()
    }

    fn generation_neutral_digests(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<CanonicalGenerationNeutralDigests, GenerationNeutralDigestError> {
        context
            .check()
            .map_err(GenerationNeutralDigestError::Control)?;
        let mut control_error = None;
        self.document
            .generation_neutral_digests(|| match context.check() {
                Ok(()) => true,
                Err(error) => {
                    control_error.get_or_insert(error);
                    false
                }
            })
            .map_err(|error| match error {
                NormalizedRebindError::Interrupted => control_error.map_or(
                    GenerationNeutralDigestError::InvalidGeneration,
                    GenerationNeutralDigestError::Control,
                ),
                NormalizedRebindError::ResourceLimit => {
                    GenerationNeutralDigestError::ResourceUnavailable
                }
                _ => GenerationNeutralDigestError::InvalidGeneration,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct SymbolPostingIndex {
    starts: Vec<(SymbolId, usize)>,
    ordinals: Vec<usize>,
}

impl SymbolPostingIndex {
    fn from_entries<E>(
        mut entries: Vec<(SymbolId, usize)>,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, IndexConstructionError<E>> {
        sort_index_with_checkpoint(&mut entries, |entry| *entry, checkpoint)?;
        let mut symbols = 0_usize;
        let mut previous = None;
        for &(symbol, _) in &entries {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            if previous != Some(symbol) {
                symbols = symbols
                    .checked_add(1)
                    .ok_or_else(index_workspace_unavailable)?;
                previous = Some(symbol);
            }
        }
        let mut starts = reserved_index_vec(symbols)?;
        let mut ordinals = reserved_index_vec(entries.len())?;
        previous = None;
        for (symbol, ordinal) in entries {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            if previous != Some(symbol) {
                starts.push((symbol, ordinals.len()));
                previous = Some(symbol);
            }
            ordinals.push(ordinal);
        }
        Ok(Self { starts, ordinals })
    }

    fn ordinals(&self, symbol: SymbolId) -> &[usize] {
        let Ok(index) = self.starts.binary_search_by_key(&symbol, |entry| entry.0) else {
            return &[];
        };
        let start = self.starts[index].1;
        let end = self
            .starts
            .get(index + 1)
            .map_or(self.ordinals.len(), |entry| entry.1);
        &self.ordinals[start..end]
    }

    fn retained_bytes(&self) -> Result<u64, GenerationValidationError> {
        index_capacity_bytes::<(SymbolId, usize)>(self.starts.capacity())?
            .checked_add(index_capacity_bytes::<usize>(self.ordinals.capacity())?)
            .ok_or(GenerationValidationError::IndexResourceUnavailable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SymbolExplainIndexes {
    relations: SymbolPostingIndex,
    occurrences: SymbolPostingIndex,
    lexical: SymbolPostingIndex,
    retained_bytes: u64,
}

impl SymbolExplainIndexes {
    fn build<E>(
        document: &NormalizedIrDocument,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, IndexConstructionError<E>> {
        let counts = SymbolPostingCounts::count(document, checkpoint)?;
        let mut entries = reserved_index_vec(counts.relations)?;
        for (ordinal, relation) in document.relations.iter().enumerate() {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            for symbol in direct_relation_entities(relation).into_iter().flatten() {
                entries.push((symbol, ordinal));
            }
        }
        let relations = SymbolPostingIndex::from_entries(entries, checkpoint)?;
        let mut entries = reserved_index_vec(counts.occurrences)?;
        for (ordinal, occurrence) in document.occurrences.iter().enumerate() {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            match &occurrence.target {
                OccurrenceTarget::Resolved { symbol } => entries.push((*symbol, ordinal)),
                OccurrenceTarget::Candidates { symbols, .. } => {
                    for &symbol in symbols {
                        checkpoint().map_err(IndexConstructionError::Checkpoint)?;
                        entries.push((symbol, ordinal));
                    }
                }
                OccurrenceTarget::Unresolved { .. } => {}
            }
        }
        let occurrences = SymbolPostingIndex::from_entries(entries, checkpoint)?;
        let mut entries = reserved_index_vec(counts.lexical)?;
        for (ordinal, envelope) in document.extensions.iter().enumerate() {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            if let Some(symbol) = lexical_entity_owner(envelope) {
                entries.push((symbol, ordinal));
            }
        }
        let lexical = SymbolPostingIndex::from_entries(entries, checkpoint)?;
        let relation_bytes = relations
            .retained_bytes()
            .map_err(IndexConstructionError::Validation)?;
        let occurrence_bytes = occurrences
            .retained_bytes()
            .map_err(IndexConstructionError::Validation)?;
        let lexical_bytes = lexical
            .retained_bytes()
            .map_err(IndexConstructionError::Validation)?;
        let retained_bytes = relation_bytes
            .checked_add(occurrence_bytes)
            .and_then(|bytes| bytes.checked_add(lexical_bytes))
            .ok_or_else(index_workspace_unavailable)?;
        Ok(Self {
            relations,
            occurrences,
            lexical,
            retained_bytes,
        })
    }
}

struct SymbolPostingCounts {
    relations: usize,
    occurrences: usize,
    lexical: usize,
}

impl SymbolPostingCounts {
    fn count<E>(
        document: &NormalizedIrDocument,
        checkpoint: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<Self, IndexConstructionError<E>> {
        let mut result = Self {
            relations: 0,
            occurrences: 0,
            lexical: 0,
        };
        for relation in &document.relations {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            for _ in direct_relation_entities(relation).into_iter().flatten() {
                result.relations = result
                    .relations
                    .checked_add(1)
                    .ok_or_else(index_workspace_unavailable)?;
            }
        }
        for occurrence in &document.occurrences {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            match &occurrence.target {
                OccurrenceTarget::Resolved { .. } => {
                    result.occurrences = result
                        .occurrences
                        .checked_add(1)
                        .ok_or_else(index_workspace_unavailable)?;
                }
                OccurrenceTarget::Candidates { symbols, .. } => {
                    for _ in symbols {
                        checkpoint().map_err(IndexConstructionError::Checkpoint)?;
                        result.occurrences = result
                            .occurrences
                            .checked_add(1)
                            .ok_or_else(index_workspace_unavailable)?;
                    }
                }
                OccurrenceTarget::Unresolved { .. } => {}
            }
        }
        for envelope in &document.extensions {
            checkpoint().map_err(IndexConstructionError::Checkpoint)?;
            if lexical_entity_owner(envelope).is_some() {
                result.lexical = result
                    .lexical
                    .checked_add(1)
                    .ok_or_else(index_workspace_unavailable)?;
            }
        }
        Ok(result)
    }

    fn total(&self) -> Result<usize, GenerationValidationError> {
        self.relations
            .checked_add(self.occurrences)
            .and_then(|total| total.checked_add(self.lexical))
            .ok_or(GenerationValidationError::IndexResourceUnavailable)
    }
}

#[derive(Debug)]
enum IndexConstructionError<E> {
    Checkpoint(E),
    Validation(GenerationValidationError),
}

fn index_workspace_unavailable<E>() -> IndexConstructionError<E> {
    IndexConstructionError::Validation(GenerationValidationError::IndexResourceUnavailable)
}

fn reserved_index_vec<T, E>(capacity: usize) -> Result<Vec<T>, IndexConstructionError<E>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(capacity)
        .map_err(|_| index_workspace_unavailable())?;
    Ok(result)
}

fn index_capacity_bytes<T>(capacity: usize) -> Result<u64, GenerationValidationError> {
    let bytes = capacity
        .checked_mul(std::mem::size_of::<T>())
        .ok_or(GenerationValidationError::IndexResourceUnavailable)?;
    u64::try_from(bytes).map_err(|_| GenerationValidationError::IndexResourceUnavailable)
}

fn direct_relation_entities(relation: &rootlight_ir::RelationRecord) -> [Option<SymbolId>; 2] {
    let subject = match relation.subject {
        RelationEndpoint::Entity(symbol) => Some(symbol),
        RelationEndpoint::Repository(_)
        | RelationEndpoint::File(_)
        | RelationEndpoint::Occurrence(_) => None,
    };
    let object = match relation.object {
        RelationEndpoint::Entity(symbol) => (Some(symbol) != subject).then_some(symbol),
        RelationEndpoint::Repository(_)
        | RelationEndpoint::File(_)
        | RelationEndpoint::Occurrence(_) => None,
    };
    [subject, object]
}

fn lexical_entity_owner(envelope: &rootlight_ir::ExtensionEnvelope) -> Option<SymbolId> {
    if envelope.namespace != LEXICAL_EXTENSION_NAMESPACE
        || envelope.version == LEXICAL_SUMMARY_BATCH_VERSION
    {
        return None;
    }
    match envelope.evidence.derivation.as_slice() {
        [FactRef::Entity(symbol)] => Some(*symbol),
        _ => None,
    }
}

// Heap sorting checks cancellation during comparisons without allocating a
// second posting buffer. Canonical ordinals break ties independently of names.
fn sort_index_with_checkpoint<T, K: Ord, E>(
    values: &mut [T],
    key: impl Fn(&T) -> K,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), IndexConstructionError<E>> {
    checkpoint().map_err(IndexConstructionError::Checkpoint)?;
    let length = values.len();
    for root in (0..length / 2).rev() {
        sift_index_heap(values, root, length, &key, checkpoint)?;
    }
    for end in (1..values.len()).rev() {
        checkpoint().map_err(IndexConstructionError::Checkpoint)?;
        values.swap(0, end);
        sift_index_heap(values, 0, end, &key, checkpoint)?;
    }
    checkpoint().map_err(IndexConstructionError::Checkpoint)
}

fn sift_index_heap<T, K: Ord, E>(
    values: &mut [T],
    mut root: usize,
    end: usize,
    key: &impl Fn(&T) -> K,
    checkpoint: &mut impl FnMut() -> Result<(), E>,
) -> Result<(), IndexConstructionError<E>> {
    while root < end / 2 {
        checkpoint().map_err(IndexConstructionError::Checkpoint)?;
        let mut child = root * 2 + 1;
        if child + 1 < end && key(&values[child]) < key(&values[child + 1]) {
            child += 1;
        }
        if key(&values[root]) >= key(&values[child]) {
            break;
        }
        values.swap(root, child);
        root = child;
    }
    Ok(())
}

fn validate_source_file_catalog(
    metadata: GenerationMetadata,
    document: &NormalizedIrDocument,
    source_files: &SourceFileCatalog,
) -> Result<(), SourceFileCatalogError> {
    source_files.validate_ownership(metadata.repository(), metadata.generation())?;
    let normalized_paths = document
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<BTreeSet<_>>();
    for entry in source_files.entries() {
        let file = entry.file();
        if document
            .files
            .binary_search_by_key(&file.id, |candidate| candidate.id)
            .is_ok()
            || normalized_paths.contains(file.path.as_str())
        {
            return Err(SourceFileCatalogError::NormalizedFileOverlap);
        }
        if document
            .provenance
            .binary_search_by_key(&file.provenance, |record| record.id)
            .is_err()
        {
            return Err(SourceFileCatalogError::MissingProvenance);
        }
    }
    Ok(())
}

fn require_generation_budget<'snapshot>(
    contract_version: GenerationContractVersion,
    document: &'snapshot NormalizedIrDocument,
    source_files: &'snapshot SourceFileCatalog,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    context.check()?;
    let logical_rows = [
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
        source_files.len(),
        source_files.len(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, length| {
        checked_add_resource(
            total,
            usize_to_budget_u64(length, GenerationResource::Rows, context)?,
            GenerationResource::Rows,
            context,
        )
    })?;
    let schema_rows = if contract_version == LEGACY_GENERATION_CONTRACT_VERSION {
        1
    } else {
        2
    };
    let mut rows = checked_add_resource(schema_rows, 1, GenerationResource::Rows, context)?;
    rows = checked_add_resource(rows, logical_rows, GenerationResource::Rows, context)?;
    rows = checked_add_resource(rows, logical_rows, GenerationResource::Rows, context)?;
    let mut text_bytes = 0_u64;
    let mut encoded_text_bytes = 0_u64;
    let mut sources = BTreeSet::new();

    for file in &document.files {
        context.check()?;
        observe_evidence(&file.evidence, &mut sources, &mut rows, context)?;
        observe_text(&file.path, &mut text_bytes, context)?;
        if let Some(locator) = &file.path_locator {
            for component in locator.components() {
                observe_text(component, &mut text_bytes, context)?;
            }
        }
        observe_text(&file.language, &mut text_bytes, context)?;
        observe_text(&file.encoding, &mut text_bytes, context)?;
        context.check()?;
    }
    for entry in source_files.entries() {
        context.check()?;
        let file = entry.file();
        observe_evidence(&file.evidence, &mut sources, &mut rows, context)?;
        observe_text(&file.path, &mut text_bytes, context)?;
        if let Some(locator) = &file.path_locator {
            for component in locator.components() {
                observe_encoded_text(component, &mut encoded_text_bytes, context)?;
            }
        }
        observe_text(&file.language, &mut text_bytes, context)?;
        observe_text(&file.encoding, &mut text_bytes, context)?;
        encoded_text_bytes = checked_add_resource(
            encoded_text_bytes,
            usize_to_budget_u64(
                entry.path_identity().len(),
                GenerationResource::EncodedTextBytes,
                context,
            )?,
            GenerationResource::EncodedTextBytes,
            context,
        )?;
        context.check()?;
    }
    for entity in &document.entities {
        context.check()?;
        for _ in &entity.flags {
            rows = checked_add_resource(rows, 1, GenerationResource::Rows, context)?;
        }
        observe_evidence(&entity.evidence, &mut sources, &mut rows, context)?;
        observe_text(&entity.language, &mut text_bytes, context)?;
        observe_text(&entity.canonical_name, &mut text_bytes, context)?;
        observe_text(&entity.display_name, &mut text_bytes, context)?;
        observe_text(&entity.qualified_name, &mut text_bytes, context)?;
        context.check()?;
    }
    for occurrence in &document.occurrences {
        context.check()?;
        observe_source(&occurrence.source, &mut sources, &mut rows, context)?;
        if let OccurrenceTarget::Candidates { symbols, .. } = &occurrence.target {
            for _ in symbols {
                rows = checked_add_resource(rows, 1, GenerationResource::Rows, context)?;
            }
        }
        observe_evidence(&occurrence.evidence, &mut sources, &mut rows, context)?;
        observe_text(&occurrence.syntax_kind, &mut text_bytes, context)?;
        context.check()?;
    }
    for relation in &document.relations {
        context.check()?;
        observe_evidence(&relation.evidence, &mut sources, &mut rows, context)?;
        context.check()?;
    }
    for provenance in &document.provenance {
        context.check()?;
        for source in provenance
            .input_sources
            .iter()
            .chain(&provenance.evidence_sources)
        {
            rows = checked_add_resource(rows, 1, GenerationResource::Rows, context)?;
            observe_source(source, &mut sources, &mut rows, context)?;
        }
        for _ in &provenance.derivation_parents {
            rows = checked_add_resource(rows, 1, GenerationResource::Rows, context)?;
        }
        observe_text(provenance.producer.name(), &mut text_bytes, context)?;
        observe_text(provenance.producer.version(), &mut text_bytes, context)?;
        observe_optional_text(
            provenance.frontend_version.as_deref(),
            &mut text_bytes,
            context,
        )?;
        observe_text(&provenance.language, &mut text_bytes, context)?;
        observe_optional_text(provenance.rule.as_deref(), &mut text_bytes, context)?;
        context.check()?;
    }
    for mapping in &document.source_mappings {
        context.check()?;
        observe_source(&mapping.from, &mut sources, &mut rows, context)?;
        observe_source(&mapping.to, &mut sources, &mut rows, context)?;
        observe_opaque_evidence(&mapping.evidence, &mut sources, &mut rows, context)?;
        context.check()?;
    }
    for coverage in &document.coverage_records {
        context.check()?;
        observe_evidence(&coverage.evidence, &mut sources, &mut rows, context)?;
        context.check()?;
    }
    for region in &document.skipped_regions {
        context.check()?;
        observe_source(&region.source, &mut sources, &mut rows, context)?;
        observe_opaque_evidence(&region.evidence, &mut sources, &mut rows, context)?;
        observe_text(&region.detail, &mut text_bytes, context)?;
        context.check()?;
    }
    for diagnostic in &document.diagnostics {
        context.check()?;
        if let Some(source) = &diagnostic.source {
            observe_source(source, &mut sources, &mut rows, context)?;
        }
        observe_opaque_evidence(&diagnostic.evidence, &mut sources, &mut rows, context)?;
        observe_text(&diagnostic.code, &mut text_bytes, context)?;
        observe_text(&diagnostic.message, &mut text_bytes, context)?;
        context.check()?;
    }
    for extension in &document.extensions {
        context.check()?;
        observe_opaque_evidence(&extension.evidence, &mut sources, &mut rows, context)?;
        observe_text(&extension.namespace, &mut text_bytes, context)?;
        observe_text(&extension.version, &mut text_bytes, context)?;
        observe_text(&extension.payload, &mut text_bytes, context)?;
        context.check()?;
    }
    context.require(GenerationResource::Rows, rows)?;
    context.require(
        GenerationResource::SourceReferences,
        usize_to_budget_u64(sources.len(), GenerationResource::SourceReferences, context)?,
    )?;
    context.require(GenerationResource::TextBytes, text_bytes)?;
    context.require(GenerationResource::EncodedTextBytes, encoded_text_bytes)?;
    context.check()
}

fn observe_evidence<'document>(
    evidence: &'document FactEvidence,
    sources: &mut BTreeSet<&'document SourceRef>,
    rows: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    if let Some(source) = &evidence.source {
        observe_source(source, sources, rows, context)?;
    }
    for _ in &evidence.derivation {
        *rows = checked_add_resource(*rows, 1, GenerationResource::Rows, context)?;
    }
    Ok(())
}

fn observe_opaque_evidence<'document>(
    evidence: &'document FactEvidence,
    sources: &mut BTreeSet<&'document SourceRef>,
    rows: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    if let Some(source) = &evidence.source {
        observe_source(source, sources, rows, context)?;
    }
    // Opaque facts serialize derivations into one payload. Counting each item
    // as materialized work keeps canonicalization inside the same row cap.
    for _ in &evidence.derivation {
        *rows = checked_add_resource(*rows, 1, GenerationResource::Rows, context)?;
    }
    Ok(())
}

fn observe_source<'document>(
    source: &'document SourceRef,
    sources: &mut BTreeSet<&'document SourceRef>,
    rows: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    context.check()?;
    if !sources.contains(source) {
        let source_count = sources
            .len()
            .checked_add(1)
            .ok_or_else(|| budget_exceeded(GenerationResource::SourceReferences, context))?;
        context.require(
            GenerationResource::SourceReferences,
            usize_to_budget_u64(source_count, GenerationResource::SourceReferences, context)?,
        )?;
        let next_rows = checked_add_resource(*rows, 1, GenerationResource::Rows, context)?;
        context.check()?;
        let inserted = sources.insert(source);
        debug_assert!(inserted, "source membership changed without mutation");
        *rows = next_rows;
    }
    context.check()
}

fn observe_text(
    value: &str,
    text_bytes: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    context.check()?;
    *text_bytes = checked_add_resource(
        *text_bytes,
        usize_to_budget_u64(value.len(), GenerationResource::TextBytes, context)?,
        GenerationResource::TextBytes,
        context,
    )?;
    context.check()
}

fn observe_encoded_text(
    value: &str,
    encoded_text_bytes: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    context.check()?;
    *encoded_text_bytes = checked_add_resource(
        *encoded_text_bytes,
        usize_to_budget_u64(value.len(), GenerationResource::EncodedTextBytes, context)?,
        GenerationResource::EncodedTextBytes,
        context,
    )?;
    context.check()
}

fn observe_optional_text(
    value: Option<&str>,
    text_bytes: &mut u64,
    context: &GenerationContext<'_>,
) -> Result<(), GenerationControlError> {
    if let Some(value) = value {
        observe_text(value, text_bytes, context)?;
    }
    Ok(())
}

fn checked_add_resource(
    current: u64,
    increment: u64,
    resource: GenerationResource,
    context: &GenerationContext<'_>,
) -> Result<u64, GenerationControlError> {
    context.check()?;
    let next = current
        .checked_add(increment)
        .ok_or_else(|| budget_exceeded(resource, context))?;
    context.require(resource, next)?;
    Ok(next)
}

fn usize_to_budget_u64(
    value: usize,
    resource: GenerationResource,
    context: &GenerationContext<'_>,
) -> Result<u64, GenerationControlError> {
    u64::try_from(value).map_err(|_| budget_exceeded(resource, context))
}

fn budget_exceeded(
    resource: GenerationResource,
    context: &GenerationContext<'_>,
) -> GenerationControlError {
    GenerationControlError::BudgetExceeded {
        resource,
        observed: u64::MAX,
        limit: context.budget().limit(resource),
    }
}

/// Canonical versioned input-manifest recipe used by generation identity.
///
/// File claims are untrusted producer inputs. Constructing this value does not
/// verify a generation; [`IdentityVerifiedGeneration::verify`] independently
/// compares the claims with canonical records before granting the opaque type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationManifestRecipe {
    recipe_version: GenerationContractVersion,
    repository: RepositoryId,
    configuration_hash: ContentHash,
    files: Vec<FileIdentityClaim>,
}

impl GenerationManifestRecipe {
    /// Creates a canonical manifest recipe from unverified file claims.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityVerificationError::IdentityMismatch`] for duplicate
    /// file IDs, mismatched repositories, or claims that do not recompute to
    /// their asserted file IDs.
    pub fn new(
        repository: RepositoryId,
        configuration_hash: ContentHash,
        files: Vec<FileIdentityClaim>,
    ) -> Result<Self, IdentityVerificationError> {
        Self::new_with_checkpoint(repository, configuration_hash, files, || Ok(()))
    }

    fn new_with_context(
        repository: RepositoryId,
        configuration_hash: ContentHash,
        files: Vec<FileIdentityClaim>,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        Self::new_with_checkpoint(repository, configuration_hash, files, || {
            context.check().map_err(IdentityVerificationError::Control)
        })
    }

    fn new_with_checkpoint(
        repository: RepositoryId,
        configuration_hash: ContentHash,
        mut files: Vec<FileIdentityClaim>,
        mut checkpoint: impl FnMut() -> Result<(), IdentityVerificationError>,
    ) -> Result<Self, IdentityVerificationError> {
        checkpoint()?;
        files.sort();
        checkpoint()?;
        let mut previous = None;
        for claim in &files {
            checkpoint()?;
            let derived_file = claim.derived_file();
            checkpoint()?;
            if claim.repository != repository
                || derived_file != claim.file
                || previous == Some(claim.file)
            {
                return Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::FileClaim,
                ));
            }
            previous = Some(claim.file);
            checkpoint()?;
        }
        Ok(Self {
            recipe_version: PROPOSED_IDENTITY_CLAIM_VERSION,
            repository,
            configuration_hash,
            files,
        })
    }

    /// Returns the canonical manifest hash consumed by [`GenerationMetadata`].
    ///
    /// # Errors
    ///
    /// Returns [`IdentityVerificationError::RecipeEncoding`] if the fixed
    /// canonical representation cannot be encoded.
    pub fn canonical_hash(&self) -> Result<ContentHash, IdentityVerificationError> {
        let mut writer = IdentityRecipeHashWriter::new(None)?;
        serde_json::to_writer(&mut writer, self)
            .map_err(|_| IdentityVerificationError::RecipeEncoding)?;
        writer.finish()
    }

    fn canonical_hash_with_context(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<ContentHash, IdentityVerificationError> {
        let mut writer = IdentityRecipeHashWriter::new(Some(*context))?;
        if serde_json::to_writer(&mut writer, self).is_err() {
            return writer
                .control
                .map_or(Err(IdentityVerificationError::RecipeEncoding), |error| {
                    Err(IdentityVerificationError::Control(error))
                });
        }
        writer.finish()
    }

    /// Returns the canonical file claims in stable ID order.
    #[must_use]
    pub fn files(&self) -> &[FileIdentityClaim] {
        &self.files
    }
}

struct IdentityRecipeHashWriter<'context> {
    hasher: blake3::Hasher,
    context: Option<GenerationContext<'context>>,
    control: Option<GenerationControlError>,
}

impl<'context> IdentityRecipeHashWriter<'context> {
    fn new(
        context: Option<GenerationContext<'context>>,
    ) -> Result<Self, IdentityVerificationError> {
        if let Some(context) = context {
            context
                .check()
                .map_err(IdentityVerificationError::Control)?;
        }
        Ok(Self {
            hasher: blake3::Hasher::new(),
            context,
            control: None,
        })
    }

    fn finish(self) -> Result<ContentHash, IdentityVerificationError> {
        if let Some(error) = self.control {
            return Err(IdentityVerificationError::Control(error));
        }
        if let Some(context) = self.context {
            context
                .check()
                .map_err(IdentityVerificationError::Control)?;
        }
        Ok(ContentHash::from_bytes(*self.hasher.finalize().as_bytes()))
    }

    fn stop(&mut self, error: GenerationControlError) -> io::Error {
        self.control = Some(error);
        io::Error::other("identity recipe hashing stopped")
    }
}

impl Write for IdentityRecipeHashWriter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        for chunk in buffer.chunks(IDENTITY_RECIPE_CHECKPOINT_BYTES) {
            if let Some(context) = self.context
                && let Err(error) = context.check()
            {
                return Err(self.stop(error));
            }
            self.hasher.update(chunk);
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A generation whose stable identities were recomputed from canonical records.
///
/// The type has no unchecked constructor. Public claim envelopes remain
/// forgeable transport records; only [`Self::verify`] can grant this wrapper.
#[derive(Debug, PartialEq, Eq)]
pub struct IdentityVerifiedGeneration {
    snapshot: GenerationSnapshot,
}

impl IdentityVerifiedGeneration {
    /// Canonicalizes a generation and independently verifies all stable IDs.
    ///
    /// The verifier requires exactly one file claim per file and one symbol
    /// claim per entity, binds the canonical file manifest to generation
    /// metadata, recomputes every common fact ID from its typed v2 recipe, and
    /// validates each supported extension identity. Arbitrary caller-supplied
    /// IDs cannot enter the returned opaque wrapper.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityVerificationError`] for interruption, invalid IR,
    /// missing or extra claims, unsupported extensions, manifest drift, or any
    /// identity mismatch.
    pub fn verify(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        Self::verify_with_source_files(
            metadata,
            document,
            SourceFileCatalog::default(),
            limits,
            extensions,
            context,
        )
    }

    /// Verifies normalized facts and exact file-only retrieval records together.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::verify`]. Every catalog claim also
    /// participates in the generation manifest identity check.
    pub fn verify_with_source_files(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        let snapshot = GenerationSnapshot::new_with_source_files_and_context(
            metadata,
            document,
            source_files,
            limits,
            extensions,
            context,
        )
        .map_err(IdentityVerificationError::from_snapshot)?;
        Self::verify_snapshot(snapshot, context)
    }

    /// Verifies an already canonical generation snapshot.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::verify`].
    pub fn verify_snapshot(
        snapshot: GenerationSnapshot,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if snapshot.metadata().contract_version() != GENERATION_CONTRACT_VERSION {
            return Err(IdentityVerificationError::LegacyContract);
        }
        require_generation_budget(
            snapshot.metadata().contract_version(),
            snapshot.document(),
            snapshot.source_files(),
            context,
        )
        .map_err(IdentityVerificationError::Control)?;
        verify_snapshot_identities(&snapshot, context)?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        Ok(Self { snapshot })
    }

    /// Restores a crash-safe published snapshot from its exact bounded JSON image.
    ///
    /// The digest may be stored beside the recovery image, so it proves
    /// consistency rather than authenticity. Restoration therefore rechecks
    /// the normalized IR contract, quotas, metadata ownership, and every
    /// identity recipe before returning the verified capability.
    ///
    /// # Errors
    ///
    /// Returns [`IdentityVerificationError::InvalidGeneration`] when the digest,
    /// document contract, or metadata binding differs, an identity-verification
    /// error when a claim, fact ID, or manifest binding is invalid, and
    /// [`IdentityVerificationError::Control`] when the recovery budget or
    /// cancellation policy stops the operation.
    pub fn restore_published_json(
        metadata: GenerationMetadata,
        encoded: &[u8],
        expected_digest: ContentHash,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        Self::restore_published_json_with_source_files(
            metadata,
            encoded,
            expected_digest,
            SourceFileCatalog::default(),
            limits,
            extensions,
            context,
        )
    }

    /// Restores normalized JSON and a separately checksummed file-only catalog.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::restore_published_json`].
    pub fn restore_published_json_with_source_files(
        metadata: GenerationMetadata,
        encoded: &[u8],
        expected_digest: ContentHash,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        verify_published_digest(encoded, expected_digest, context)?;
        let document =
            decode_normalized_ir_document_with_checkpoint(encoded, limits, extensions, || {
                context.check().is_ok()
            })
            .map_err(|error| match error {
                IrDocumentDecodeError::Interrupted => context.check().err().map_or(
                    IdentityVerificationError::InvalidGeneration,
                    IdentityVerificationError::Control,
                ),
                _ => IdentityVerificationError::InvalidGeneration,
            })?;
        Self::restore_decoded_published(
            metadata,
            document,
            source_files,
            limits,
            extensions,
            context,
        )
    }

    /// Restores a published snapshot from an exact-length MessagePack stream.
    ///
    /// The stream is hashed as it is decoded, avoiding a second in-memory copy
    /// of large durable recovery documents while preserving the same integrity
    /// and identity verification as [`Self::restore_published_json`].
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::restore_published_json`].
    pub fn restore_published_messagepack_reader<R: Read>(
        metadata: GenerationMetadata,
        reader: R,
        encoded_bytes: usize,
        expected_digest: ContentHash,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        Self::restore_published_messagepack_reader_with_source_files(
            metadata,
            reader,
            encoded_bytes,
            expected_digest,
            SourceFileCatalog::default(),
            limits,
            extensions,
            context,
        )
    }

    /// Restores MessagePack normalized IR and a checksummed file-only catalog.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::restore_published_messagepack_reader`].
    #[expect(
        clippy::too_many_arguments,
        reason = "the encoded stream and independently persisted catalog have separate integrity inputs"
    )]
    pub fn restore_published_messagepack_reader_with_source_files<R: Read>(
        metadata: GenerationMetadata,
        reader: R,
        encoded_bytes: usize,
        expected_digest: ContentHash,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        let mut digest = PublishedDigestReader::new(reader, *context)?;
        let maximum_read = u64::try_from(encoded_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or(IdentityVerificationError::InvalidGeneration)?;
        // MessagePack reads tiny markers; batch above the digest so hashing and
        // cancellation checks operate on checkpoint-sized chunks. Keep read-ahead
        // within the declared document plus the existing trailing-byte probe.
        let buffered = BufReader::with_capacity(
            IDENTITY_RECIPE_CHECKPOINT_BYTES,
            (&mut digest).take(maximum_read),
        );
        let document = decode_normalized_ir_document_messagepack_reader_with_checkpoint(
            buffered,
            encoded_bytes,
            limits,
            extensions,
            || context.check().is_ok(),
        );
        if let Some(error) = digest.control {
            return Err(IdentityVerificationError::Control(error));
        }
        let document = document.map_err(|error| match error {
            IrDocumentDecodeError::Interrupted => context.check().err().map_or(
                IdentityVerificationError::InvalidGeneration,
                IdentityVerificationError::Control,
            ),
            _ => IdentityVerificationError::InvalidGeneration,
        })?;
        if digest.finish()? != expected_digest {
            return Err(IdentityVerificationError::InvalidGeneration);
        }
        Self::restore_decoded_published(
            metadata,
            document,
            source_files,
            limits,
            extensions,
            context,
        )
    }

    fn restore_decoded_published(
        metadata: GenerationMetadata,
        document: NormalizedIrDocument,
        source_files: SourceFileCatalog,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        context: &GenerationContext<'_>,
    ) -> Result<Self, IdentityVerificationError> {
        if document.repository != metadata.repository()
            || document.generation != metadata.generation()
            || metadata.contract_version() != GENERATION_CONTRACT_VERSION
        {
            return Err(IdentityVerificationError::InvalidGeneration);
        }
        let snapshot = GenerationSnapshot::new_with_source_files_and_context(
            metadata,
            document,
            source_files,
            limits,
            extensions,
            context,
        )
        .map_err(IdentityVerificationError::from_snapshot)?;
        Self::verify_snapshot(snapshot, context)
    }

    /// Returns the verified generation metadata.
    #[must_use]
    pub const fn metadata(&self) -> GenerationMetadata {
        self.snapshot.metadata()
    }

    /// Returns the verified canonical normalized document.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        self.snapshot.document()
    }

    /// Returns the canonical snapshot behind this verified capability.
    #[must_use]
    pub const fn snapshot(&self) -> &GenerationSnapshot {
        &self.snapshot
    }

    /// Streams neutral logical digests from this verified canonical generation.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationNeutralDigestError::Control`] when cancellation
    /// interrupts projection, [`GenerationNeutralDigestError::ResourceUnavailable`]
    /// when checked workspace reservation fails, or
    /// [`GenerationNeutralDigestError::InvalidGeneration`] when a verified
    /// identity recipe cannot be projected.
    pub fn generation_neutral_digests(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<CanonicalGenerationNeutralDigests, GenerationNeutralDigestError> {
        self.snapshot.generation_neutral_digests(context)
    }

    /// Consumes the verified wrapper into its canonical generation.
    #[must_use]
    pub fn into_snapshot(self) -> GenerationSnapshot {
        self.snapshot
    }
}

fn verify_published_digest(
    encoded: &[u8],
    expected_digest: ContentHash,
    context: &GenerationContext<'_>,
) -> Result<(), IdentityVerificationError> {
    context
        .check()
        .map_err(IdentityVerificationError::Control)?;
    let mut digest = IdentityRecipeHashWriter::new(Some(*context))?;
    if digest.write_all(encoded).is_err() {
        return digest
            .control
            .map_or(Err(IdentityVerificationError::InvalidGeneration), |error| {
                Err(IdentityVerificationError::Control(error))
            });
    }
    if digest.finish()? != expected_digest {
        return Err(IdentityVerificationError::InvalidGeneration);
    }
    context.check().map_err(IdentityVerificationError::Control)
}

struct PublishedDigestReader<'context, R> {
    inner: R,
    hasher: blake3::Hasher,
    context: GenerationContext<'context>,
    control: Option<GenerationControlError>,
}

impl<'context, R> PublishedDigestReader<'context, R> {
    fn new(
        inner: R,
        context: GenerationContext<'context>,
    ) -> Result<Self, IdentityVerificationError> {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        Ok(Self {
            inner,
            hasher: blake3::Hasher::new(),
            context,
            control: None,
        })
    }

    fn finish(self) -> Result<ContentHash, IdentityVerificationError> {
        if let Some(error) = self.control {
            return Err(IdentityVerificationError::Control(error));
        }
        self.context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        Ok(ContentHash::from_bytes(*self.hasher.finalize().as_bytes()))
    }

    fn stop(&mut self, error: GenerationControlError) -> io::Error {
        self.control = Some(error);
        io::Error::other("published digest verification stopped")
    }
}

impl<R: Read> Read for PublishedDigestReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        for chunk in buffer[..read].chunks(IDENTITY_RECIPE_CHECKPOINT_BYTES) {
            if let Err(error) = self.context.check() {
                return Err(self.stop(error));
            }
            self.hasher.update(chunk);
        }
        Ok(read)
    }
}

fn verify_snapshot_identities(
    snapshot: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<(), IdentityVerificationError> {
    let document = snapshot.document();
    let mut file_claims = BTreeMap::new();
    let mut symbol_claims = BTreeMap::new();
    let mut lexical_batch_files = None;
    let mut has_unsupported_extension = false;
    for envelope in &document.extensions {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        match envelope.namespace.as_str() {
            rootlight_ir::GROOVY_VALUE_LOOKUP_NAMESPACE => {
                rootlight_ir::decode_groovy_value_lookup_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::GROOVY_CALL_LOOKUP_NAMESPACE => {
                rootlight_ir::decode_groovy_call_lookup_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::GROOVY_STATIC_IMPORT_NAMESPACE => {
                rootlight_ir::decode_groovy_static_import_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::GROOVY_CALLABLE_NAMESPACE => {
                rootlight_ir::decode_groovy_callable_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::GROOVY_MEMBER_NAMESPACE => {
                rootlight_ir::decode_groovy_member_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::GROOVY_TYPE_LOOKUP_NAMESPACE => {
                rootlight_ir::decode_groovy_type_lookup_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::GROOVY_BINDING_NAMESPACE => {
                rootlight_ir::decode_groovy_binding_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            FILE_IDENTITY_CLAIM_NAMESPACE => {
                let claim = decode_file_identity_claim_envelope_with_checkpoint(envelope, || {
                    context.check().is_ok()
                })
                .map_err(|error| map_claim_error(error, context))?;
                let source = envelope.evidence.source.clone().ok_or(
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    ),
                )?;
                if file_claims.insert(claim.file, (claim, source)).is_some() {
                    return Err(IdentityVerificationError::DuplicateClaim);
                }
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            SYMBOL_IDENTITY_CLAIM_NAMESPACE => {
                let claim = decode_symbol_identity_claim_envelope_with_checkpoint(envelope, || {
                    context.check().is_ok()
                })
                .map_err(|error| map_claim_error(error, context))?;
                let source = envelope.evidence.source.clone().ok_or(
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    ),
                )?;
                if symbol_claims
                    .insert(claim.symbol, (claim, source))
                    .is_some()
                {
                    return Err(IdentityVerificationError::DuplicateClaim);
                }
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            LEXICAL_EXTENSION_NAMESPACE => {
                if envelope.version == LEXICAL_SUMMARY_BATCH_VERSION {
                    let children =
                        decode_lexical_summary_batch_envelope(envelope).map_err(|_| {
                            IdentityVerificationError::IdentityMismatch(
                                IdentityMismatchComponent::LexicalEvidence,
                            )
                        })?;
                    let files = lexical_batch_files.get_or_insert_with(|| {
                        document
                            .files
                            .iter()
                            .map(|file| (file.id, file))
                            .collect::<BTreeMap<_, _>>()
                    });
                    for child in children {
                        context
                            .check()
                            .map_err(IdentityVerificationError::Control)?;
                        let source = child.evidence.source.as_ref().ok_or(
                            IdentityVerificationError::IdentityMismatch(
                                IdentityMismatchComponent::LexicalEvidence,
                            ),
                        )?;
                        let file = files.get(&source.span().file()).ok_or(
                            IdentityVerificationError::IdentityMismatch(
                                IdentityMismatchComponent::LexicalEvidence,
                            ),
                        )?;
                        if source.content_hash() != file.content_hash
                            || source.span().end_byte() > file.byte_length
                        {
                            return Err(IdentityVerificationError::IdentityMismatch(
                                IdentityMismatchComponent::LexicalEvidence,
                            ));
                        }
                    }
                    context
                        .check()
                        .map_err(IdentityVerificationError::Control)?;
                    continue;
                }
                let subject = envelope
                    .evidence
                    .derivation
                    .first()
                    .copied()
                    .filter(|_| envelope.evidence.derivation.len() == 1)
                    .ok_or(IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::LexicalEvidence,
                    ))?;
                let source = envelope.evidence.source.as_ref().ok_or(
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::LexicalEvidence,
                    ),
                )?;
                validate_lexical_evidence_envelope(envelope, subject, source, envelope.provenance)
                    .map_err(|_| {
                        IdentityVerificationError::IdentityMismatch(
                            IdentityMismatchComponent::LexicalEvidence,
                        )
                    })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::FSHARP_BINDING_NAMESPACE => {
                rootlight_ir::decode_fsharp_binding_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            PERL_BINDING_NAMESPACE => {
                decode_perl_binding_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            rootlight_ir::VBA_COMPILATION_NAMESPACE => {
                rootlight_ir::decode_vba_compilation_envelope(envelope).map_err(|_| {
                    IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope,
                    )
                })?;
                context
                    .check()
                    .map_err(IdentityVerificationError::Control)?;
            }
            _ => has_unsupported_extension = true,
        }
    }
    if file_claims.len() != document.files.len() || symbol_claims.len() != document.entities.len() {
        return Err(IdentityVerificationError::MissingClaim);
    }
    if has_unsupported_extension {
        return Err(IdentityVerificationError::UnsupportedExtension);
    }

    for file in &document.files {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let (claim, claim_source) = file_claims
            .get(&file.id)
            .ok_or(IdentityVerificationError::MissingClaim)?;
        if claim.repository != file.repository
            || claim.path != file.path
            || claim.content_hash != file.content_hash
            || claim.byte_length != file.byte_length
            || claim.derived_file() != file.id
            || file.evidence.source.as_ref() != Some(claim_source)
        {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::FileClaim,
            ));
        }
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
    }
    for entry in snapshot.source_files().entries() {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let file = entry.file();
        let source =
            file.evidence
                .source
                .clone()
                .ok_or(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::FileClaim,
                ))?;
        if file_claims
            .insert(file.id, (entry.identity_claim(), source))
            .is_some()
        {
            return Err(IdentityVerificationError::DuplicateClaim);
        }
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
    }
    context
        .check()
        .map_err(IdentityVerificationError::Control)?;
    let mut manifest_claims = Vec::with_capacity(file_claims.len());
    for (claim, _source) in file_claims.into_values() {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        manifest_claims.push(claim);
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
    }
    context
        .check()
        .map_err(IdentityVerificationError::Control)?;
    let manifest = GenerationManifestRecipe::new_with_context(
        document.repository,
        snapshot.metadata().configuration_hash(),
        manifest_claims,
        context,
    )?;
    context
        .check()
        .map_err(IdentityVerificationError::Control)?;
    if manifest.canonical_hash_with_context(context)? != snapshot.metadata().manifest_hash() {
        return Err(IdentityVerificationError::ManifestMismatch);
    }

    context
        .check()
        .map_err(IdentityVerificationError::Control)?;
    let mut provenance_by_id = BTreeMap::new();
    for record in &document.provenance {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        provenance_by_id.insert(record.id, record);
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
    }
    for entity in &document.entities {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let (claim, claim_source) = symbol_claims
            .get(&entity.id)
            .ok_or(IdentityVerificationError::MissingClaim)?;
        let provenance = provenance_by_id.get(&entity.provenance).ok_or(
            IdentityVerificationError::IdentityMismatch(IdentityMismatchComponent::SymbolClaim),
        )?;
        if claim.repository != entity.repository
            || claim.language != entity.language
            || claim.kind != entity.kind
            || claim.container != entity.container
            || claim.declared_identity != entity.canonical_name
            || !symbol_claim_source_matches(entity, provenance, claim_source)
            || claim.build_context_discriminator
                != provenance.build_context.digest().as_bytes().as_slice()
            || claim.derived_symbol() != entity.id
        {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::SymbolClaim,
            ));
        }
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
    }

    verify_fact_ids(document, context)
}

fn symbol_claim_source_matches(
    entity: &rootlight_ir::EntityRecord,
    provenance: &rootlight_ir::ProvenanceRecord,
    claim_source: &SourceRef,
) -> bool {
    if let Some(source) = &entity.evidence.source {
        return source == claim_source;
    }
    // A rule-derived external model has no repository definition. Its claim
    // must instead witness a retained input to the exact producer derivation;
    // ordinary entities may never substitute that anchor for a definition.
    entity.kind == rootlight_ir::EntityKind::ExternalSymbol
        && entity.flags.contains(&rootlight_ir::EntityFlag::External)
        && entity.flags.contains(&rootlight_ir::EntityFlag::Synthetic)
        && entity.container.is_none()
        && entity.evidence.derivation == [rootlight_ir::FactRef::Fact(provenance.id)]
        && provenance.producer_kind == rootlight_ir::ProducerKind::Rule
        && provenance
            .rule
            .as_ref()
            .is_some_and(|rule| !rule.is_empty())
        && provenance.evidence_sources.is_empty()
        && provenance.input_sources.contains(claim_source)
        && provenance
            .derivation_parents
            .contains(&rootlight_ir::FactRef::File(claim_source.span().file()))
}

fn map_claim_error(
    error: IdentityClaimError,
    context: &GenerationContext<'_>,
) -> IdentityVerificationError {
    if error == IdentityClaimError::Interrupted {
        match context.check() {
            Err(error) => IdentityVerificationError::Control(error),
            Ok(()) => IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::ClaimEnvelope,
            ),
        }
    } else {
        IdentityVerificationError::IdentityMismatch(IdentityMismatchComponent::ClaimEnvelope)
    }
}

fn map_fact_recipe_error(context: &GenerationContext<'_>) -> IdentityVerificationError {
    match context.check() {
        Err(error) => IdentityVerificationError::Control(error),
        Ok(()) => IdentityVerificationError::RecipeEncoding,
    }
}

fn verify_fact_ids(
    document: &NormalizedIrDocument,
    context: &GenerationContext<'_>,
) -> Result<(), IdentityVerificationError> {
    for record in &document.provenance {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived =
            derive_provenance_record_id_with_checkpoint(record, || context.check().is_ok())
                .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::Provenance,
            ));
        }
    }
    for record in &document.occurrences {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived =
            derive_occurrence_record_id_with_checkpoint(record, || context.check().is_ok())
                .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::Occurrence,
            ));
        }
    }
    for record in &document.relations {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived = derive_relation_record_id_with_checkpoint(record, || context.check().is_ok())
            .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::Relation,
            ));
        }
    }
    for record in &document.source_mappings {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived =
            derive_source_mapping_record_id_with_checkpoint(record, || context.check().is_ok())
                .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::SourceMapping,
            ));
        }
    }
    for record in &document.coverage_records {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived = derive_coverage_record_id_with_checkpoint(record, || context.check().is_ok())
            .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::Coverage,
            ));
        }
    }
    for record in &document.skipped_regions {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived = derive_skipped_region_id_with_checkpoint(record, || context.check().is_ok())
            .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::SkippedRegion,
            ));
        }
    }
    for record in &document.diagnostics {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        let derived =
            derive_diagnostic_record_id_with_checkpoint(record, || context.check().is_ok())
                .map_err(|_| map_fact_recipe_error(context))?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        if derived != record.id {
            return Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::Diagnostic,
            ));
        }
    }
    context.check().map_err(IdentityVerificationError::Control)
}

/// Logical and physical cardinalities for one sealed generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationStats {
    files: u64,
    entities: u64,
    occurrences: u64,
    relations: u64,
    provenance: u64,
    source_mappings: u64,
    coverage: u64,
    skipped_regions: u64,
    diagnostics: u64,
    extensions: u64,
    source_refs: u64,
    stored_rows: u64,
    text_bytes: u64,
}

impl GenerationStats {
    /// Creates checked generation cardinalities.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationValidationError::InvalidStatistics`] when the
    /// physical row count cannot contain the declared logical records.
    #[expect(
        clippy::too_many_arguments,
        reason = "the explicit counters are the versioned statistics contract"
    )]
    pub fn new(
        files: u64,
        entities: u64,
        occurrences: u64,
        relations: u64,
        provenance: u64,
        source_mappings: u64,
        coverage: u64,
        skipped_regions: u64,
        diagnostics: u64,
        extensions: u64,
        source_refs: u64,
        stored_rows: u64,
        text_bytes: u64,
    ) -> Result<Self, GenerationValidationError> {
        let minimum_rows = [
            2,
            files,
            entities,
            occurrences,
            relations,
            provenance,
            source_mappings,
            coverage,
            skipped_regions,
            diagnostics,
            extensions,
            source_refs,
        ]
        .into_iter()
        .try_fold(0_u64, u64::checked_add)
        .ok_or(GenerationValidationError::StatisticsOverflow)?;
        if stored_rows < minimum_rows {
            return Err(GenerationValidationError::InvalidStatistics);
        }
        Ok(Self {
            files,
            entities,
            occurrences,
            relations,
            provenance,
            source_mappings,
            coverage,
            skipped_regions,
            diagnostics,
            extensions,
            source_refs,
            stored_rows,
            text_bytes,
        })
    }

    /// Returns the file count.
    #[must_use]
    pub const fn files(self) -> u64 {
        self.files
    }

    /// Returns the entity count.
    #[must_use]
    pub const fn entities(self) -> u64 {
        self.entities
    }

    /// Returns the occurrence count.
    #[must_use]
    pub const fn occurrences(self) -> u64 {
        self.occurrences
    }

    /// Returns the relation count.
    #[must_use]
    pub const fn relations(self) -> u64 {
        self.relations
    }

    /// Returns the provenance-record count.
    #[must_use]
    pub const fn provenance(self) -> u64 {
        self.provenance
    }

    /// Returns the source-mapping count.
    #[must_use]
    pub const fn source_mappings(self) -> u64 {
        self.source_mappings
    }

    /// Returns the coverage-record count.
    #[must_use]
    pub const fn coverage(self) -> u64 {
        self.coverage
    }

    /// Returns the skipped-region count.
    #[must_use]
    pub const fn skipped_regions(self) -> u64 {
        self.skipped_regions
    }

    /// Returns the diagnostic count.
    #[must_use]
    pub const fn diagnostics(self) -> u64 {
        self.diagnostics
    }

    /// Returns the extension-envelope count.
    #[must_use]
    pub const fn extensions(self) -> u64 {
        self.extensions
    }

    /// Returns the distinct source-reference count.
    #[must_use]
    pub const fn source_refs(self) -> u64 {
        self.source_refs
    }

    /// Returns generation-owned rows, including its header, seal, and child rows.
    #[must_use]
    pub const fn stored_rows(self) -> u64 {
        self.stored_rows
    }

    /// Returns encoded UTF-8 bytes owned by the persisted record representation.
    #[must_use]
    pub const fn text_bytes(self) -> u64 {
        self.text_bytes
    }
}

/// Resources independently capped during generation persistence and reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GenerationResource {
    /// Physical rows inserted or materialized.
    Rows,
    /// Distinct generation-bound source references.
    SourceReferences,
    /// Dynamic UTF-8 bytes owned by records.
    TextBytes,
    /// Lossless locator and backend-encoded UTF-8 bytes.
    EncodedTextBytes,
}

impl GenerationResource {
    const fn hard_limit(self) -> u64 {
        match self {
            Self::Rows => HARD_MAX_GENERATION_ROWS,
            Self::SourceReferences => HARD_MAX_GENERATION_SOURCE_REFS,
            Self::TextBytes => HARD_MAX_GENERATION_TEXT_BYTES,
            Self::EncodedTextBytes => HARD_MAX_GENERATION_ENCODED_TEXT_BYTES,
        }
    }
}

/// Per-operation limits enforced before and during generation work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationBudget {
    max_rows: u64,
    max_source_refs: u64,
    max_text_bytes: u64,
}

impl GenerationBudget {
    /// Creates a nonzero budget within the contract hard ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationBudgetError`] when any limit is zero or exceeds its
    /// hard ceiling.
    pub fn new(
        max_rows: u64,
        max_source_refs: u64,
        max_text_bytes: u64,
    ) -> Result<Self, GenerationBudgetError> {
        for (resource, value) in [
            (GenerationResource::Rows, max_rows),
            (GenerationResource::SourceReferences, max_source_refs),
            (GenerationResource::TextBytes, max_text_bytes),
        ] {
            if value == 0 {
                return Err(GenerationBudgetError::Zero(resource));
            }
            let maximum = resource.hard_limit();
            if value > maximum {
                return Err(GenerationBudgetError::AboveHardLimit { resource, maximum });
            }
        }
        Ok(Self {
            max_rows,
            max_source_refs,
            max_text_bytes,
        })
    }

    /// Returns the limit for one resource.
    #[must_use]
    pub const fn limit(self, resource: GenerationResource) -> u64 {
        match resource {
            GenerationResource::Rows => self.max_rows,
            GenerationResource::SourceReferences => self.max_source_refs,
            GenerationResource::TextBytes => self.max_text_bytes,
            GenerationResource::EncodedTextBytes => {
                let expanded = self
                    .max_text_bytes
                    .saturating_mul(ENCODED_TEXT_EXPANSION_FACTOR);
                if expanded < HARD_MAX_GENERATION_ENCODED_TEXT_BYTES {
                    expanded
                } else {
                    HARD_MAX_GENERATION_ENCODED_TEXT_BYTES
                }
            }
        }
    }
}

impl Default for GenerationBudget {
    fn default() -> Self {
        Self {
            max_rows: DEFAULT_MAX_GENERATION_ROWS,
            max_source_refs: DEFAULT_MAX_GENERATION_SOURCE_REFS,
            max_text_bytes: DEFAULT_MAX_GENERATION_TEXT_BYTES,
        }
    }
}

/// Cancellation and resource policy for one synchronous generation operation.
#[derive(Debug, Clone, Copy)]
pub struct GenerationContext<'a> {
    cancellation: &'a Cancellation,
    budget: GenerationBudget,
}

impl<'a> GenerationContext<'a> {
    /// Binds a cancellation token and a validated generation budget.
    #[must_use]
    pub const fn new(cancellation: &'a Cancellation, budget: GenerationBudget) -> Self {
        Self {
            cancellation,
            budget,
        }
    }

    /// Checks cooperative cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationControlError::Cancelled`] after cancellation or
    /// monotonic deadline expiry.
    pub fn check(self) -> Result<(), GenerationControlError> {
        self.cancellation
            .check()
            .map_err(|cancelled| GenerationControlError::Cancelled {
                reason: cancelled.reason(),
            })
    }

    /// Checks cancellation and one observed amount against its operation limit.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationControlError::BudgetExceeded`] when `observed`
    /// exceeds the selected resource limit.
    pub fn require(
        self,
        resource: GenerationResource,
        observed: u64,
    ) -> Result<(), GenerationControlError> {
        self.check()?;
        let limit = self.budget.limit(resource);
        if observed > limit {
            Err(GenerationControlError::BudgetExceeded {
                resource,
                observed,
                limit,
            })
        } else {
            Ok(())
        }
    }

    /// Returns the validated operation budget.
    #[must_use]
    pub const fn budget(self) -> GenerationBudget {
        self.budget
    }

    /// Returns the shared token for backend-native cancellation hooks.
    #[must_use]
    pub const fn cancellation(self) -> &'a Cancellation {
        self.cancellation
    }
}

/// A backend-neutral writer that consumes one identity-verified generation.
pub trait GenerationWriter {
    /// Typed backend error with a source-redacted display contract.
    type Error: Error + Send + Sync + 'static;

    /// Writes and seals one owned generation within the supplied limits.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, contention, or storage failures.
    fn write_generation(
        self: Box<Self>,
        generation: IdentityVerifiedGeneration,
        context: &GenerationContext<'_>,
    ) -> Result<GenerationStats, Self::Error>;
}

/// Invalid caller-supplied generation budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GenerationBudgetError {
    /// A resource limit was zero.
    #[error("generation budget limit must be positive")]
    Zero(GenerationResource),
    /// A resource limit exceeded the contract hard ceiling.
    #[error("generation budget exceeds the contract hard limit")]
    AboveHardLimit {
        /// Resource whose ceiling was exceeded.
        resource: GenerationResource,
        /// Maximum accepted value.
        maximum: u64,
    },
}

/// Cooperative stop returned by generation context checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GenerationControlError {
    /// Cancellation or a monotonic deadline stopped work.
    #[error("generation work was cancelled")]
    Cancelled {
        /// Stable first-writer cancellation reason.
        reason: CancellationReason,
    },
    /// A declared operation budget was exhausted.
    #[error("generation work exceeded its resource budget")]
    BudgetExceeded {
        /// Exhausted resource family.
        resource: GenerationResource,
        /// Observed resource use, or [`u64::MAX`] after arithmetic overflow.
        observed: u64,
        /// Configured operation limit.
        limit: u64,
    },
}

/// Failure to grant the opaque identity-verified generation capability.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityVerificationError {
    /// Cooperative cancellation or a resource policy stopped verification.
    #[error("generation identity verification was interrupted")]
    Control(#[source] GenerationControlError),
    /// Snapshot workspace could not be represented or reserved.
    #[error("generation snapshot workspace is unavailable")]
    ResourceUnavailable,
    /// Snapshot construction failed before identity verification.
    #[error("generation is not a valid canonical snapshot")]
    InvalidGeneration,
    /// Only the current recipe-bearing generation contract can be verified.
    #[error("legacy generation contracts do not carry complete identity claims")]
    LegacyContract,
    /// A required file or symbol claim is absent or an extra claim exists.
    #[error("generation identity claims are incomplete")]
    MissingClaim,
    /// More than one claim was supplied for one stable identity.
    #[error("generation contains duplicate identity claims")]
    DuplicateClaim,
    /// A stable ID differs from the result of its canonical recipe.
    #[error("generation {0:?} identity does not match its canonical recipe")]
    IdentityMismatch(IdentityMismatchComponent),
    /// Canonical manifest inputs do not match generation metadata.
    #[error("generation manifest does not match its canonical file inputs")]
    ManifestMismatch,
    /// A non-core extension does not expose a shared identity recipe.
    #[error("generation contains an extension without a verifiable identity recipe")]
    UnsupportedExtension,
    /// A fixed typed recipe could not be encoded.
    #[error("generation identity recipe could not be encoded")]
    RecipeEncoding,
}

/// Failure while projecting a verified generation into logical identity digests.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationNeutralDigestError {
    /// Cooperative cancellation or a resource policy stopped projection.
    #[error("generation logical projection was interrupted")]
    Control(#[source] GenerationControlError),
    /// The verified snapshot could not be projected through current identity recipes.
    #[error("generation logical projection is invalid")]
    InvalidGeneration,
    /// The checked bounded workspace could not be reserved.
    #[error("generation logical projection workspace is unavailable")]
    ResourceUnavailable,
}

/// Closed record family identifying a canonical identity mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityMismatchComponent {
    /// An identity-claim envelope is malformed or not bound to direct evidence.
    ClaimEnvelope,
    /// A lexical evidence extension is not bound to its asserted subject.
    LexicalEvidence,
    /// A file record disagrees with its canonical identity claim.
    FileClaim,
    /// An entity record disagrees with its canonical symbol claim or provenance.
    SymbolClaim,
    /// A provenance record ID differs from its typed recipe.
    Provenance,
    /// An occurrence record ID differs from its typed recipe.
    Occurrence,
    /// A relation record ID differs from its typed recipe.
    Relation,
    /// A source-mapping record ID differs from its typed recipe.
    SourceMapping,
    /// A coverage record ID differs from its typed recipe.
    Coverage,
    /// A skipped-region record ID differs from its typed recipe.
    SkippedRegion,
    /// A diagnostic record ID differs from its typed recipe.
    Diagnostic,
}

impl IdentityMismatchComponent {
    /// Returns the stable source-free label used by operational diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ClaimEnvelope => "claim_envelope",
            Self::LexicalEvidence => "lexical_evidence",
            Self::FileClaim => "file_claim",
            Self::SymbolClaim => "symbol_claim",
            Self::Provenance => "provenance",
            Self::Occurrence => "occurrence",
            Self::Relation => "relation",
            Self::SourceMapping => "source_mapping",
            Self::Coverage => "coverage",
            Self::SkippedRegion => "skipped_region",
            Self::Diagnostic => "diagnostic",
        }
    }
}

impl IdentityVerificationError {
    fn from_snapshot(error: GenerationSnapshotError) -> Self {
        match error {
            GenerationSnapshotError::Control(error) => Self::Control(error),
            GenerationSnapshotError::Validation(
                GenerationValidationError::IndexResourceUnavailable,
            ) => Self::ResourceUnavailable,
            GenerationSnapshotError::Validation(_) => Self::InvalidGeneration,
        }
    }
}

/// Failure while contextually constructing a canonical generation snapshot.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationSnapshotError {
    /// Cooperative cancellation or a resource policy stopped work.
    #[error("generation snapshot construction was interrupted")]
    Control(#[source] GenerationControlError),
    /// Metadata or normalized IR validation failed.
    #[error("generation snapshot is invalid")]
    Validation(#[source] GenerationValidationError),
}

/// Invalid generation metadata or normalized IR.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationValidationError {
    /// Persisted metadata selected an unsupported logical contract version.
    #[error("generation contract version is unsupported")]
    UnsupportedContractVersion,
    /// A generation named itself as its parent.
    #[error("generation parent cannot equal the generation")]
    SelfParent,
    /// The generation identifier was not derived from all metadata inputs.
    #[error("generation identity does not match its semantic inputs")]
    GenerationIdentityMismatch,
    /// Document repository or generation differed from metadata.
    #[error("generation document ownership does not match metadata")]
    OwnershipMismatch,
    /// Normalized IR failed its existing bounded validation contract.
    #[error("generation normalized IR is invalid")]
    InvalidIr(#[source] IrDocumentValidationError),
    /// File-only retrieval records failed their exact source contract.
    #[error("generation source file catalog is invalid")]
    InvalidSourceCatalog(#[source] SourceFileCatalogError),
    /// Checked derived-index workspace could not be represented or reserved.
    #[error("generation derived index workspace is unavailable")]
    IndexResourceUnavailable,
    /// Cardinality addition overflowed.
    #[error("generation statistics overflowed")]
    StatisticsOverflow,
    /// Physical rows could not contain the declared logical records.
    #[error("generation statistics are inconsistent")]
    InvalidStatistics,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{
        FactId, FileId, FileIdentity, GenerationId, GenerationIdentity, RepositoryId, content_hash,
        derive_file, derive_generation,
    };
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, FactEvidence, FilePathLocator, FilePathLocatorEncoding,
        ProducerIdentity, ProducerKind, ProvenanceRecord, SourceSpan, derive_provenance_record_id,
        new_file_identity_claim_envelope,
    };
    use serde::ser::SerializeSeq;

    fn metadata(repository: RepositoryId) -> GenerationMetadata {
        let manifest_hash = content_hash(b"manifest");
        let configuration_hash = content_hash(b"configuration");
        let provider_set_hash = content_hash(b"providers");
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version: generation_format_version(GENERATION_CONTRACT_VERSION),
        })
        .id();
        GenerationMetadata::new(
            repository,
            generation,
            None,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        )
        .expect("fixture metadata is valid")
    }

    fn verified_empty_metadata(repository: RepositoryId) -> GenerationMetadata {
        let configuration_hash = content_hash(b"configuration");
        let manifest_hash =
            GenerationManifestRecipe::new(repository, configuration_hash, Vec::new())
                .expect("empty manifest recipe is valid")
                .canonical_hash()
                .expect("empty manifest recipe is encodable");
        let provider_set_hash = content_hash(b"providers");
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version: generation_format_version(GENERATION_CONTRACT_VERSION),
        })
        .id();
        GenerationMetadata::new(
            repository,
            generation,
            None,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        )
        .expect("verified fixture metadata is valid")
    }

    fn file_only_generation(
        bind_manifest_claim: bool,
    ) -> (GenerationMetadata, NormalizedIrDocument, SourceFileCatalog) {
        let repository = RepositoryId::from_bytes([4; 16]);
        let configuration_hash = content_hash(b"configuration");
        let provider_set_hash = content_hash(b"providers");
        let path = "src/file.txt";
        let path_identity = b"src/file.txt".to_vec();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: &path_identity,
        })
        .id();
        let file_content_hash = content_hash(b"source");
        let claim = FileIdentityClaim {
            file,
            repository,
            path: path.to_owned(),
            path_identity,
            content_hash: file_content_hash,
            byte_length: 6,
        };
        let anchor_path = "src/anchor.txt";
        let anchor_path_identity = b"src/anchor.txt".to_vec();
        let anchor_file = derive_file(FileIdentity {
            repository,
            path_identity: &anchor_path_identity,
        })
        .id();
        let anchor_content_hash = content_hash(b"anchor");
        let anchor_claim = FileIdentityClaim {
            file: anchor_file,
            repository,
            path: anchor_path.to_owned(),
            path_identity: anchor_path_identity,
            content_hash: anchor_content_hash,
            byte_length: 6,
        };
        let mut manifest_claims = vec![anchor_claim.clone()];
        if bind_manifest_claim {
            manifest_claims.push(claim.clone());
        }
        let manifest_hash =
            GenerationManifestRecipe::new(repository, configuration_hash, manifest_claims)
                .expect("fixture manifest is valid")
                .canonical_hash()
                .expect("fixture manifest is encodable");
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version: generation_format_version(GENERATION_CONTRACT_VERSION),
        })
        .id();
        let metadata = GenerationMetadata::new(
            repository,
            generation,
            None,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        )
        .expect("fixture metadata is valid");
        let anchor_source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(anchor_file, 0, 6).expect("fixture span is valid"),
            anchor_content_hash,
            None,
        );
        let mut provenance = ProvenanceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            producer_kind: ProducerKind::Rule,
            producer: ProducerIdentity::new("fixture-file-fallback", "1.0", configuration_hash)
                .expect("fixture producer is valid"),
            binary_digest: content_hash(b"fixture-binary"),
            frontend_version: Some("fixture-1".to_owned()),
            language: "text".to_owned(),
            tier: AnalysisTier::TierD,
            build_context: BuildContextIdentity::new(configuration_hash),
            input_sources: vec![anchor_source.clone()],
            evidence_sources: vec![anchor_source.clone()],
            derivation_parents: Vec::new(),
            rule: Some("file-only-fallback".to_owned()),
        };
        provenance.id =
            derive_provenance_record_id(&provenance).expect("fixture provenance derives");
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 6).expect("fixture span is valid"),
            file_content_hash,
            None,
        );
        let record = FileRecord {
            id: file,
            repository,
            generation,
            path: path.to_owned(),
            path_locator: Some(
                FilePathLocator::new(FilePathLocatorEncoding::UnixBytesV1, vec!["61".to_owned()])
                    .expect("fixture locator is valid"),
            ),
            content_hash: file_content_hash,
            byte_length: 6,
            language: "text".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: provenance.id,
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        let catalog = SourceFileCatalog::new(vec![
            crate::SourceFileCatalogEntry::new(record, claim)
                .expect("fixture catalog entry is valid"),
        ])
        .expect("fixture catalog is valid");
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document.files.push(FileRecord {
            id: anchor_file,
            repository,
            generation,
            path: anchor_path.to_owned(),
            path_locator: Some(
                FilePathLocator::new(FilePathLocatorEncoding::UnixBytesV1, vec!["62".to_owned()])
                    .expect("fixture locator is valid"),
            ),
            content_hash: anchor_content_hash,
            byte_length: 6,
            language: "text".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: provenance.id,
            evidence: FactEvidence {
                source: Some(anchor_source.clone()),
                derivation: Vec::new(),
            },
        });
        document.provenance.push(provenance);
        document.extensions.push(
            new_file_identity_claim_envelope(
                &anchor_claim,
                generation,
                document.provenance[0].id,
                anchor_source,
            )
            .expect("fixture file claim envelope is valid"),
        );
        (metadata, document, catalog)
    }

    struct CancelDuringSerialization<'a> {
        cancellation: &'a Cancellation,
    }

    impl Serialize for CancelDuringSerialization<'_> {
        fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&"x".repeat(2 * IDENTITY_RECIPE_CHECKPOINT_BYTES))?;
            self.cancellation.cancel(CancellationReason::ClientRequest);
            sequence.serialize_element("unreachable after the next writer checkpoint")?;
            sequence.end()
        }
    }

    #[test]
    fn snapshot_binds_canonical_ir_to_metadata() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = metadata(repository);
        let generation = metadata.generation();
        let snapshot = GenerationSnapshot::new(
            metadata,
            NormalizedIrDocument::empty(repository, generation),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("empty normalized fixture is valid");

        assert_eq!(snapshot.metadata().generation(), generation);
        assert_eq!(snapshot.document().repository, repository);
    }

    fn symbol_posting_fixture() -> (GenerationMetadata, NormalizedIrDocument, SymbolId, SymbolId) {
        let (metadata, mut document, catalog) = file_only_generation(true);
        document.files.push(catalog.entries()[0].file().clone());
        let first = SymbolId::from_bytes([7; 20]);
        let second = SymbolId::from_bytes([8; 20]);
        let source = document.files[0].evidence.source.clone().unwrap();
        let other_source = document.files[1].evidence.source.clone().unwrap();
        let provenance = document.provenance[0].id;
        for (id, name) in [(second, "second"), (first, "first")] {
            document.entities.push(rootlight_ir::EntityRecord {
                id,
                repository: document.repository,
                generation: document.generation,
                kind: rootlight_ir::EntityKind::Function,
                language: "text".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(rootlight_ir::ContainerRef::File(source.span().file())),
                visibility: rootlight_ir::EntityVisibility::Public,
                flags: Vec::new(),
                provenance,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            });
        }
        for (target, source) in [
            (OccurrenceTarget::Resolved { symbol: first }, source.clone()),
            (
                OccurrenceTarget::Candidates {
                    symbols: vec![second, first],
                    total_count: 2,
                    completeness: rootlight_ir::CoverageStatus::Complete,
                },
                other_source.clone(),
            ),
            (
                OccurrenceTarget::Unresolved {
                    text_hash: content_hash(b"unknown"),
                },
                source.clone(),
            ),
        ] {
            let mut occurrence = rootlight_ir::OccurrenceRecord {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                file: source.span().file(),
                source: source.clone(),
                role: rootlight_ir::OccurrenceRole::Read,
                enclosing: Some(first),
                target,
                syntactic_text_hash: content_hash(b"anchor"),
                syntax_kind: "fixture.read".to_owned(),
                provenance,
                confidence: rootlight_ir::Confidence::new(1000).unwrap(),
                evidence: FactEvidence {
                    source: Some(source),
                    derivation: Vec::new(),
                },
            };
            occurrence.id = rootlight_ir::derive_occurrence_record_id(&occurrence).unwrap();
            document.occurrences.push(occurrence);
        }
        let read = document.occurrences[0].id;
        for (subject, object) in [
            (
                RelationEndpoint::Entity(first),
                RelationEndpoint::Entity(second),
            ),
            (
                RelationEndpoint::Entity(first),
                RelationEndpoint::Entity(first),
            ),
            (
                RelationEndpoint::Occurrence(read),
                RelationEndpoint::Entity(second),
            ),
        ] {
            let mut relation = rootlight_ir::RelationRecord {
                id: FactId::from_bytes([0; 20]),
                repository: document.repository,
                generation: document.generation,
                subject,
                predicate: rootlight_ir::RelationPredicate::RefersTo,
                object,
                confidence: rootlight_ir::Confidence::new(1000).unwrap(),
                evidence_kind: rootlight_ir::EvidenceKind::Syntax,
                provenance,
                evidence: FactEvidence {
                    source: Some(source.clone()),
                    derivation: Vec::new(),
                },
            };
            relation.id = rootlight_ir::derive_relation_record_id(&relation).unwrap();
            document.relations.push(relation);
        }
        for (kind, subject, text, source) in [
            (
                rootlight_ir::LexicalEvidenceKind::Signature,
                FactRef::Entity(first),
                "anchor",
                source.clone(),
            ),
            (
                rootlight_ir::LexicalEvidenceKind::Signature,
                FactRef::Entity(first),
                "source",
                other_source,
            ),
            (
                rootlight_ir::LexicalEvidenceKind::CommentSummary,
                FactRef::Entity(second),
                "anchor",
                source.clone(),
            ),
            (
                rootlight_ir::LexicalEvidenceKind::CommentSummary,
                FactRef::File(source.span().file()),
                "anchor",
                source,
            ),
        ] {
            let format = if kind == rootlight_ir::LexicalEvidenceKind::Signature {
                rootlight_ir::LexicalEvidenceFormat::SourceText
            } else {
                rootlight_ir::LexicalEvidenceFormat::PlainText
            };
            let evidence =
                rootlight_ir::LexicalEvidenceV1::from_complete_text(kind, subject, format, text)
                    .unwrap();
            document.extensions.push(
                rootlight_ir::new_lexical_evidence_envelope(
                    document.repository,
                    document.generation,
                    provenance,
                    source,
                    &evidence,
                )
                .unwrap(),
            );
        }
        (metadata, document, first, second)
    }

    #[test]
    fn symbol_postings_preserve_canonical_direct_endpoint_and_target_ranges() {
        let (metadata, document, first, second) = symbol_posting_fixture();
        let canonical = CanonicalNormalizedIrDocument::new(
            document.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        let snapshot = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_vec(snapshot.document()).unwrap(),
            serde_json::to_vec(canonical.document()).unwrap(),
        );
        for symbol in [first, second] {
            let expected_relations = snapshot
                .document()
                .relations
                .iter()
                .enumerate()
                .filter_map(|(ordinal, relation)| {
                    (relation.subject == RelationEndpoint::Entity(symbol)
                        || relation.object == RelationEndpoint::Entity(symbol))
                    .then_some(ordinal)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                snapshot.relation_ordinals_for_symbol(symbol),
                expected_relations
            );
            let expected_occurrences = snapshot
                .document()
                .occurrences
                .iter()
                .enumerate()
                .filter_map(|(ordinal, occurrence)| {
                    let matches = match &occurrence.target {
                        OccurrenceTarget::Resolved { symbol: target } => *target == symbol,
                        OccurrenceTarget::Candidates { symbols, .. } => symbols.contains(&symbol),
                        OccurrenceTarget::Unresolved { .. } => false,
                    };
                    matches.then_some(ordinal)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                snapshot.occurrence_ordinals_for_symbol(symbol),
                expected_occurrences
            );
        }
        assert_eq!(snapshot.relation_ordinals_for_symbol(first).len(), 2);
        assert_eq!(snapshot.relation_ordinals_for_symbol(second).len(), 2);
        assert_eq!(snapshot.occurrence_ordinals_for_symbol(first).len(), 2);
        assert_eq!(snapshot.occurrence_ordinals_for_symbol(second).len(), 1);
        let candidate =
            &snapshot.document().occurrences[snapshot.occurrence_ordinals_for_symbol(second)[0]];
        assert!(matches!(
            &candidate.target,
            OccurrenceTarget::Candidates { .. }
        ));
        assert_ne!(
            candidate.file,
            snapshot.document().entities[0]
                .evidence
                .source
                .as_ref()
                .unwrap()
                .span()
                .file()
        );
        let missing = SymbolId::from_bytes([9; 20]);
        assert!(snapshot.relation_ordinals_for_symbol(missing).is_empty());
        assert!(snapshot.occurrence_ordinals_for_symbol(missing).is_empty());
        assert!(snapshot.lexical_ordinals_for_symbol(missing).is_empty());
    }

    #[test]
    fn lexical_postings_preserve_all_exact_entity_envelopes_without_kind_inference() {
        let (metadata, document, first, second) = symbol_posting_fixture();
        let snapshot = GenerationSnapshot::new(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .unwrap();
        assert_eq!(snapshot.lexical_ordinals_for_symbol(first).len(), 2);
        assert_eq!(snapshot.lexical_ordinals_for_symbol(second).len(), 1);
        let ordinal = snapshot.lexical_ordinals_for_symbol(second)[0];
        let original = &snapshot.document().extensions[ordinal];
        assert_eq!(
            rootlight_ir::decode_lexical_evidence_envelope(original)
                .unwrap()
                .kind(),
            rootlight_ir::LexicalEvidenceKind::CommentSummary,
        );
        for mutate in 0..4 {
            let mut envelope = original.clone();
            match mutate {
                0 => envelope.namespace = "fixture.other".to_owned(),
                1 => envelope.version = LEXICAL_SUMMARY_BATCH_VERSION.to_owned(),
                2 => envelope.evidence.derivation.clear(),
                3 => envelope.evidence.derivation.push(FactRef::Entity(first)),
                _ => unreachable!(),
            }
            assert_eq!(lexical_entity_owner(&envelope), None);
        }
        for symbol in [first, second] {
            assert!(
                snapshot
                    .lexical_ordinals_for_symbol(symbol)
                    .windows(2)
                    .all(|pair| pair[0] < pair[1])
            );
        }
    }

    #[test]
    fn posting_heap_sort_matches_canonical_order_and_checks_mid_sort_cancellation() {
        for length in 0..65 {
            let mut values = (0..length)
                .map(|index| (index * 17 + 3) % 11)
                .collect::<Vec<_>>();
            let mut expected = values.clone();
            expected.sort_unstable();
            sort_index_with_checkpoint(&mut values, |value| *value, &mut || {
                Ok::<(), Infallible>(())
            })
            .unwrap();
            assert_eq!(values, expected);
        }
        let mut checkpoints = 0;
        let mut values = (0..128).rev().collect::<Vec<_>>();
        let result = sort_index_with_checkpoint(&mut values, |value| *value, &mut || {
            checkpoints += 1;
            if checkpoints == 7 {
                Err("cancelled")
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            result,
            Err(IndexConstructionError::Checkpoint("cancelled"))
        ));
        assert_eq!(checkpoints, 7);
    }

    #[test]
    fn posting_memory_estimate_covers_retained_capacities_and_existing_row_limits() {
        let (metadata, document, _, _) = symbol_posting_fixture();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let estimate =
            GenerationSnapshot::estimate_symbol_explain_index_memory_bytes(&document, &context)
                .unwrap();
        let snapshot = GenerationSnapshot::new_with_context(
            metadata,
            document.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .unwrap();
        assert!(snapshot.retained_symbol_explain_index_bytes() > 0);
        assert!(snapshot.retained_symbol_explain_index_bytes() <= estimate);
        assert_eq!(
            snapshot.retained_symbol_explain_index_bytes(),
            snapshot
                .symbol_explain_indexes
                .relations
                .retained_bytes()
                .unwrap()
                + snapshot
                    .symbol_explain_indexes
                    .occurrences
                    .retained_bytes()
                    .unwrap()
                + snapshot
                    .symbol_explain_indexes
                    .lexical
                    .retained_bytes()
                    .unwrap(),
        );
        let reduced = GenerationContext::new(
            &cancellation,
            GenerationBudget::new(
                1,
                HARD_MAX_GENERATION_SOURCE_REFS,
                HARD_MAX_GENERATION_TEXT_BYTES,
            )
            .unwrap(),
        );
        assert!(matches!(
            GenerationSnapshot::estimate_symbol_explain_index_memory_bytes(&document, &reduced),
            Err(GenerationSnapshotError::Control(
                GenerationControlError::BudgetExceeded {
                    resource: GenerationResource::Rows,
                    ..
                }
            )),
        ));
        assert_eq!(
            index_capacity_bytes::<(SymbolId, usize)>(usize::MAX),
            Err(GenerationValidationError::IndexResourceUnavailable),
        );
        let empty = NormalizedIrDocument::empty(document.repository, document.generation);
        assert_eq!(
            GenerationSnapshot::estimate_symbol_explain_index_memory_bytes(&empty, &context)
                .unwrap(),
            0
        );
    }

    #[test]
    fn posting_build_and_estimate_honor_context_and_nested_candidate_checkpoints() {
        let (metadata, document, _, _) = symbol_posting_fixture();
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for result in [
            GenerationSnapshot::estimate_symbol_explain_index_memory_bytes(&document, &context)
                .map(|_| ()),
            GenerationSnapshot::new_with_context(
                metadata,
                document.clone(),
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &context,
            )
            .map(|_| ()),
        ] {
            assert!(matches!(
                result,
                Err(GenerationSnapshotError::Control(
                    GenerationControlError::Cancelled {
                        reason: CancellationReason::ClientRequest
                    },
                ))
            ));
        }
        let mut count_checkpoints = 0;
        SymbolPostingCounts::count(&document, &mut || {
            count_checkpoints += 1;
            Ok::<(), Infallible>(())
        })
        .unwrap();
        let stop = count_checkpoints + document.relations.len() + 2;
        let mut build_checkpoints = 0;
        let result = SymbolExplainIndexes::build(&document, &mut || {
            build_checkpoints += 1;
            if build_checkpoints == stop {
                Err("build cancelled")
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            result,
            Err(IndexConstructionError::Checkpoint("build cancelled"))
        ));
        assert_eq!(build_checkpoints, stop);
        let mut candidate_document = document;
        candidate_document.relations.clear();
        candidate_document.extensions.clear();
        candidate_document
            .occurrences
            .retain(|record| matches!(&record.target, OccurrenceTarget::Candidates { .. }));
        let mut calls = 0;
        let result = SymbolPostingCounts::count(&candidate_document, &mut || {
            calls += 1;
            if calls == 3 {
                Err("candidate cancelled")
            } else {
                Ok(())
            }
        });
        assert!(matches!(
            result,
            Err(IndexConstructionError::Checkpoint("candidate cancelled"))
        ));
        assert_eq!(calls, 3);
    }

    #[test]
    fn snapshot_workspace_failure_is_not_identity_corruption() {
        assert_eq!(
            IdentityVerificationError::from_snapshot(GenerationSnapshotError::Validation(
                GenerationValidationError::IndexResourceUnavailable,
            )),
            IdentityVerificationError::ResourceUnavailable,
        );
        let cancelled = GenerationControlError::Cancelled {
            reason: CancellationReason::ClientRequest,
        };
        assert_eq!(
            IdentityVerificationError::from_snapshot(GenerationSnapshotError::Control(cancelled)),
            IdentityVerificationError::Control(cancelled),
        );
        assert_eq!(
            IdentityVerificationError::from_snapshot(GenerationSnapshotError::Validation(
                GenerationValidationError::OwnershipMismatch,
            )),
            IdentityVerificationError::InvalidGeneration,
        );
    }

    #[test]
    fn lexical_batch_verification_checks_each_hidden_source() {
        let (metadata, document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let source = file.evidence.source.clone().unwrap();
        let lexical = |source: SourceRef, text: &str| {
            let evidence = rootlight_ir::LexicalEvidenceV1::from_complete_text(
                rootlight_ir::LexicalEvidenceKind::CommentSummary,
                rootlight_ir::FactRef::File(file.id),
                rootlight_ir::LexicalEvidenceFormat::PlainText,
                text,
            )
            .unwrap();
            rootlight_ir::new_lexical_evidence_envelope(
                document.repository,
                document.generation,
                file.provenance,
                source,
                &evidence,
            )
            .unwrap()
        };
        let good = lexical(source.clone(), "source-linked comment");
        let wrong_source = SourceRef::new(
            document.repository,
            document.generation,
            source.span(),
            rootlight_ids::content_hash(b"different content"),
            None,
        );
        let bad = (0..100)
            .map(|index| lexical(wrong_source.clone(), &format!("hidden comment {index}")))
            .find(|candidate| good.id < candidate.id)
            .expect("fixture finds a later child identity");
        let mut document = document;
        document
            .extensions
            .push(rootlight_ir::new_lexical_summary_batch_envelope(&[good, bad]).unwrap());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let snapshot = GenerationSnapshot::new_with_source_files(
            metadata,
            document,
            catalog,
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("outer source alone passes generic IR validation");
        assert_eq!(
            IdentityVerifiedGeneration::verify_snapshot(snapshot, &context),
            Err(IdentityVerificationError::IdentityMismatch(
                IdentityMismatchComponent::LexicalEvidence
            ))
        );
    }

    #[test]
    fn snapshot_rejects_mismatched_generation() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = metadata(repository);
        let result = GenerationSnapshot::new(
            metadata,
            NormalizedIrDocument::empty(repository, GenerationId::from_bytes([3; 20])),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        );

        assert_eq!(result, Err(GenerationValidationError::OwnershipMismatch));
    }

    #[test]
    fn file_only_catalog_participates_in_manifest_identity_and_lookup() {
        let (metadata, document, catalog) = file_only_generation(true);
        let file = catalog.entries()[0].file().id;
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let verified = IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document,
            catalog,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .expect("file-only catalog verifies");

        assert_eq!(verified.snapshot().document().files.len(), 1);
        assert_eq!(verified.snapshot().file_count(), 2);
        assert_eq!(
            verified.snapshot().find_file(file).map(|record| record.id),
            Some(file)
        );

        let (metadata, document, catalog) = file_only_generation(false);
        assert_eq!(
            IdentityVerifiedGeneration::verify_with_source_files(
                metadata,
                document,
                catalog,
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &context,
            ),
            Err(IdentityVerificationError::ManifestMismatch)
        );
    }

    #[test]
    fn vba_compilation_identity_is_verified_before_snapshot_admission() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let scope = SourceSpan::new(file.id, 0, file.byte_length).unwrap();
        let region = SourceSpan::new(file.id, 1, file.byte_length - 1).unwrap();
        let source = SourceRef::new(
            document.repository,
            document.generation,
            scope,
            file.content_hash,
            None,
        );
        let rule_provenance = file.provenance;
        let mut parser = document.provenance[0].clone();
        parser.producer_kind = ProducerKind::Parser;
        parser.language = "vba".into();
        parser.rule = None;
        parser.id = derive_provenance_record_id(&parser).unwrap();
        let parser_id = parser.id;
        document.provenance.push(parser);
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let preserved = ExtensionSupport::default();
        let mut skipped = preserved.clone();
        skipped.unknown_noncritical = rootlight_ir::UnknownNoncriticalExtensionPolicy::Skip;

        for complete in [true, false] {
            let compilation = rootlight_ir::VbaCompilation {
                scope,
                complete,
                branches: vec![rootlight_ir::VbaCompilationBranch {
                    group: scope,
                    region,
                    activity: rootlight_ir::VbaCompilationActivity::Active,
                }],
            };
            let envelope = rootlight_ir::new_vba_compilation_envelope(
                document.repository,
                document.generation,
                parser_id,
                source.clone(),
                compilation.clone(),
            )
            .unwrap();
            let mut valid = document.clone();
            valid.extensions.push(envelope.clone());
            let verified = IdentityVerifiedGeneration::verify_with_source_files(
                metadata,
                valid.clone(),
                catalog.clone(),
                &IrLimits::default(),
                &preserved,
                &context,
            )
            .expect("canonical compilation identity verifies without asserting language semantics");
            assert!(verified.document().extensions.contains(&envelope));

            for mutate in [
                |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
                |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
                |value: &mut rootlight_ir::ExtensionEnvelope| {
                    value.id = FactId::from_bytes([99; 20])
                },
                |value: &mut rootlight_ir::ExtensionEnvelope| {
                    value.criticality = rootlight_ir::ExtensionCriticality::Critical
                },
            ] {
                let mut malformed = envelope.clone();
                mutate(&mut malformed);
                for support in [&preserved, &skipped] {
                    let mut changed = document.clone();
                    changed.extensions.push(malformed.clone());
                    assert_eq!(
                        IdentityVerifiedGeneration::verify_with_source_files(
                            metadata,
                            changed,
                            catalog.clone(),
                            &IrLimits::default(),
                            support,
                            &context,
                        ),
                        Err(IdentityVerificationError::InvalidGeneration)
                    );
                }
            }

            let unbound = rootlight_ir::new_vba_compilation_envelope(
                document.repository,
                document.generation,
                rule_provenance,
                source.clone(),
                compilation,
            )
            .unwrap();
            for support in [&preserved, &skipped] {
                let mut changed = document.clone();
                changed.extensions.push(unbound.clone());
                assert_eq!(
                    IdentityVerifiedGeneration::verify_with_source_files(
                        metadata,
                        changed,
                        catalog.clone(),
                        &IrLimits::default(),
                        support,
                        &context,
                    ),
                    Err(IdentityVerificationError::InvalidGeneration)
                );
            }
        }
    }

    #[test]
    fn fsharp_binding_identity_is_verified_before_snapshot_admission() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let source = file.evidence.source.clone().unwrap();
        let envelope = rootlight_ir::new_fsharp_binding_envelope(
            document.repository,
            document.generation,
            file.provenance,
            source.clone(),
            rootlight_ir::FSharpBindingEvidence::new(
                source.span(),
                rootlight_ir::FSharpBinding::Context,
            ),
        )
        .unwrap();
        document.extensions.push(envelope.clone());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document.clone(),
            catalog.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .unwrap();
        for mutate in [
            |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
        ] {
            let mut changed = document.clone();
            mutate(
                changed
                    .extensions
                    .iter_mut()
                    .find(|item| item.id == envelope.id)
                    .unwrap(),
            );
            assert_eq!(
                IdentityVerifiedGeneration::verify_with_source_files(
                    metadata,
                    changed,
                    catalog.clone(),
                    &IrLimits::default(),
                    &ExtensionSupport::default(),
                    &context
                ),
                Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::ClaimEnvelope
                ))
            );
        }
    }

    #[test]
    fn fsharp_stored_legacy_claim_restores_beside_versioned_import_evidence() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let source = file.evidence.source.clone().unwrap();
        let payload = format!(
            "{{\"scope\":{},\"binding\":{{\"kind\":\"context\"}}}}",
            serde_json::to_string(&source.span()).unwrap()
        );
        let derivation = vec![rootlight_ir::FactRef::File(file.id)];
        let identity = serde_json::to_vec(&(
            "rootlight.fsharp_binding",
            "1",
            document.repository,
            document.generation,
            file.provenance,
            &source,
            &payload,
            &derivation,
        ))
        .unwrap();
        let legacy = rootlight_ir::ExtensionEnvelope {
            id: rootlight_ids::derive_fact("rootlight.fsharp_binding/v1", &identity).id(),
            repository: document.repository,
            generation: document.generation,
            namespace: "rootlight.fsharp_binding".to_owned(),
            version: "1".to_owned(),
            criticality: rootlight_ir::ExtensionCriticality::Noncritical,
            payload,
            provenance: file.provenance,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation,
            },
        };
        document.extensions.push(legacy.clone());
        for binding in [
            rootlight_ir::FSharpBinding::ImportContext {
                containers: vec![source.span()],
                opens: vec![],
            },
            rootlight_ir::FSharpBinding::Container {
                path: vec![content_hash(b"Library")],
                roots: vec![],
            },
            rootlight_ir::FSharpBinding::OpenedReference {
                name: content_hash(b"offset"),
            },
            rootlight_ir::FSharpBinding::OpenedCall {
                name: content_hash(b"carry"),
                slots: 1,
            },
        ] {
            document.extensions.push(
                rootlight_ir::new_fsharp_binding_envelope(
                    document.repository,
                    document.generation,
                    file.provenance,
                    source.clone(),
                    rootlight_ir::FSharpBindingEvidence::new(source.span(), binding),
                )
                .unwrap(),
            );
        }
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let limits = IrLimits::default();
        let extensions = ExtensionSupport::default();
        let verified = IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document,
            catalog.clone(),
            &limits,
            &extensions,
            &context,
        )
        .unwrap();
        let encoded = serde_json::to_vec(verified.snapshot().document()).unwrap();
        let restored = IdentityVerifiedGeneration::restore_published_json_with_source_files(
            metadata,
            &encoded,
            content_hash(&encoded),
            catalog,
            &limits,
            &extensions,
            &context,
        )
        .unwrap();
        assert_eq!(
            restored.snapshot().document(),
            verified.snapshot().document()
        );
        let retained = restored
            .snapshot()
            .document()
            .extensions
            .iter()
            .find(|item| item.id == legacy.id)
            .unwrap();
        assert_eq!(retained, &legacy);
        assert_eq!(
            rootlight_ir::decode_fsharp_binding_envelope(retained)
                .unwrap()
                .binding(),
            &rootlight_ir::FSharpBinding::Context
        );
        assert_eq!(
            restored
                .snapshot()
                .document()
                .extensions
                .iter()
                .filter(|item| {
                    item.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE
                        && item.version == rootlight_ir::FSHARP_IMPORT_BINDING_VERSION
                })
                .count(),
            2
        );
        assert_eq!(
            restored
                .snapshot()
                .document()
                .extensions
                .iter()
                .filter(|item| {
                    item.namespace == rootlight_ir::FSHARP_BINDING_NAMESPACE
                        && item.version == rootlight_ir::FSHARP_OPENED_BINDING_VERSION
                })
                .count(),
            2
        );
    }

    #[test]
    fn fsharp_signature_value_restores_with_exact_declaration_identity() {
        assert_fsharp_signature_declaration_roundtrip(
            rootlight_ir::EntityKind::Variable,
            "carry",
            b"val carry: int -> int",
            |path, symbol| rootlight_ir::FSharpBinding::SignatureValue { path, symbol },
            rootlight_ir::FSHARP_SIGNATURE_BINDING_VERSION,
        );
    }

    #[test]
    fn fsharp_signature_record_name_restores_with_exact_declaration_identity() {
        assert_fsharp_signature_declaration_roundtrip(
            rootlight_ir::EntityKind::Struct,
            "Data",
            b"type Data = { Value: int }",
            |path, symbol| rootlight_ir::FSharpBinding::SignatureType { path, symbol },
            rootlight_ir::FSHARP_SIGNATURE_TYPE_BINDING_VERSION,
        );
    }

    #[test]
    fn fsharp_signature_alias_name_restores_with_exact_declaration_identity() {
        assert_fsharp_signature_declaration_roundtrip(
            rootlight_ir::EntityKind::TypeAlias,
            "Data",
            b"type Data = System.Int32",
            |path, symbol| rootlight_ir::FSharpBinding::SignatureAlias { path, symbol },
            rootlight_ir::FSHARP_SIGNATURE_ALIAS_BINDING_VERSION,
        );
    }

    fn assert_fsharp_signature_declaration_roundtrip(
        kind: rootlight_ir::EntityKind,
        name: &str,
        signature: &[u8],
        binding: fn(Vec<ContentHash>, rootlight_ids::SymbolId) -> rootlight_ir::FSharpBinding,
        version: &str,
    ) {
        use rootlight_ir::{
            ContainerRef, EntityRecord, EntityVisibility, FSharpBindingEvidence, FactRef,
            SymbolIdentityClaim, new_fsharp_binding_envelope, new_symbol_identity_claim_envelope,
        };
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let scope = file.evidence.source.as_ref().unwrap().span();
        let declaration = SourceRef::new(
            document.repository,
            document.generation,
            SourceSpan::new(file.id, 1, 5).unwrap(),
            file.content_hash,
            None,
        );
        let binding_source = if matches!(
            kind,
            rootlight_ir::EntityKind::Struct | rootlight_ir::EntityKind::TypeAlias
        ) {
            SourceRef::new(
                document.repository,
                document.generation,
                scope,
                file.content_hash,
                None,
            )
        } else {
            declaration.clone()
        };
        let provenance = &document.provenance[0];
        let mut container_identity = vec![0];
        container_identity.extend_from_slice(file.id.as_bytes());
        let mut claim = SymbolIdentityClaim {
            symbol: rootlight_ids::SymbolId::from_bytes([0; 20]),
            repository: document.repository,
            language: provenance.language.clone(),
            kind,
            container: Some(ContainerRef::File(file.id)),
            container_identity,
            declared_identity: name.to_owned(),
            signature_discriminator: signature.to_vec(),
            build_context_discriminator: provenance.build_context.digest().as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        document.entities.push(EntityRecord {
            id: claim.symbol,
            repository: document.repository,
            generation: document.generation,
            kind: claim.kind,
            language: claim.language.clone(),
            tier: provenance.tier,
            canonical_name: claim.declared_identity.clone(),
            display_name: claim.declared_identity.clone(),
            qualified_name: format!("Library.{name}"),
            container: claim.container,
            visibility: EntityVisibility::Public,
            flags: Vec::new(),
            provenance: provenance.id,
            evidence: FactEvidence {
                source: Some(declaration.clone()),
                derivation: Vec::new(),
            },
        });
        document.extensions.push(
            new_symbol_identity_claim_envelope(
                &claim,
                document.generation,
                provenance.id,
                declaration.clone(),
            )
            .unwrap(),
        );
        let binding = binding(
            vec![content_hash(b"Library"), content_hash(name.as_bytes())],
            claim.symbol,
        );
        let envelope = new_fsharp_binding_envelope(
            document.repository,
            document.generation,
            provenance.id,
            binding_source.clone(),
            FSharpBindingEvidence::new(scope, binding.clone()),
        )
        .unwrap();
        document.extensions.push(envelope.clone());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let limits = IrLimits::default();
        let extensions = ExtensionSupport::default();
        let verified = IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document.clone(),
            catalog.clone(),
            &limits,
            &extensions,
            &context,
        )
        .unwrap();
        let encoded = serde_json::to_vec(verified.snapshot().document()).unwrap();
        let restored = IdentityVerifiedGeneration::restore_published_json_with_source_files(
            metadata,
            &encoded,
            content_hash(&encoded),
            catalog.clone(),
            &limits,
            &extensions,
            &context,
        )
        .unwrap();
        assert_eq!(
            restored.snapshot().document(),
            verified.snapshot().document()
        );
        let retained = restored
            .snapshot()
            .document()
            .extensions
            .iter()
            .find(|item| item.id == envelope.id)
            .unwrap();
        assert_eq!(retained, &envelope);
        assert_eq!(retained.version, version);
        assert_eq!(retained.evidence.source, Some(binding_source));
        assert_eq!(
            retained.evidence.derivation,
            vec![FactRef::Entity(claim.symbol)]
        );
        assert_eq!(
            rootlight_ir::decode_fsharp_binding_envelope(retained)
                .unwrap()
                .binding(),
            &binding
        );
        for mutate in [
            |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "1".into(),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.evidence.derivation.clear(),
        ] {
            let mut changed = document.clone();
            mutate(
                changed
                    .extensions
                    .iter_mut()
                    .find(|item| item.id == envelope.id)
                    .unwrap(),
            );
            assert_eq!(
                IdentityVerifiedGeneration::verify_with_source_files(
                    metadata,
                    changed,
                    catalog.clone(),
                    &limits,
                    &extensions,
                    &context
                ),
                Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::ClaimEnvelope
                ))
            );
        }
        for wrong_version in ["3", "4", "5"] {
            if wrong_version == version {
                continue;
            }
            let mut changed = document.clone();
            changed
                .extensions
                .iter_mut()
                .find(|item| item.id == envelope.id)
                .unwrap()
                .version = wrong_version.to_owned();
            assert_eq!(
                IdentityVerifiedGeneration::verify_with_source_files(
                    metadata,
                    changed,
                    catalog.clone(),
                    &limits,
                    &extensions,
                    &context
                ),
                Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::ClaimEnvelope
                ))
            );
        }
    }

    #[test]
    fn groovy_call_context_identity_is_verified_before_snapshot_admission() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let span = SourceSpan::new(file.id, 0, file.byte_length).unwrap();
        let source = SourceRef::new(
            document.repository,
            document.generation,
            span,
            file.content_hash,
            None,
        );
        let name = SourceSpan::new(file.id, 0, 1).unwrap();
        let envelopes = [
            rootlight_ir::new_groovy_value_lookup_envelope(
                document.repository,
                document.generation,
                file.provenance,
                SourceRef::new(
                    document.repository,
                    document.generation,
                    name,
                    file.content_hash,
                    None,
                ),
                rootlight_ir::GroovyValueLookup {
                    spelling: "f".into(),
                    name_hash: content_hash(b"f"),
                    enclosing_type: Some(span),
                    enclosing_callable: Some(span),
                    lexical_binding: false,
                    static_context: true,
                    read_only: false,
                    write_kind: Some(rootlight_ir::GroovyValueWrite::Assignment),
                },
            )
            .unwrap(),
            rootlight_ir::new_groovy_call_lookup_envelope(
                document.repository,
                document.generation,
                file.provenance,
                source.clone(),
                rootlight_ir::GroovyCallLookup {
                    name,
                    name_hash: content_hash(b"f"),
                    spelling: "f".into(),
                    arguments: rootlight_ir::GroovyCallArguments::default(),
                    enclosing_type: Some(span),
                    enclosing_callable: Some(span),
                    lexical_binding: false,
                    static_context: true,
                },
            )
            .unwrap(),
            rootlight_ir::new_groovy_static_import_envelope(
                document.repository,
                document.generation,
                file.provenance,
                source,
                rootlight_ir::GroovyStaticImport {
                    name,
                    alias: None,
                    owner: content_hash(b"sample.Tools"),
                    member: None,
                    binding: None,
                    member_spelling: None,
                    binding_spelling: None,
                },
            )
            .unwrap(),
        ];
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        for envelope in envelopes {
            document
                .extensions
                .retain(|item| item.namespace != envelope.namespace);
            document.extensions.push(envelope.clone());
            IdentityVerifiedGeneration::verify_with_source_files(
                metadata,
                document.clone(),
                catalog.clone(),
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &context,
            )
            .expect("canonical context identity verifies independently of language semantics");
            for mutate in [
                |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
                |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
                |value: &mut rootlight_ir::ExtensionEnvelope| {
                    value.id = FactId::from_bytes([99; 20])
                },
            ] {
                let mut changed = document.clone();
                mutate(
                    changed
                        .extensions
                        .iter_mut()
                        .find(|item| item.id == envelope.id)
                        .unwrap(),
                );
                assert_eq!(
                    IdentityVerifiedGeneration::verify_with_source_files(
                        metadata,
                        changed,
                        catalog.clone(),
                        &IrLimits::default(),
                        &ExtensionSupport::default(),
                        &context,
                    ),
                    Err(IdentityVerificationError::IdentityMismatch(
                        IdentityMismatchComponent::ClaimEnvelope
                    ))
                );
            }
        }
    }

    #[test]
    fn groovy_member_identity_is_verified_before_snapshot_admission() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let envelope = rootlight_ir::new_groovy_member_envelope(
            document.repository,
            document.generation,
            file.provenance,
            SourceRef::new(
                document.repository,
                document.generation,
                SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                file.content_hash,
                None,
            ),
            rootlight_ir::GroovyMember {
                owner: SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                declaration: SourceSpan::new(file.id, 0, 3).unwrap(),
                name: SourceSpan::new(file.id, 0, 1).unwrap(),
                storage: rootlight_ir::GroovyMemberStorage::Static,
                visibility: rootlight_ir::EntityVisibility::Public,
            },
        )
        .unwrap();
        document.extensions.push(envelope.clone());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document.clone(),
            catalog.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .unwrap();
        for mutate in [
            |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
        ] {
            let mut changed = document.clone();
            mutate(
                changed
                    .extensions
                    .iter_mut()
                    .find(|item| item.id == envelope.id)
                    .unwrap(),
            );
            assert_eq!(
                IdentityVerifiedGeneration::verify_with_source_files(
                    metadata,
                    changed,
                    catalog.clone(),
                    &IrLimits::default(),
                    &ExtensionSupport::default(),
                    &context
                ),
                Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::ClaimEnvelope
                ))
            );
        }
    }

    #[test]
    fn derived_external_symbols_require_exact_model_input_claims() {
        use rootlight_ir::{
            EntityFlag, EntityKind, EntityRecord, EntityVisibility, FactRef, SymbolIdentityClaim,
            new_symbol_identity_claim_envelope,
        };
        let (metadata, mut document, catalog) = file_only_generation(true);
        let source = document.files[0].evidence.source.clone().unwrap();
        let mut provenance = document.provenance[0].clone();
        provenance.producer_kind = ProducerKind::Rule;
        provenance.rule = Some("language-standard-library-model-v1".to_owned());
        provenance.input_sources = vec![source.clone()];
        provenance.evidence_sources.clear();
        provenance.derivation_parents = vec![FactRef::File(source.span().file())];
        provenance.id = derive_provenance_record_id(&provenance).unwrap();
        let mut claim = SymbolIdentityClaim {
            symbol: rootlight_ids::SymbolId::from_bytes([0; 20]),
            repository: document.repository,
            language: provenance.language.clone(),
            kind: EntityKind::ExternalSymbol,
            container: None,
            container_identity: Vec::new(),
            declared_identity: "standard.constant".to_owned(),
            signature_discriminator: b"standard-v1".to_vec(),
            build_context_discriminator: provenance.build_context.digest().as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let entity = EntityRecord {
            id: claim.symbol,
            repository: document.repository,
            generation: document.generation,
            kind: claim.kind,
            language: claim.language.clone(),
            tier: provenance.tier,
            canonical_name: claim.declared_identity.clone(),
            display_name: claim.declared_identity.clone(),
            qualified_name: claim.declared_identity.clone(),
            container: None,
            visibility: EntityVisibility::Public,
            flags: vec![EntityFlag::External, EntityFlag::Synthetic],
            provenance: provenance.id,
            evidence: FactEvidence {
                source: None,
                derivation: vec![FactRef::Fact(provenance.id)],
            },
        };
        document.entities.push(entity.clone());
        document.extensions.push(
            new_symbol_identity_claim_envelope(
                &claim,
                document.generation,
                provenance.id,
                source.clone(),
            )
            .unwrap(),
        );
        document.provenance.push(provenance.clone());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document,
            catalog,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .unwrap();
        for mutate in [
            |e: &mut EntityRecord| e.kind = EntityKind::Constant,
            |e: &mut EntityRecord| e.flags.retain(|flag| *flag != EntityFlag::External),
            |e: &mut EntityRecord| e.flags.retain(|flag| *flag != EntityFlag::Synthetic),
            |e: &mut EntityRecord| e.evidence.derivation.clear(),
            |e: &mut EntityRecord| {
                e.container = Some(rootlight_ir::ContainerRef::File(
                    rootlight_ids::FileId::from_bytes([9; 20]),
                ))
            },
        ] {
            let mut invalid = entity.clone();
            mutate(&mut invalid);
            assert!(!symbol_claim_source_matches(&invalid, &provenance, &source));
        }
        for mutate in [
            |p: &mut ProvenanceRecord| p.producer_kind = ProducerKind::Parser,
            |p: &mut ProvenanceRecord| p.rule = None,
            |p: &mut ProvenanceRecord| p.rule = Some(String::new()),
            |p: &mut ProvenanceRecord| p.derivation_parents.clear(),
            |p: &mut ProvenanceRecord| p.input_sources.clear(),
            |p: &mut ProvenanceRecord| p.evidence_sources = p.input_sources.clone(),
        ] {
            let mut invalid = provenance.clone();
            mutate(&mut invalid);
            assert!(!symbol_claim_source_matches(&entity, &invalid, &source));
        }
        let mut ordinary = entity;
        ordinary.kind = EntityKind::Constant;
        ordinary.flags.clear();
        ordinary.evidence.source = Some(source.clone());
        assert!(symbol_claim_source_matches(&ordinary, &provenance, &source));
        ordinary.evidence.source = Some(document_source_at(&source, 1));
        assert!(!symbol_claim_source_matches(
            &ordinary,
            &provenance,
            &source
        ));
    }

    fn document_source_at(source: &SourceRef, start: u64) -> SourceRef {
        SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(source.span().file(), start, source.span().end_byte()).unwrap(),
            source.content_hash(),
            None,
        )
    }

    #[test]
    fn groovy_callable_identity_is_verified_before_snapshot_admission() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let envelope = rootlight_ir::new_groovy_callable_envelope(
            document.repository,
            document.generation,
            file.provenance,
            SourceRef::new(
                document.repository,
                document.generation,
                SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                file.content_hash,
                None,
            ),
            rootlight_ir::GroovyCallable {
                name: SourceSpan::new(file.id, 0, 1).unwrap(),
                name_identity: None,
                parameters: SourceSpan::new(file.id, 1, 3).unwrap(),
                parameter_count: 0,
                required_parameter_count: 0,
                variadic: false,
                dispatch: rootlight_ir::GroovyCallableDispatch::Static,
                visibility: rootlight_ir::GroovyCallableVisibility::Public,
                return_type: rootlight_ir::GroovyCallableReturnType::PrimitiveBoolean,
            },
        )
        .unwrap();
        document.extensions.push(envelope.clone());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document.clone(),
            catalog.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .expect("canonical identity verifies without asserting native language semantics");
        for mutate in [
            |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
        ] {
            let mut changed = document.clone();
            mutate(
                changed
                    .extensions
                    .iter_mut()
                    .find(|item| item.id == envelope.id)
                    .unwrap(),
            );
            assert_eq!(
                IdentityVerifiedGeneration::verify_with_source_files(
                    metadata,
                    changed,
                    catalog.clone(),
                    &IrLimits::default(),
                    &ExtensionSupport::default(),
                    &context
                ),
                Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::ClaimEnvelope
                ))
            );
        }
    }

    #[test]
    fn groovy_type_lookup_identity_is_verified_before_snapshot_admission() {
        let (metadata, mut document, catalog) = file_only_generation(true);
        let file = &document.files[0];
        let envelope = rootlight_ir::new_groovy_type_lookup_envelope(
            document.repository,
            document.generation,
            file.provenance,
            SourceRef::new(
                document.repository,
                document.generation,
                SourceSpan::new(file.id, 2, 3).unwrap(),
                file.content_hash,
                None,
            ),
            rootlight_ir::GroovyTypeLookup {
                owner: SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
                name: "Item".into(),
                fallback_import: None,
                fallback_local: None,
            },
        )
        .unwrap();
        document.extensions.push(envelope.clone());
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        IdentityVerifiedGeneration::verify_with_source_files(
            metadata,
            document.clone(),
            catalog.clone(),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .expect("canonical lookup identity verifies without asserting language semantics");
        for mutate in [
            |value: &mut rootlight_ir::ExtensionEnvelope| value.payload.push(' '),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.version = "2".into(),
            |value: &mut rootlight_ir::ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
        ] {
            let mut changed = document.clone();
            let extension = changed
                .extensions
                .iter_mut()
                .find(|item| item.id == envelope.id)
                .unwrap();
            mutate(extension);
            assert_eq!(
                IdentityVerifiedGeneration::verify_with_source_files(
                    metadata,
                    changed,
                    catalog.clone(),
                    &IrLimits::default(),
                    &ExtensionSupport::default(),
                    &context,
                ),
                Err(IdentityVerificationError::IdentityMismatch(
                    IdentityMismatchComponent::ClaimEnvelope
                ))
            );
        }
    }

    #[test]
    fn file_only_locator_bytes_use_the_encoded_text_budget() {
        let repository = RepositoryId::from_bytes([6; 16]);
        let generation = GenerationId::from_bytes([8; 20]);
        let path = "a".repeat(4_096);
        let path_identity = path.as_bytes().to_vec();
        let file = derive_file(FileIdentity {
            repository,
            path_identity: &path_identity,
        })
        .id();
        let source_hash = content_hash(b"source");
        let claim = FileIdentityClaim {
            file,
            repository,
            path: path.clone(),
            path_identity,
            content_hash: source_hash,
            byte_length: 6,
        };
        let record = FileRecord {
            id: file,
            repository,
            generation,
            path,
            path_locator: Some(
                FilePathLocator::new(
                    FilePathLocatorEncoding::UnixBytesV1,
                    vec!["61".repeat(8_192)],
                )
                .expect("encoded locator remains within its hard bound"),
            ),
            content_hash: source_hash,
            byte_length: 6,
            language: "text".to_owned(),
            encoding: "utf-8".to_owned(),
            generated: false,
            provenance: FactId::from_bytes([9; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    repository,
                    generation,
                    SourceSpan::new(file, 0, 6).expect("fixture span is valid"),
                    source_hash,
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        let catalog = SourceFileCatalog::new(vec![
            crate::SourceFileCatalogEntry::new(record, claim)
                .expect("file-only catalog entry is valid"),
        ])
        .expect("file-only catalog is valid");
        let document = NormalizedIrDocument::empty(repository, generation);
        let cancellation = Cancellation::new();
        let budget = GenerationBudget::new(64, 64, 8_192).expect("fixture budget is valid");
        let context = GenerationContext::new(&cancellation, budget);

        require_generation_budget(GENERATION_CONTRACT_VERSION, &document, &catalog, &context)
            .expect("lossless locator encoding does not consume semantic text capacity");
    }

    #[test]
    fn metadata_rejects_an_underived_generation_id() {
        let result = GenerationMetadata::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            None,
            content_hash(b"manifest"),
            content_hash(b"configuration"),
            content_hash(b"providers"),
        );

        assert_eq!(
            result,
            Err(GenerationValidationError::GenerationIdentityMismatch)
        );
    }

    #[test]
    fn budget_rejects_zero_and_above_hard_limits() {
        let default = GenerationBudget::default();
        assert_eq!(
            default.limit(GenerationResource::Rows),
            HARD_MAX_GENERATION_ROWS
        );
        assert_eq!(
            default.limit(GenerationResource::SourceReferences),
            HARD_MAX_GENERATION_SOURCE_REFS
        );
        assert_eq!(
            default.limit(GenerationResource::TextBytes),
            HARD_MAX_GENERATION_TEXT_BYTES
        );
        assert_eq!(
            default.limit(GenerationResource::EncodedTextBytes),
            HARD_MAX_GENERATION_ENCODED_TEXT_BYTES
        );
        let reduced = GenerationBudget::new(1, 1, 1).expect("reduced budget is valid");
        assert_eq!(
            reduced.limit(GenerationResource::EncodedTextBytes),
            ENCODED_TEXT_EXPANSION_FACTOR
        );
        assert_eq!(
            GenerationBudget::new(0, 1, 1),
            Err(GenerationBudgetError::Zero(GenerationResource::Rows))
        );
        assert_eq!(
            GenerationBudget::new(HARD_MAX_GENERATION_ROWS + 1, 1, 1),
            Err(GenerationBudgetError::AboveHardLimit {
                resource: GenerationResource::Rows,
                maximum: HARD_MAX_GENERATION_ROWS,
            })
        );
    }

    #[test]
    fn context_reports_first_cancellation_reason() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());

        assert_eq!(
            context.check(),
            Err(GenerationControlError::Cancelled {
                reason: CancellationReason::ClientRequest,
            })
        );
    }

    #[test]
    fn identity_verification_enforces_budget_before_canonicalization() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = verified_empty_metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let cancellation = Cancellation::new();
        let budget = GenerationBudget::new(2, 1, 1).expect("fixture budget is valid");
        let context = GenerationContext::new(&cancellation, budget);

        let result = IdentityVerifiedGeneration::verify(
            metadata,
            document,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        );

        assert_eq!(
            result,
            Err(IdentityVerificationError::Control(
                GenerationControlError::BudgetExceeded {
                    resource: GenerationResource::Rows,
                    observed: 3,
                    limit: 2,
                }
            ))
        );
    }

    #[test]
    fn snapshot_verification_rechecks_operation_budget() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = verified_empty_metadata(repository);
        let snapshot = GenerationSnapshot::new(
            metadata,
            NormalizedIrDocument::empty(repository, metadata.generation()),
            &IrLimits::default(),
            &ExtensionSupport::default(),
        )
        .expect("empty normalized fixture is valid");
        let cancellation = Cancellation::new();
        let budget = GenerationBudget::new(2, 1, 1).expect("fixture budget is valid");
        let context = GenerationContext::new(&cancellation, budget);

        let result = IdentityVerifiedGeneration::verify_snapshot(snapshot, &context);

        assert_eq!(
            result,
            Err(IdentityVerificationError::Control(
                GenerationControlError::BudgetExceeded {
                    resource: GenerationResource::Rows,
                    observed: 3,
                    limit: 2,
                }
            ))
        );
    }

    #[test]
    fn verified_generation_owns_the_safe_logical_digest_projection() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = verified_empty_metadata(repository);
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let verified = IdentityVerifiedGeneration::verify(
            metadata,
            NormalizedIrDocument::empty(repository, metadata.generation()),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .expect("empty generation verifies");
        let digests = verified
            .generation_neutral_digests(&context)
            .expect("verified generation projects logical digests");
        assert_eq!(digests.records(), 0);

        cancellation.cancel(CancellationReason::ClientRequest);
        assert_eq!(
            verified.generation_neutral_digests(&context),
            Err(GenerationNeutralDigestError::Control(
                GenerationControlError::Cancelled {
                    reason: CancellationReason::ClientRequest,
                }
            ))
        );
    }

    #[test]
    fn published_json_restore_rechecks_manifest_identity() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let encoded = serde_json::to_vec(&document).expect("fixture document serializes");
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());

        let result = IdentityVerifiedGeneration::restore_published_json(
            metadata,
            &encoded,
            content_hash(&encoded),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        );

        assert_eq!(result, Err(IdentityVerificationError::ManifestMismatch));
    }

    #[test]
    fn published_json_restore_rejects_a_digest_mismatch() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let metadata = verified_empty_metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let encoded = serde_json::to_vec(&document).expect("fixture document serializes");
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());

        let result = IdentityVerifiedGeneration::restore_published_json(
            metadata,
            &encoded,
            ContentHash::from_bytes([9; 32]),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        );

        assert_eq!(result, Err(IdentityVerificationError::InvalidGeneration));
    }

    #[test]
    fn published_messagepack_stream_amortizes_small_parser_reads() {
        struct ObservedReader<'a> {
            remaining: &'a [u8],
            reads: usize,
        }

        impl Read for ObservedReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                self.reads += 1;
                self.remaining.read(buffer)
            }
        }

        let repository = RepositoryId::from_bytes([2; 16]);
        let metadata = verified_empty_metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let encoded = rmp_serde::to_vec_named(&document).expect("fixture encodes");
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let mut reader = ObservedReader {
            remaining: &encoded,
            reads: 0,
        };

        let restored = IdentityVerifiedGeneration::restore_published_messagepack_reader(
            metadata,
            &mut reader,
            encoded.len(),
            content_hash(&encoded),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .expect("batched reads preserve the verified generation");

        assert_eq!(restored.document(), &document);
        assert!(reader.remaining.is_empty());
        assert!(
            reader.reads <= encoded.len().div_ceil(IDENTITY_RECIPE_CHECKPOINT_BYTES) + 1,
            "parser fragments must not become individual digest reads: {}",
            reader.reads,
        );
    }

    #[test]
    fn published_messagepack_stream_bounds_read_ahead_and_rejects_bad_lengths() {
        let repository = RepositoryId::from_bytes([2; 16]);
        let metadata = verified_empty_metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let encoded = rmp_serde::to_vec_named(&document).expect("fixture encodes");
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let mut trailing = encoded.clone();
        trailing.extend_from_slice(&[0; 2 * IDENTITY_RECIPE_CHECKPOINT_BYTES]);

        for (input, declared) in [
            (&encoded[..encoded.len() - 1], encoded.len()),
            (encoded.as_slice(), encoded.len() - 1),
            (encoded.as_slice(), encoded.len() + 1),
            (trailing.as_slice(), encoded.len()),
        ] {
            let mut reader = io::Cursor::new(input);
            let result = IdentityVerifiedGeneration::restore_published_messagepack_reader(
                metadata,
                &mut reader,
                declared,
                content_hash(&encoded),
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &context,
            );
            assert_eq!(result, Err(IdentityVerificationError::InvalidGeneration));
            assert!(reader.position() <= u64::try_from(declared + 1).expect("fixture fits"));
        }

        let mut reader = io::Cursor::new(&encoded);
        let mut limits = IrLimits::default();
        limits.max_document_bytes = encoded.len() - 1;
        assert_eq!(
            IdentityVerifiedGeneration::restore_published_messagepack_reader(
                metadata,
                &mut reader,
                encoded.len(),
                content_hash(&encoded),
                &limits,
                &ExtensionSupport::default(),
                &context,
            ),
            Err(IdentityVerificationError::InvalidGeneration),
        );
        assert_eq!(reader.position(), 0);
    }

    #[test]
    fn published_messagepack_stream_preserves_source_catalog_across_short_reads() {
        struct ShortReader<'a> {
            remaining: &'a [u8],
            maximum_read: usize,
        }

        impl Read for ShortReader<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let maximum = buffer.len().min(self.maximum_read);
                self.remaining.read(&mut buffer[..maximum])
            }
        }

        let (metadata, mut document, catalog) = file_only_generation(true);
        let provenance = document.provenance[0].clone();
        for index in 0..32 {
            let mut record = provenance.clone();
            record.rule = Some(format!("stream-validation-{index}"));
            record.id = derive_provenance_record_id(&record).expect("fixture identity derives");
            document.provenance.push(record);
        }
        let limits = IrLimits::default();
        let extensions = ExtensionSupport::default();
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let expected = GenerationSnapshot::new_with_source_files(
            metadata,
            document.clone(),
            catalog.clone(),
            &limits,
            &extensions,
        )
        .expect("fixture canonicalizes");
        let expected = IdentityVerifiedGeneration::verify_snapshot(expected, &context)
            .expect("fixture identities verify");
        let encoded = rmp_serde::to_vec_named(&document).expect("fixture encodes");
        assert!(encoded.len() > 2 * IDENTITY_RECIPE_CHECKPOINT_BYTES);

        for maximum_read in [1, 3, IDENTITY_RECIPE_CHECKPOINT_BYTES] {
            let restored =
                IdentityVerifiedGeneration::restore_published_messagepack_reader_with_source_files(
                    metadata,
                    ShortReader {
                        remaining: &encoded,
                        maximum_read,
                    },
                    encoded.len(),
                    content_hash(&encoded),
                    catalog.clone(),
                    &limits,
                    &extensions,
                    &context,
                )
                .expect("fragmented input preserves the full generation");
            assert_eq!(restored, expected);
        }
    }

    #[test]
    fn published_messagepack_stream_preserves_cancellation_during_read() {
        struct CancelOnRead<'a> {
            remaining: &'a [u8],
            cancellation: &'a Cancellation,
        }

        impl Read for CancelOnRead<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let count = self.remaining.read(buffer)?;
                self.cancellation.cancel(CancellationReason::ClientRequest);
                Ok(count)
            }
        }

        let repository = RepositoryId::from_bytes([2; 16]);
        let metadata = verified_empty_metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let encoded = rmp_serde::to_vec_named(&document).expect("fixture encodes");
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        assert_eq!(
            IdentityVerifiedGeneration::restore_published_messagepack_reader(
                metadata,
                CancelOnRead {
                    remaining: &encoded,
                    cancellation: &cancellation
                },
                encoded.len(),
                content_hash(&encoded),
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &context,
            ),
            Err(IdentityVerificationError::Control(
                GenerationControlError::Cancelled {
                    reason: CancellationReason::ClientRequest,
                }
            )),
        );
    }

    #[test]
    fn published_messagepack_stream_hashes_while_restoring() {
        let repository = RepositoryId::from_bytes([2; 16]);
        let metadata = verified_empty_metadata(repository);
        let document = NormalizedIrDocument::empty(repository, metadata.generation());
        let encoded =
            rmp_serde::to_vec_named(&document).expect("fixture document serializes as MessagePack");
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());

        let mismatch = IdentityVerifiedGeneration::restore_published_messagepack_reader(
            metadata,
            io::Cursor::new(&encoded),
            encoded.len(),
            ContentHash::from_bytes([9; 32]),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        );
        assert_eq!(mismatch, Err(IdentityVerificationError::InvalidGeneration));

        let restored = IdentityVerifiedGeneration::restore_published_messagepack_reader(
            metadata,
            io::Cursor::new(&encoded),
            encoded.len(),
            content_hash(&encoded),
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &context,
        )
        .expect("the MessagePack recovery stream verifies");

        assert_eq!(restored.document(), &document);
    }

    #[test]
    fn identity_recipe_hashing_observes_mid_stream_cancellation() {
        let cancellation = Cancellation::new();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let mut writer =
            IdentityRecipeHashWriter::new(Some(context)).expect("active context is accepted");

        let result = serde_json::to_writer(
            &mut writer,
            &CancelDuringSerialization {
                cancellation: &cancellation,
            },
        );

        assert!(result.is_err());
        assert_eq!(
            writer.control,
            Some(GenerationControlError::Cancelled {
                reason: CancellationReason::ClientRequest,
            })
        );
    }

    #[test]
    fn source_budget_is_checked_before_registry_growth() {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let first_file = FileId::from_bytes([3; 20]);
        let second_file = FileId::from_bytes([4; 20]);
        let hash = content_hash(b"x");
        let first = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(first_file, 0, 1).expect("fixture span is valid"),
            hash,
            None,
        );
        let second = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(second_file, 0, 1).expect("fixture span is valid"),
            hash,
            None,
        );
        let cancellation = Cancellation::new();
        let budget = GenerationBudget::new(100, 1, 1).expect("fixture budget is valid");
        let context = GenerationContext::new(&cancellation, budget);
        let mut sources = BTreeSet::new();
        let mut rows = 0;
        observe_source(&first, &mut sources, &mut rows, &context)
            .expect("first source fits the registry");

        let result = observe_source(&second, &mut sources, &mut rows, &context);

        assert_eq!(
            result,
            Err(GenerationControlError::BudgetExceeded {
                resource: GenerationResource::SourceReferences,
                observed: 2,
                limit: 1,
            })
        );
        assert_eq!(sources.len(), 1);
        assert_eq!(rows, 1);
    }
}
