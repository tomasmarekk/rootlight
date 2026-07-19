//! Logical generation data shared by replaceable persistence backends.
//!
//! Values crossing this boundary own their records. Backend implementations
//! must enforce the supplied row, source-reference, text, and cancellation caps.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::{self, Write},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    ContentHash, GenerationId, GenerationIdentity, RepositoryId, derive_generation,
};
use rootlight_ir::{
    ExtensionSupport, FILE_IDENTITY_CLAIM_NAMESPACE, FactEvidence, FileIdentityClaim,
    IdentityClaimError, IrDocumentValidationError, IrLimits, IrVersion,
    LEXICAL_EXTENSION_NAMESPACE, NORMALIZED_IR_VERSION, NormalizedIrDocument, OccurrenceTarget,
    SYMBOL_IDENTITY_CLAIM_NAMESPACE, SourceRef, canonicalize_ir_document,
    decode_file_identity_claim_envelope_with_checkpoint,
    decode_symbol_identity_claim_envelope_with_checkpoint,
    derive_coverage_record_id_with_checkpoint, derive_diagnostic_record_id_with_checkpoint,
    derive_occurrence_record_id_with_checkpoint, derive_provenance_record_id_with_checkpoint,
    derive_relation_record_id_with_checkpoint, derive_skipped_region_id_with_checkpoint,
    derive_source_mapping_record_id_with_checkpoint, validate_lexical_evidence_envelope,
};
use serde::Serialize;

/// Current backend-neutral generation contract version.
pub const GENERATION_CONTRACT_VERSION: GenerationContractVersion =
    GenerationContractVersion::new(1, 2);

/// Version of the generation identity-claim recipe.
pub const PROPOSED_IDENTITY_CLAIM_VERSION: GenerationContractVersion =
    GenerationContractVersion::new(1, 0);

const LEGACY_GENERATION_CONTRACT_VERSION: GenerationContractVersion =
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

const fn generation_format_version(version: GenerationContractVersion) -> u32 {
    (version.major() as u32) << 16 | version.minor() as u32
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
        require_document_budget(metadata.contract_version(), &document, context)
            .map_err(GenerationSnapshotError::Control)?;
        context.check().map_err(GenerationSnapshotError::Control)?;
        let snapshot = Self::new(metadata, document, limits, extensions)
            .map_err(GenerationSnapshotError::Validation)?;
        context.check().map_err(GenerationSnapshotError::Control)?;
        require_document_budget(
            snapshot.metadata().contract_version(),
            snapshot.document(),
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
        &self.document
    }

    /// Consumes the snapshot into its canonical normalized document.
    #[must_use]
    pub fn into_document(self) -> NormalizedIrDocument {
        self.document
    }
}

fn require_document_budget(
    contract_version: GenerationContractVersion,
    document: &NormalizedIrDocument,
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
    let mut sources = BTreeSet::new();

    for file in &document.files {
        context.check()?;
        observe_evidence(&file.evidence, &mut sources, &mut rows, context)?;
        observe_text(&file.path, &mut text_bytes, context)?;
        observe_text(&file.language, &mut text_bytes, context)?;
        observe_text(&file.encoding, &mut text_bytes, context)?;
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
                return Err(IdentityVerificationError::IdentityMismatch);
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
        let snapshot =
            GenerationSnapshot::new_with_context(metadata, document, limits, extensions, context)
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
        require_document_budget(
            snapshot.metadata().contract_version(),
            snapshot.document(),
            context,
        )
        .map_err(IdentityVerificationError::Control)?;
        verify_snapshot_identities(&snapshot, context)?;
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        Ok(Self { snapshot })
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

    /// Consumes the verified wrapper into its canonical generation.
    #[must_use]
    pub fn into_snapshot(self) -> GenerationSnapshot {
        self.snapshot
    }
}

fn verify_snapshot_identities(
    snapshot: &GenerationSnapshot,
    context: &GenerationContext<'_>,
) -> Result<(), IdentityVerificationError> {
    let document = snapshot.document();
    let mut file_claims = BTreeMap::new();
    let mut symbol_claims = BTreeMap::new();
    let mut has_unsupported_extension = false;
    for envelope in &document.extensions {
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
        match envelope.namespace.as_str() {
            FILE_IDENTITY_CLAIM_NAMESPACE => {
                let claim = decode_file_identity_claim_envelope_with_checkpoint(envelope, || {
                    context.check().is_ok()
                })
                .map_err(|error| map_claim_error(error, context))?;
                let source = envelope
                    .evidence
                    .source
                    .clone()
                    .ok_or(IdentityVerificationError::IdentityMismatch)?;
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
                let source = envelope
                    .evidence
                    .source
                    .clone()
                    .ok_or(IdentityVerificationError::IdentityMismatch)?;
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
                let subject = envelope
                    .evidence
                    .derivation
                    .first()
                    .copied()
                    .filter(|_| envelope.evidence.derivation.len() == 1)
                    .ok_or(IdentityVerificationError::IdentityMismatch)?;
                let source = envelope
                    .evidence
                    .source
                    .as_ref()
                    .ok_or(IdentityVerificationError::IdentityMismatch)?;
                validate_lexical_evidence_envelope(envelope, subject, source, envelope.provenance)
                    .map_err(|_| IdentityVerificationError::IdentityMismatch)?;
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
        let provenance = provenance_by_id
            .get(&entity.provenance)
            .ok_or(IdentityVerificationError::IdentityMismatch)?;
        if claim.repository != entity.repository
            || claim.language != entity.language
            || claim.kind != entity.kind
            || claim.container != entity.container
            || claim.declared_identity != entity.canonical_name
            || entity.evidence.source.as_ref() != Some(claim_source)
            || claim.build_context_discriminator
                != provenance.build_context.digest().as_bytes().as_slice()
            || claim.derived_symbol() != entity.id
        {
            return Err(IdentityVerificationError::IdentityMismatch);
        }
        context
            .check()
            .map_err(IdentityVerificationError::Control)?;
    }

    verify_fact_ids(document, context)
}

fn map_claim_error(
    error: IdentityClaimError,
    context: &GenerationContext<'_>,
) -> IdentityVerificationError {
    if error == IdentityClaimError::Interrupted {
        match context.check() {
            Err(error) => IdentityVerificationError::Control(error),
            Ok(()) => IdentityVerificationError::IdentityMismatch,
        }
    } else {
        IdentityVerificationError::IdentityMismatch
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
            return Err(IdentityVerificationError::IdentityMismatch);
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
    #[error("generation identity does not match its canonical recipe")]
    IdentityMismatch,
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

impl IdentityVerificationError {
    fn from_snapshot(error: GenerationSnapshotError) -> Self {
        match error {
            GenerationSnapshotError::Control(error) => Self::Control(error),
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
        FileId, GenerationId, GenerationIdentity, RepositoryId, content_hash, derive_generation,
    };
    use rootlight_ir::SourceSpan;
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
                    limit: 2,
                }
            ))
        );
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
                limit: 1,
            })
        );
        assert_eq!(sources.len(), 1);
        assert_eq!(rows, 1);
    }
}
