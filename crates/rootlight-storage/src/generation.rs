//! Logical generation data shared by replaceable persistence backends.
//!
//! Values crossing this boundary own their records. Backend implementations
//! must enforce the supplied row, source-reference, text, and cancellation caps.

use std::error::Error;

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    ContentHash, GenerationId, GenerationIdentity, RepositoryId, derive_generation,
};
use rootlight_ir::{
    ExtensionSupport, IrDocumentValidationError, IrLimits, IrVersion, NORMALIZED_IR_VERSION,
    NormalizedIrDocument, canonicalize_ir_document,
};

/// Current backend-neutral generation contract version.
pub const GENERATION_CONTRACT_VERSION: GenerationContractVersion =
    GenerationContractVersion::new(1, 1);

/// Hard ceiling for physical rows written or materialized by one operation.
pub const HARD_MAX_GENERATION_ROWS: u64 = 1_000_000;
/// Hard ceiling for distinct source references in one generation operation.
pub const HARD_MAX_GENERATION_SOURCE_REFS: u64 = 500_000;
/// Hard ceiling for owned dynamic text crossing one generation operation.
pub const HARD_MAX_GENERATION_TEXT_BYTES: u64 = 128 * 1024 * 1024;

const DEFAULT_MAX_GENERATION_ROWS: u64 = 250_000;
const DEFAULT_MAX_GENERATION_SOURCE_REFS: u64 = 100_000;
const DEFAULT_MAX_GENERATION_TEXT_BYTES: u64 = 32 * 1024 * 1024;

/// Major/minor compatibility version for logical generation storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
        if parent == Some(generation) {
            return Err(GenerationValidationError::SelfParent);
        }
        let expected = derive_generation(GenerationIdentity {
            repository,
            parent,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version: generation_format_version(),
        })
        .id();
        if generation != expected {
            return Err(GenerationValidationError::GenerationIdentityMismatch);
        }
        Ok(Self {
            repository,
            generation,
            parent,
            manifest_hash,
            configuration_hash,
            provider_set_hash,
        })
    }

    /// Returns the logical storage contract version.
    #[must_use]
    pub const fn contract_version(&self) -> GenerationContractVersion {
        GENERATION_CONTRACT_VERSION
    }

    /// Returns the normalized IR version stored by this contract.
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

const fn generation_format_version() -> u32 {
    (GENERATION_CONTRACT_VERSION.major() as u32) << 16 | GENERATION_CONTRACT_VERSION.minor() as u32
}

/// Owned, canonical normalized IR for one generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationSnapshot {
    metadata: GenerationMetadata,
    document: NormalizedIrDocument,
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
        let document = canonicalize_ir_document(document, limits, extensions)
            .map_err(GenerationValidationError::InvalidIr)?;
        if document.repository != metadata.repository()
            || document.generation != metadata.generation()
        {
            return Err(GenerationValidationError::OwnershipMismatch);
        }
        Ok(Self { metadata, document })
    }

    /// Returns the generation metadata.
    #[must_use]
    pub const fn metadata(&self) -> GenerationMetadata {
        self.metadata
    }

    /// Returns the canonical normalized document.
    #[must_use]
    pub const fn document(&self) -> &NormalizedIrDocument {
        &self.document
    }

    /// Consumes the snapshot into its canonical normalized document.
    #[must_use]
    pub fn into_document(self) -> NormalizedIrDocument {
        self.document
    }
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
            1,
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

    /// Returns the total persisted row count, including normalized child rows.
    #[must_use]
    pub const fn stored_rows(self) -> u64 {
        self.stored_rows
    }

    /// Returns dynamic UTF-8 bytes owned by the persisted logical records.
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
}

impl GenerationResource {
    const fn hard_limit(self) -> u64 {
        match self {
            Self::Rows => HARD_MAX_GENERATION_ROWS,
            Self::SourceReferences => HARD_MAX_GENERATION_SOURCE_REFS,
            Self::TextBytes => HARD_MAX_GENERATION_TEXT_BYTES,
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

    /// Checks one observed resource amount against its operation limit.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationControlError::BudgetExceeded`] when `observed`
    /// exceeds the selected resource limit.
    pub const fn require(
        self,
        resource: GenerationResource,
        observed: u64,
    ) -> Result<(), GenerationControlError> {
        let limit = self.budget.limit(resource);
        if observed > limit {
            Err(GenerationControlError::BudgetExceeded { resource, limit })
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

/// A backend-neutral reader for one immutable, pinned generation.
pub trait GenerationReader: Send + Sync {
    /// Typed backend error with a source-redacted display contract.
    type Error: Error + Send + Sync + 'static;

    /// Returns the pinned generation metadata.
    fn metadata(&self) -> GenerationMetadata;

    /// Returns verified generation cardinalities.
    fn stats(&self) -> GenerationStats;

    /// Materializes the owned canonical generation within the supplied limits.
    ///
    /// # Errors
    ///
    /// Returns the backend error for cancellation, budget, compatibility,
    /// corruption, or storage failures.
    fn read_generation(
        &self,
        context: &GenerationContext<'_>,
    ) -> Result<GenerationSnapshot, Self::Error>;
}

/// A backend-neutral writer that consumes one complete validated generation.
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
        generation: GenerationSnapshot,
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
        /// Configured operation limit.
        limit: u64,
    },
}

/// Invalid generation metadata or normalized IR.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GenerationValidationError {
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
        GenerationId, GenerationIdentity, RepositoryId, content_hash, derive_generation,
    };

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
            format_version: generation_format_version(),
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
}
