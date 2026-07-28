//! Portable read-only transfer bundles for immutable generations.
//!
//! A bundle contains canonical source-free generation metadata followed by one
//! canonical normalized-IR artifact. Source bodies and checkout paths are never
//! embedded. Import therefore requires the receiving caller to provide the
//! expected repository and source-set hash before identity verification.

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, GenerationId, RepositoryId, content_hash};
use rootlight_ir::{
    ExtensionSupport, IrDocument, IrLimits, NormalizedIrDocument, decode_ir_document,
};
use serde::{Deserialize, Serialize};

use crate::{
    GenerationContext, GenerationContractVersion, GenerationMetadata, GenerationSnapshot,
    GenerationValidationError, IdentityVerificationError, IdentityVerifiedGeneration,
};

/// Wire identity for portable immutable-generation bundles.
pub const SHARED_GENERATION_BUNDLE_SCHEMA: &str = "rootlight.shared-generation/1";
/// Maximum accepted header size under any caller policy.
pub const HARD_MAX_SHARED_GENERATION_HEADER_BYTES: usize = 64 * 1024;
/// Maximum accepted complete bundle size under any caller policy.
pub const HARD_MAX_SHARED_GENERATION_BUNDLE_BYTES: usize = 128 * 1024 * 1024;

const MAGIC: &[u8; 8] = b"RLSHARE1";
const LENGTH_BYTES: usize = size_of::<u32>();
const PREFIX_BYTES: usize = MAGIC.len() + LENGTH_BYTES;
const DEFAULT_MAX_BUNDLE_BYTES: usize = 32 * 1024 * 1024;
const SOURCE_SET_DOMAIN: &[u8] = b"rootlight/shared-source-set/v1";
const ARTIFACT_KIND: &str = "normalized_ir";
const ARTIFACT_FORMAT: &str = "rootlight.normalized-ir/1.1";

/// Caller-selected transfer limits no broader than the process hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedGenerationLimits {
    max_header_bytes: usize,
    max_bundle_bytes: usize,
}

impl SharedGenerationLimits {
    /// Creates bounded transfer limits.
    ///
    /// # Errors
    ///
    /// Returns [`SharedGenerationError::InvalidLimit`] for zero values, a
    /// header larger than the complete bundle, or values above hard ceilings.
    pub fn new(
        max_header_bytes: usize,
        max_bundle_bytes: usize,
    ) -> Result<Self, SharedGenerationError> {
        if max_header_bytes == 0
            || max_bundle_bytes == 0
            || max_header_bytes > max_bundle_bytes
            || max_header_bytes > HARD_MAX_SHARED_GENERATION_HEADER_BYTES
            || max_bundle_bytes > HARD_MAX_SHARED_GENERATION_BUNDLE_BYTES
        {
            return Err(SharedGenerationError::InvalidLimit);
        }
        Ok(Self {
            max_header_bytes,
            max_bundle_bytes,
        })
    }

    /// Returns the maximum canonical manifest size.
    #[must_use]
    pub const fn max_header_bytes(self) -> usize {
        self.max_header_bytes
    }

    /// Returns the maximum complete transfer size.
    #[must_use]
    pub const fn max_bundle_bytes(self) -> usize {
        self.max_bundle_bytes
    }
}

impl Default for SharedGenerationLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: HARD_MAX_SHARED_GENERATION_HEADER_BYTES,
            max_bundle_bytes: DEFAULT_MAX_BUNDLE_BYTES,
        }
    }
}

/// Exact receiving-side identity expected for one imported generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedGenerationExpectation {
    repository: RepositoryId,
    source_set_hash: ContentHash,
    generation: Option<GenerationId>,
}

impl SharedGenerationExpectation {
    /// Requires a repository and source-set identity while accepting the
    /// generation declared by the verified bundle.
    #[must_use]
    pub const fn new(repository: RepositoryId, source_set_hash: ContentHash) -> Self {
        Self {
            repository,
            source_set_hash,
            generation: None,
        }
    }

    /// Also requires one exact immutable generation identity.
    #[must_use]
    pub const fn with_generation(mut self, generation: GenerationId) -> Self {
        self.generation = Some(generation);
        self
    }
}

/// One verified imported generation that has no mutation or activation API.
#[derive(Debug)]
pub struct SharedGenerationImport {
    generation: IdentityVerifiedGeneration,
    source_set_hash: ContentHash,
}

impl SharedGenerationImport {
    /// Returns the identity-verified immutable generation.
    #[must_use]
    pub const fn generation(&self) -> &IdentityVerifiedGeneration {
        &self.generation
    }

    /// Returns the verified source-set identity required by the importer.
    #[must_use]
    pub const fn source_set_hash(&self) -> ContentHash {
        self.source_set_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedGenerationManifest {
    schema: String,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    generation_contract_major: u16,
    generation_contract_minor: u16,
    manifest_hash: ContentHash,
    configuration_hash: ContentHash,
    provider_set_hash: ContentHash,
    source_set_hash: ContentHash,
    artifacts: Vec<SharedArtifactManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SharedArtifactManifest {
    kind: String,
    format: String,
    bytes: u64,
    content_hash: ContentHash,
}

/// Computes the canonical source-set identity for one normalized generation.
///
/// The digest covers the repository and every canonical file identity,
/// relative display path, content hash, length, language, encoding, and
/// generated classification. It deliberately excludes both source bodies and
/// generation metadata so independent builds of the same source set agree.
///
/// # Errors
///
/// Returns [`SharedGenerationError::ResourceLimit`] when a length cannot be
/// represented or accumulated safely.
pub fn shared_generation_source_set_hash(
    document: &NormalizedIrDocument,
) -> Result<ContentHash, SharedGenerationError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(SOURCE_SET_DOMAIN);
    hasher.update(document.repository.as_bytes());
    let file_count =
        u64::try_from(document.files.len()).map_err(|_| SharedGenerationError::ResourceLimit)?;
    hasher.update(&file_count.to_be_bytes());
    for file in &document.files {
        write_hash_component(&mut hasher, file.id.as_bytes())?;
        write_hash_component(&mut hasher, file.path.as_bytes())?;
        write_hash_component(&mut hasher, file.content_hash.as_bytes())?;
        write_hash_component(&mut hasher, &file.byte_length.to_be_bytes())?;
        write_hash_component(&mut hasher, file.language.as_bytes())?;
        write_hash_component(&mut hasher, file.encoding.as_bytes())?;
        write_hash_component(&mut hasher, &[u8::from(file.generated)])?;
    }
    Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
}

/// Encodes one identity-verified generation for read-only transfer.
///
/// # Errors
///
/// Returns [`SharedGenerationError`] for cancellation, noncanonical metadata,
/// serialization failure, or a configured resource limit.
pub fn export_shared_generation(
    generation: &GenerationSnapshot,
    limits: SharedGenerationLimits,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, SharedGenerationError> {
    check(cancellation)?;
    let metadata = generation.metadata();
    let document = generation.document();
    if document.repository != metadata.repository() || document.generation != metadata.generation()
    {
        return Err(SharedGenerationError::Identity);
    }
    let document_bytes =
        serde_json::to_vec(document).map_err(|_| SharedGenerationError::Encoding)?;
    let document_length =
        u64::try_from(document_bytes.len()).map_err(|_| SharedGenerationError::ResourceLimit)?;
    let source_set_hash = shared_generation_source_set_hash(document)?;
    let contract = metadata.contract_version();
    let manifest = SharedGenerationManifest {
        schema: SHARED_GENERATION_BUNDLE_SCHEMA.to_owned(),
        repository: metadata.repository(),
        generation: metadata.generation(),
        parent: metadata.parent(),
        generation_contract_major: contract.major(),
        generation_contract_minor: contract.minor(),
        manifest_hash: metadata.manifest_hash(),
        configuration_hash: metadata.configuration_hash(),
        provider_set_hash: metadata.provider_set_hash(),
        source_set_hash,
        artifacts: vec![SharedArtifactManifest {
            kind: ARTIFACT_KIND.to_owned(),
            format: ARTIFACT_FORMAT.to_owned(),
            bytes: document_length,
            content_hash: content_hash(&document_bytes),
        }],
    };
    let header = serde_json::to_vec(&manifest).map_err(|_| SharedGenerationError::Encoding)?;
    if header.is_empty() || header.len() > limits.max_header_bytes {
        return Err(SharedGenerationError::ResourceLimit);
    }
    let header_length =
        u32::try_from(header.len()).map_err(|_| SharedGenerationError::ResourceLimit)?;
    let total = PREFIX_BYTES
        .checked_add(header.len())
        .and_then(|bytes| bytes.checked_add(document_bytes.len()))
        .ok_or(SharedGenerationError::ResourceLimit)?;
    if total > limits.max_bundle_bytes {
        return Err(SharedGenerationError::ResourceLimit);
    }
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(total)
        .map_err(|_| SharedGenerationError::ResourceLimit)?;
    encoded.extend_from_slice(MAGIC);
    encoded.extend_from_slice(&header_length.to_be_bytes());
    encoded.extend_from_slice(&header);
    encoded.extend_from_slice(&document_bytes);
    check(cancellation)?;
    Ok(encoded)
}

/// Verifies and decodes one portable generation without activating or
/// overwriting any local generation.
///
/// # Errors
///
/// Returns [`SharedGenerationError`] when framing, canonical encoding,
/// repository/generation/source identity, artifact inventory, normalized IR,
/// or generation identity verification fails.
pub fn import_shared_generation(
    encoded: &[u8],
    expectation: SharedGenerationExpectation,
    limits: SharedGenerationLimits,
    ir_limits: &IrLimits,
    extensions: &ExtensionSupport,
    context: &GenerationContext<'_>,
) -> Result<SharedGenerationImport, SharedGenerationError> {
    context
        .check()
        .map_err(|_| SharedGenerationError::Cancelled)?;
    if encoded.len() < PREFIX_BYTES || encoded.len() > limits.max_bundle_bytes {
        return Err(SharedGenerationError::ResourceLimit);
    }
    if encoded.get(..MAGIC.len()) != Some(MAGIC) {
        return Err(SharedGenerationError::Framing);
    }
    let length_bytes: [u8; LENGTH_BYTES] = encoded[MAGIC.len()..PREFIX_BYTES]
        .try_into()
        .map_err(|_| SharedGenerationError::Framing)?;
    let header_length = usize::try_from(u32::from_be_bytes(length_bytes))
        .map_err(|_| SharedGenerationError::ResourceLimit)?;
    if header_length == 0 || header_length > limits.max_header_bytes {
        return Err(SharedGenerationError::ResourceLimit);
    }
    let header_end = PREFIX_BYTES
        .checked_add(header_length)
        .ok_or(SharedGenerationError::ResourceLimit)?;
    let header = encoded
        .get(PREFIX_BYTES..header_end)
        .ok_or(SharedGenerationError::Framing)?;
    let document_bytes = encoded
        .get(header_end..)
        .filter(|bytes| !bytes.is_empty())
        .ok_or(SharedGenerationError::Framing)?;
    let manifest: SharedGenerationManifest =
        serde_json::from_slice(header).map_err(|_| SharedGenerationError::Encoding)?;
    let canonical_header =
        serde_json::to_vec(&manifest).map_err(|_| SharedGenerationError::Encoding)?;
    if canonical_header != header {
        return Err(SharedGenerationError::NonCanonical);
    }
    verify_manifest(&manifest, document_bytes, expectation)?;
    let document = match decode_ir_document(document_bytes, ir_limits, extensions)
        .map_err(|_| SharedGenerationError::Document)?
    {
        IrDocument::NormalizedV1_1(document) => document,
        IrDocument::LegacyV1_0(_) => return Err(SharedGenerationError::Document),
    };
    if document.repository != manifest.repository || document.generation != manifest.generation {
        return Err(SharedGenerationError::Identity);
    }
    if shared_generation_source_set_hash(&document)? != manifest.source_set_hash {
        return Err(SharedGenerationError::SourceSet);
    }
    let contract = GenerationContractVersion::new(
        manifest.generation_contract_major,
        manifest.generation_contract_minor,
    );
    let metadata = GenerationMetadata::new_for_contract(
        contract,
        manifest.repository,
        manifest.generation,
        manifest.parent,
        manifest.manifest_hash,
        manifest.configuration_hash,
        manifest.provider_set_hash,
    )
    .map_err(map_metadata_error)?;
    let generation =
        IdentityVerifiedGeneration::verify(metadata, document, ir_limits, extensions, context)
            .map_err(map_identity_error)?;
    Ok(SharedGenerationImport {
        generation,
        source_set_hash: manifest.source_set_hash,
    })
}

fn verify_manifest(
    manifest: &SharedGenerationManifest,
    document_bytes: &[u8],
    expectation: SharedGenerationExpectation,
) -> Result<(), SharedGenerationError> {
    if manifest.schema != SHARED_GENERATION_BUNDLE_SCHEMA
        || manifest.repository != expectation.repository
        || expectation
            .generation
            .is_some_and(|generation| generation != manifest.generation)
    {
        return Err(SharedGenerationError::Identity);
    }
    if manifest.source_set_hash != expectation.source_set_hash {
        return Err(SharedGenerationError::SourceSet);
    }
    let [artifact] = manifest.artifacts.as_slice() else {
        return Err(SharedGenerationError::Inventory);
    };
    let document_length =
        u64::try_from(document_bytes.len()).map_err(|_| SharedGenerationError::ResourceLimit)?;
    if artifact.kind != ARTIFACT_KIND
        || artifact.format != ARTIFACT_FORMAT
        || artifact.bytes != document_length
        || artifact.content_hash != content_hash(document_bytes)
    {
        return Err(SharedGenerationError::Inventory);
    }
    Ok(())
}

fn write_hash_component(
    hasher: &mut blake3::Hasher,
    value: &[u8],
) -> Result<(), SharedGenerationError> {
    let length = u64::try_from(value.len()).map_err(|_| SharedGenerationError::ResourceLimit)?;
    hasher.update(&length.to_be_bytes());
    hasher.update(value);
    Ok(())
}

fn check(cancellation: &Cancellation) -> Result<(), SharedGenerationError> {
    cancellation
        .check()
        .map_err(|_| SharedGenerationError::Cancelled)
}

fn map_metadata_error(_error: GenerationValidationError) -> SharedGenerationError {
    SharedGenerationError::Identity
}

fn map_identity_error(_error: IdentityVerificationError) -> SharedGenerationError {
    SharedGenerationError::Identity
}

/// Invalid, mismatched, or resource-exhausting shared generation transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SharedGenerationError {
    /// A configured transfer limit is zero, inconsistent, or above hard limits.
    #[error("shared generation transfer limit is invalid")]
    InvalidLimit,
    /// The transfer exceeded a byte, collection, or representation limit.
    #[error("shared generation transfer exceeded a resource limit")]
    ResourceLimit,
    /// Cooperative cancellation or a monotonic deadline stopped the transfer.
    #[error("shared generation transfer was cancelled")]
    Cancelled,
    /// The bundle prefix or length-delimited layout is malformed.
    #[error("shared generation bundle framing is invalid")]
    Framing,
    /// The canonical manifest or normalized document could not be encoded.
    #[error("shared generation bundle encoding is invalid")]
    Encoding,
    /// The manifest bytes are valid JSON but not the unique canonical encoding.
    #[error("shared generation manifest is not canonical")]
    NonCanonical,
    /// Repository, generation, or generation-metadata identity differs.
    #[error("shared generation identity differs")]
    Identity,
    /// The receiving source-set identity differs from the exported generation.
    #[error("shared generation source set differs")]
    SourceSet,
    /// The complete immutable artifact inventory differs or is corrupted.
    #[error("shared generation artifact inventory differs")]
    Inventory,
    /// The normalized IR artifact is malformed or unsupported.
    #[error("shared generation document is invalid")]
    Document,
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::{GenerationIdentity, RepositoryId, content_hash, derive_generation};
    use rootlight_ir::{ExtensionSupport, IrLimits, NormalizedIrDocument};

    use super::*;
    use crate::{GENERATION_CONTRACT_VERSION, GenerationBudget, GenerationManifestRecipe};

    fn fixture() -> (GenerationSnapshot, Cancellation, IrLimits, ExtensionSupport) {
        let repository = RepositoryId::from_bytes([7; 16]);
        let configuration_hash = content_hash(b"shared-configuration");
        let manifest_hash =
            GenerationManifestRecipe::new(repository, configuration_hash, Vec::new())
                .expect("empty source manifest is valid")
                .canonical_hash()
                .expect("source manifest encodes");
        let provider_set_hash = content_hash(b"shared-providers");
        let format_version = (u32::from(GENERATION_CONTRACT_VERSION.major()) << 16)
            | u32::from(GENERATION_CONTRACT_VERSION.minor());
        let generation = derive_generation(GenerationIdentity {
            repository,
            parent: None,
            manifest_hash,
            config_hash: configuration_hash,
            provider_set_hash,
            format_version,
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
        let document = NormalizedIrDocument::empty(repository, generation);
        let cancellation = Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(5))
                .expect("deadline is representable"),
        );
        let limits = IrLimits::default();
        let extensions = ExtensionSupport::default();
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let generation =
            IdentityVerifiedGeneration::verify(metadata, document, &limits, &extensions, &context)
                .expect("fixture generation verifies")
                .into_snapshot();
        (generation, cancellation, limits, extensions)
    }

    #[test]
    fn bundle_round_trip_is_canonical_source_bound_and_read_only() {
        let (generation, cancellation, ir_limits, extensions) = fixture();
        let source_set_hash =
            shared_generation_source_set_hash(generation.document()).expect("source set hashes");
        let independently_built = NormalizedIrDocument::empty(
            generation.metadata().repository(),
            GenerationId::from_bytes([9; 20]),
        );
        assert_eq!(
            shared_generation_source_set_hash(&independently_built)
                .expect("independent source set hashes"),
            source_set_hash
        );
        let first = export_shared_generation(
            &generation,
            SharedGenerationLimits::default(),
            &cancellation,
        )
        .expect("generation exports");
        let second = export_shared_generation(
            &generation,
            SharedGenerationLimits::default(),
            &cancellation,
        )
        .expect("generation re-exports");
        assert_eq!(first, second);
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());
        let imported = import_shared_generation(
            &first,
            SharedGenerationExpectation::new(generation.metadata().repository(), source_set_hash)
                .with_generation(generation.metadata().generation()),
            SharedGenerationLimits::default(),
            &ir_limits,
            &extensions,
            &context,
        )
        .expect("generation imports");
        assert_eq!(imported.source_set_hash(), source_set_hash);
        assert_eq!(imported.generation().metadata(), generation.metadata());
        assert_eq!(imported.generation().document(), generation.document());
    }

    #[test]
    fn importer_rejects_wrong_identity_tampering_and_truncation() {
        let (generation, cancellation, ir_limits, extensions) = fixture();
        let encoded = export_shared_generation(
            &generation,
            SharedGenerationLimits::default(),
            &cancellation,
        )
        .expect("generation exports");
        let source_set_hash =
            shared_generation_source_set_hash(generation.document()).expect("source set hashes");
        let context = GenerationContext::new(&cancellation, GenerationBudget::default());

        let wrong_repository =
            SharedGenerationExpectation::new(RepositoryId::from_bytes([8; 16]), source_set_hash);
        assert_eq!(
            import_shared_generation(
                &encoded,
                wrong_repository,
                SharedGenerationLimits::default(),
                &ir_limits,
                &extensions,
                &context,
            )
            .expect_err("wrong repository is rejected"),
            SharedGenerationError::Identity
        );
        let wrong_source = SharedGenerationExpectation::new(
            generation.metadata().repository(),
            content_hash(b"another source set"),
        );
        assert_eq!(
            import_shared_generation(
                &encoded,
                wrong_source,
                SharedGenerationLimits::default(),
                &ir_limits,
                &extensions,
                &context,
            )
            .expect_err("wrong source set is rejected"),
            SharedGenerationError::SourceSet
        );

        let expectation =
            SharedGenerationExpectation::new(generation.metadata().repository(), source_set_hash);
        let mut tampered = encoded.clone();
        let last = tampered.last_mut().expect("bundle has document bytes");
        *last ^= 1;
        assert_eq!(
            import_shared_generation(
                &tampered,
                expectation,
                SharedGenerationLimits::default(),
                &ir_limits,
                &extensions,
                &context,
            )
            .expect_err("artifact tampering is rejected"),
            SharedGenerationError::Inventory
        );
        assert_eq!(
            import_shared_generation(
                &encoded[..PREFIX_BYTES - 1],
                expectation,
                SharedGenerationLimits::default(),
                &ir_limits,
                &extensions,
                &context,
            )
            .expect_err("truncation is rejected"),
            SharedGenerationError::ResourceLimit
        );
    }

    #[test]
    fn limits_and_cancellation_fail_before_import_or_export() {
        assert!(SharedGenerationLimits::new(0, 1).is_err());
        assert!(
            SharedGenerationLimits::new(
                HARD_MAX_SHARED_GENERATION_HEADER_BYTES,
                HARD_MAX_SHARED_GENERATION_BUNDLE_BYTES + 1,
            )
            .is_err()
        );
        let (generation, _live, ir_limits, extensions) = fixture();
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert_eq!(
            export_shared_generation(&generation, SharedGenerationLimits::default(), &cancelled,)
                .expect_err("cancelled export fails"),
            SharedGenerationError::Cancelled
        );
        let context = GenerationContext::new(&cancelled, GenerationBudget::default());
        assert_eq!(
            import_shared_generation(
                b"not-a-bundle",
                SharedGenerationExpectation::new(
                    generation.metadata().repository(),
                    content_hash(b"source"),
                ),
                SharedGenerationLimits::default(),
                &ir_limits,
                &extensions,
                &context,
            )
            .expect_err("cancelled import fails"),
            SharedGenerationError::Cancelled
        );
    }
}
