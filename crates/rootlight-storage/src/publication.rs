//! Canonical publication manifests and recovery classification.
//!
//! This module verifies caller-owned bytes and inventories without performing
//! filesystem mutation, durability, pointer updates, or reclamation.

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, GenerationId, RepositoryId, content_hash};
use serde::{Deserialize, Serialize};

/// Current canonical generation-publication manifest schema.
pub const PUBLICATION_MANIFEST_SCHEMA: &str = "rootlight.generation-publication/1";
/// Current canonical completed-generation marker schema.
pub const PUBLICATION_MARKER_SCHEMA: &str = "rootlight.generation-complete/1";
/// Hard ceiling for artifacts described by one generation.
pub const HARD_MAX_PUBLICATION_ARTIFACTS: u16 = 256;
/// Hard ceiling for one canonical publication manifest.
pub const HARD_MAX_PUBLICATION_MANIFEST_BYTES: u32 = 256 * 1024;
/// Hard ceiling for one canonical completion marker.
pub const HARD_MAX_PUBLICATION_MARKER_BYTES: u16 = 4 * 1024;
/// Hard ceiling for aggregate bytes claimed by one generation manifest.
pub const HARD_MAX_PUBLICATION_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024 * 1024;

const DEFAULT_MAX_PUBLICATION_ARTIFACTS: u16 = 64;
const DEFAULT_MAX_PUBLICATION_MANIFEST_BYTES: u32 = 64 * 1024;
const DEFAULT_MAX_PUBLICATION_MARKER_BYTES: u16 = 1024;
const DEFAULT_MAX_PUBLICATION_ARTIFACT_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Closed artifact identities accepted by the publication contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GenerationArtifactKind {
    /// Normalized SQLite generation oracle.
    Oracle,
    /// Generation-aligned lexical search index.
    SearchIndex,
    /// Bounded source-reference storage.
    SourceReferences,
    /// Bounded read-only Git facts.
    GitFacts,
}

/// Freshness layer represented by one atomically published generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PublicationStage {
    /// Fast structural facts that do not depend on deep context.
    Structural,
    /// Semantic refinement of one exact structural generation.
    Semantic,
}

/// Immutable checksum claim for one closed generation artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationArtifact {
    kind: GenerationArtifactKind,
    byte_length: u64,
    content_hash: ContentHash,
}

impl GenerationArtifact {
    /// Creates a non-empty immutable artifact claim.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError::InvalidArtifact`] when `byte_length` is zero.
    pub fn new(
        kind: GenerationArtifactKind,
        byte_length: u64,
        content_hash: ContentHash,
    ) -> Result<Self, PublicationError> {
        if byte_length == 0 {
            return Err(PublicationError::InvalidArtifact);
        }
        Ok(Self {
            kind,
            byte_length,
            content_hash,
        })
    }

    /// Returns the closed artifact identity.
    #[must_use]
    pub const fn kind(self) -> GenerationArtifactKind {
        self.kind
    }

    /// Returns the exact durable byte length.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }

    /// Returns the checksum of the complete durable bytes.
    #[must_use]
    pub const fn content_hash(self) -> ContentHash {
        self.content_hash
    }
}

/// Validated resource ceilings for manifest decoding and verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationLimits {
    max_artifacts: u16,
    max_manifest_bytes: u32,
    max_marker_bytes: u16,
    max_artifact_bytes: u64,
}

impl PublicationLimits {
    /// Creates positive limits within the hard publication ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError::InvalidLimits`] when any limit is zero or
    /// exceeds its corresponding hard ceiling.
    pub fn new(
        max_artifacts: u16,
        max_manifest_bytes: u32,
        max_marker_bytes: u16,
        max_artifact_bytes: u64,
    ) -> Result<Self, PublicationError> {
        if max_artifacts == 0
            || max_artifacts > HARD_MAX_PUBLICATION_ARTIFACTS
            || max_manifest_bytes == 0
            || max_manifest_bytes > HARD_MAX_PUBLICATION_MANIFEST_BYTES
            || max_marker_bytes == 0
            || max_marker_bytes > HARD_MAX_PUBLICATION_MARKER_BYTES
            || max_artifact_bytes == 0
            || max_artifact_bytes > HARD_MAX_PUBLICATION_ARTIFACT_BYTES
        {
            return Err(PublicationError::InvalidLimits);
        }
        Ok(Self {
            max_artifacts,
            max_manifest_bytes,
            max_marker_bytes,
            max_artifact_bytes,
        })
    }

    /// Returns the maximum artifact count.
    #[must_use]
    pub const fn max_artifacts(self) -> u16 {
        self.max_artifacts
    }

    /// Returns the maximum canonical manifest size.
    #[must_use]
    pub const fn max_manifest_bytes(self) -> u32 {
        self.max_manifest_bytes
    }

    /// Returns the maximum canonical marker size.
    #[must_use]
    pub const fn max_marker_bytes(self) -> u16 {
        self.max_marker_bytes
    }

    /// Returns the maximum aggregate artifact size.
    #[must_use]
    pub const fn max_artifact_bytes(self) -> u64 {
        self.max_artifact_bytes
    }
}

impl Default for PublicationLimits {
    fn default() -> Self {
        Self {
            max_artifacts: DEFAULT_MAX_PUBLICATION_ARTIFACTS,
            max_manifest_bytes: DEFAULT_MAX_PUBLICATION_MANIFEST_BYTES,
            max_marker_bytes: DEFAULT_MAX_PUBLICATION_MARKER_BYTES,
            max_artifact_bytes: DEFAULT_MAX_PUBLICATION_ARTIFACT_BYTES,
        }
    }
}

/// Canonical source-free description of one immutable generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationManifest {
    schema: String,
    repository: RepositoryId,
    generation: GenerationId,
    parent: Option<GenerationId>,
    stage: PublicationStage,
    refines: Option<GenerationId>,
    artifacts: Vec<GenerationArtifact>,
    artifact_bytes: u64,
}

impl PublicationManifest {
    /// Creates a canonical manifest from untrusted artifact claims.
    ///
    /// A semantic generation must name its structural predecessor as both
    /// `parent` and `refines`; a structural generation cannot refine another
    /// generation. Artifacts are sorted by their closed identity.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for cancellation, invalid lineage, empty,
    /// duplicate, oversized, or over-budget artifact claims, or an encoded
    /// manifest above the configured ceiling.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repository: RepositoryId,
        generation: GenerationId,
        parent: Option<GenerationId>,
        stage: PublicationStage,
        refines: Option<GenerationId>,
        artifacts: Vec<GenerationArtifact>,
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, PublicationError> {
        cancellation.check()?;
        validate_lineage(generation, parent, stage, refines)?;
        let (artifacts, artifact_bytes) =
            validate_artifacts(artifacts, limits, false, cancellation)?;
        let manifest = Self {
            schema: PUBLICATION_MANIFEST_SCHEMA.to_owned(),
            repository,
            generation,
            parent,
            stage,
            refines,
            artifacts,
            artifact_bytes,
        };
        let _ = manifest.canonical_json(limits, cancellation)?;
        Ok(manifest)
    }

    /// Decodes and revalidates exact canonical manifest bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for cancellation, an oversized input,
    /// invalid or unknown JSON, an unsupported schema, non-canonical encoding,
    /// invalid lineage, or invalid artifact claims.
    pub fn decode(
        encoded: &[u8],
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, PublicationError> {
        cancellation.check()?;
        if encoded.len()
            > usize::try_from(limits.max_manifest_bytes)
                .map_err(|_| PublicationError::InvalidLimits)?
        {
            return Err(PublicationError::ManifestTooLarge);
        }
        let decoded: Self =
            serde_json::from_slice(encoded).map_err(|_| PublicationError::InvalidManifest)?;
        if decoded.schema != PUBLICATION_MANIFEST_SCHEMA {
            return Err(PublicationError::UnsupportedManifestSchema);
        }
        let rebuilt = Self::new(
            decoded.repository,
            decoded.generation,
            decoded.parent,
            decoded.stage,
            decoded.refines,
            decoded.artifacts,
            limits,
            cancellation,
        )?;
        if decoded.artifact_bytes != rebuilt.artifact_bytes {
            return Err(PublicationError::InvalidManifest);
        }
        let canonical = rebuilt.canonical_json(limits, cancellation)?;
        if canonical != encoded {
            return Err(PublicationError::NonCanonicalManifest);
        }
        Ok(rebuilt)
    }

    /// Encodes the deterministic compact JSON representation.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for cancellation, serialization failure, or
    /// output above the configured manifest ceiling.
    pub fn canonical_json(
        &self,
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, PublicationError> {
        cancellation.check()?;
        let encoded = serde_json::to_vec(self).map_err(|_| PublicationError::Encode)?;
        cancellation.check()?;
        if encoded.len()
            > usize::try_from(limits.max_manifest_bytes)
                .map_err(|_| PublicationError::InvalidLimits)?
        {
            return Err(PublicationError::ManifestTooLarge);
        }
        Ok(encoded)
    }

    /// Computes the checksum bound by the completion marker.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] when canonical encoding fails or is
    /// cancelled.
    pub fn canonical_hash(
        &self,
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<ContentHash, PublicationError> {
        self.canonical_json(limits, cancellation)
            .map(|encoded| content_hash(&encoded))
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

    /// Returns the previous active generation, when present.
    #[must_use]
    pub const fn parent(&self) -> Option<GenerationId> {
        self.parent
    }

    /// Returns the published freshness layer.
    #[must_use]
    pub const fn stage(&self) -> PublicationStage {
        self.stage
    }

    /// Returns the structural generation refined by this semantic generation.
    #[must_use]
    pub const fn refines(&self) -> Option<GenerationId> {
        self.refines
    }

    /// Returns canonical artifact claims in closed-identity order.
    #[must_use]
    pub fn artifacts(&self) -> &[GenerationArtifact] {
        &self.artifacts
    }

    /// Returns the checked aggregate artifact byte count.
    #[must_use]
    pub const fn artifact_bytes(&self) -> u64 {
        self.artifact_bytes
    }
}

/// Canonical marker written only after every manifest artifact is durable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationMarker {
    schema: &'static str,
    generation: GenerationId,
    manifest_hash: ContentHash,
}

impl PublicationMarker {
    /// Binds a completion marker to one exact canonical manifest.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] when manifest hashing is cancelled or fails.
    pub fn for_manifest(
        manifest: &PublicationManifest,
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, PublicationError> {
        Ok(Self {
            schema: PUBLICATION_MARKER_SCHEMA,
            generation: manifest.generation,
            manifest_hash: manifest.canonical_hash(limits, cancellation)?,
        })
    }

    /// Decodes and revalidates exact canonical marker bytes.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for cancellation, oversized or invalid
    /// input, an unsupported schema, or non-canonical encoding.
    pub fn decode(
        encoded: &[u8],
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, PublicationError> {
        cancellation.check()?;
        if encoded.len() > usize::from(limits.max_marker_bytes) {
            return Err(PublicationError::MarkerTooLarge);
        }
        let decoded: PublicationMarkerWire =
            serde_json::from_slice(encoded).map_err(|_| PublicationError::InvalidMarker)?;
        if decoded.schema != PUBLICATION_MARKER_SCHEMA {
            return Err(PublicationError::UnsupportedMarkerSchema);
        }
        let marker = Self {
            schema: PUBLICATION_MARKER_SCHEMA,
            generation: decoded.generation,
            manifest_hash: decoded.manifest_hash,
        };
        let canonical = marker.canonical_json(limits, cancellation)?;
        if canonical != encoded {
            return Err(PublicationError::NonCanonicalMarker);
        }
        Ok(marker)
    }

    /// Encodes the deterministic compact JSON representation.
    ///
    /// # Errors
    ///
    /// Returns [`PublicationError`] for cancellation, serialization failure, or
    /// output above the configured marker ceiling.
    pub fn canonical_json(
        &self,
        limits: PublicationLimits,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, PublicationError> {
        cancellation.check()?;
        let encoded = serde_json::to_vec(self).map_err(|_| PublicationError::Encode)?;
        cancellation.check()?;
        if encoded.len() > usize::from(limits.max_marker_bytes) {
            return Err(PublicationError::MarkerTooLarge);
        }
        Ok(encoded)
    }

    /// Returns the completed generation identity.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the exact manifest checksum.
    #[must_use]
    pub const fn manifest_hash(self) -> ContentHash {
        self.manifest_hash
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublicationMarkerWire {
    schema: String,
    generation: GenerationId,
    manifest_hash: ContentHash,
}

/// Expected directory identity supplied by the caller-owned recovery layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationExpectation {
    repository: RepositoryId,
    generation: GenerationId,
}

impl PublicationExpectation {
    /// Creates an expected repository and generation identity.
    #[must_use]
    pub const fn new(repository: RepositoryId, generation: GenerationId) -> Self {
        Self {
            repository,
            generation,
        }
    }
}

/// Opaque proof that manifest, marker, identity, and artifact inventory agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedPublication {
    manifest: PublicationManifest,
    marker: PublicationMarker,
}

impl VerifiedPublication {
    /// Returns the verified canonical manifest.
    #[must_use]
    pub const fn manifest(&self) -> &PublicationManifest {
        &self.manifest
    }

    /// Returns the verified completion marker.
    #[must_use]
    pub const fn marker(&self) -> PublicationMarker {
        self.marker
    }
}

/// Verifies a complete candidate before an external catalog pointer update.
///
/// # Errors
///
/// Returns [`PublicationError`] for cancellation, malformed or non-canonical
/// bytes, identity drift, checksum drift, or an incomplete artifact inventory.
pub fn verify_publication(
    expectation: PublicationExpectation,
    manifest_bytes: &[u8],
    marker_bytes: &[u8],
    observed_artifacts: Vec<GenerationArtifact>,
    limits: PublicationLimits,
    cancellation: &Cancellation,
) -> Result<VerifiedPublication, PublicationError> {
    cancellation.check()?;
    let manifest = PublicationManifest::decode(manifest_bytes, limits, cancellation)?;
    if manifest.repository != expectation.repository
        || manifest.generation != expectation.generation
    {
        return Err(PublicationError::IdentityMismatch);
    }
    let marker = PublicationMarker::decode(marker_bytes, limits, cancellation)?;
    if marker.generation != manifest.generation {
        return Err(PublicationError::IdentityMismatch);
    }
    if marker.manifest_hash != manifest.canonical_hash(limits, cancellation)? {
        return Err(PublicationError::ManifestDigestMismatch);
    }
    let (observed_artifacts, observed_bytes) =
        validate_artifacts(observed_artifacts, limits, false, cancellation)?;
    if observed_artifacts != manifest.artifacts || observed_bytes != manifest.artifact_bytes {
        return Err(PublicationError::ArtifactInventoryMismatch);
    }
    Ok(VerifiedPublication { manifest, marker })
}

/// Deterministic startup disposition for one caller-owned generation location.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RecoveryState {
    /// No manifest, marker, or artifact exists.
    Absent,
    /// Work exists without a completion marker and may be discarded or resumed.
    Incomplete,
    /// Every immutable byte and identity claim is verified.
    Ready(Box<VerifiedPublication>),
    /// A completed or partially described candidate contradicts its contract.
    Corrupt(PublicationCorruption),
}

/// Source-free corruption classes safe for recovery diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PublicationCorruption {
    /// Manifest bytes are invalid, unsupported, or non-canonical.
    InvalidManifest,
    /// Marker bytes are invalid, unsupported, or non-canonical.
    InvalidMarker,
    /// Repository or generation identity differs from its location.
    IdentityMismatch,
    /// Marker checksum differs from the canonical manifest.
    ManifestDigestMismatch,
    /// Observed artifacts are duplicated, unexpected, changed, or incomplete.
    ArtifactInventoryMismatch,
    /// A bounded publication resource ceiling was exceeded.
    ResourceLimit,
}

/// Classifies one startup candidate without mutating caller-owned storage.
///
/// Missing artifacts before the marker are incomplete. Once the marker exists,
/// the same absence is corruption because completed generations are immutable.
///
/// # Errors
///
/// Returns [`Cancelled`] only when the cooperative cancellation token stops
/// classification. All hostile-data failures become a source-free corrupt state.
pub fn classify_recovery(
    expectation: PublicationExpectation,
    manifest_bytes: Option<&[u8]>,
    marker_bytes: Option<&[u8]>,
    observed_artifacts: Vec<GenerationArtifact>,
    limits: PublicationLimits,
    cancellation: &Cancellation,
) -> Result<RecoveryState, Cancelled> {
    cancellation.check()?;
    match (manifest_bytes, marker_bytes) {
        (None, None) if observed_artifacts.is_empty() => Ok(RecoveryState::Absent),
        (None, None) => Ok(RecoveryState::Incomplete),
        (None, Some(_)) => Ok(RecoveryState::Corrupt(
            PublicationCorruption::InvalidManifest,
        )),
        (Some(manifest_bytes), None) => {
            let manifest = match PublicationManifest::decode(manifest_bytes, limits, cancellation) {
                Ok(manifest) => manifest,
                Err(PublicationError::Cancelled(cancelled)) => return Err(cancelled),
                Err(error) => return Ok(RecoveryState::Corrupt(corruption_for(error))),
            };
            if manifest.repository != expectation.repository
                || manifest.generation != expectation.generation
            {
                return Ok(RecoveryState::Corrupt(
                    PublicationCorruption::IdentityMismatch,
                ));
            }
            match validate_partial_inventory(&manifest, observed_artifacts, limits, cancellation) {
                Ok(()) => Ok(RecoveryState::Incomplete),
                Err(PublicationError::Cancelled(cancelled)) => Err(cancelled),
                Err(error) => Ok(RecoveryState::Corrupt(corruption_for(error))),
            }
        }
        (Some(manifest_bytes), Some(marker_bytes)) => match verify_publication(
            expectation,
            manifest_bytes,
            marker_bytes,
            observed_artifacts,
            limits,
            cancellation,
        ) {
            Ok(verified) => Ok(RecoveryState::Ready(Box::new(verified))),
            Err(PublicationError::Cancelled(cancelled)) => Err(cancelled),
            Err(error) => Ok(RecoveryState::Corrupt(corruption_for(error))),
        },
    }
}

fn validate_lineage(
    generation: GenerationId,
    parent: Option<GenerationId>,
    stage: PublicationStage,
    refines: Option<GenerationId>,
) -> Result<(), PublicationError> {
    if parent == Some(generation) {
        return Err(PublicationError::InvalidLineage);
    }
    match stage {
        PublicationStage::Structural if refines.is_none() => Ok(()),
        PublicationStage::Semantic
            if refines.is_some() && parent == refines && refines != Some(generation) =>
        {
            Ok(())
        }
        PublicationStage::Structural | PublicationStage::Semantic => {
            Err(PublicationError::InvalidLineage)
        }
    }
}

fn validate_artifacts(
    mut artifacts: Vec<GenerationArtifact>,
    limits: PublicationLimits,
    allow_empty: bool,
    cancellation: &Cancellation,
) -> Result<(Vec<GenerationArtifact>, u64), PublicationError> {
    cancellation.check()?;
    if (!allow_empty && artifacts.is_empty()) || artifacts.len() > usize::from(limits.max_artifacts)
    {
        return Err(PublicationError::ArtifactCount);
    }
    artifacts.sort_unstable_by_key(|artifact| artifact.kind);
    let mut previous = None;
    let mut total = 0_u64;
    for artifact in &artifacts {
        cancellation.check()?;
        if artifact.byte_length == 0 {
            return Err(PublicationError::InvalidArtifact);
        }
        if previous == Some(artifact.kind) {
            return Err(PublicationError::DuplicateArtifact);
        }
        previous = Some(artifact.kind);
        total = total
            .checked_add(artifact.byte_length)
            .ok_or(PublicationError::ArtifactBytesExceeded)?;
        if total > limits.max_artifact_bytes {
            return Err(PublicationError::ArtifactBytesExceeded);
        }
    }
    Ok((artifacts, total))
}

fn validate_partial_inventory(
    manifest: &PublicationManifest,
    observed_artifacts: Vec<GenerationArtifact>,
    limits: PublicationLimits,
    cancellation: &Cancellation,
) -> Result<(), PublicationError> {
    let (observed_artifacts, _) =
        validate_artifacts(observed_artifacts, limits, true, cancellation)?;
    for observed in observed_artifacts {
        cancellation.check()?;
        match manifest
            .artifacts
            .binary_search_by_key(&observed.kind, |artifact| artifact.kind)
        {
            Ok(index) if manifest.artifacts[index] == observed => {}
            Ok(_) | Err(_) => return Err(PublicationError::ArtifactInventoryMismatch),
        }
    }
    Ok(())
}

fn corruption_for(error: PublicationError) -> PublicationCorruption {
    match error {
        PublicationError::InvalidMarker
        | PublicationError::UnsupportedMarkerSchema
        | PublicationError::NonCanonicalMarker
        | PublicationError::MarkerTooLarge => PublicationCorruption::InvalidMarker,
        PublicationError::IdentityMismatch | PublicationError::InvalidLineage => {
            PublicationCorruption::IdentityMismatch
        }
        PublicationError::ManifestDigestMismatch => PublicationCorruption::ManifestDigestMismatch,
        PublicationError::ArtifactInventoryMismatch
        | PublicationError::InvalidArtifact
        | PublicationError::DuplicateArtifact => PublicationCorruption::ArtifactInventoryMismatch,
        PublicationError::ArtifactCount
        | PublicationError::ArtifactBytesExceeded
        | PublicationError::ManifestTooLarge
        | PublicationError::InvalidLimits => PublicationCorruption::ResourceLimit,
        PublicationError::InvalidManifest
        | PublicationError::UnsupportedManifestSchema
        | PublicationError::NonCanonicalManifest
        | PublicationError::Encode => PublicationCorruption::InvalidManifest,
        PublicationError::Cancelled(_) => PublicationCorruption::ResourceLimit,
    }
}

/// Invalid publication input or a stopped verification operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PublicationError {
    /// Cooperative cancellation stopped verification.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// A configured publication limit was zero or above its hard ceiling.
    #[error("publication limits are invalid")]
    InvalidLimits,
    /// An artifact claim described an empty artifact.
    #[error("publication artifact is invalid")]
    InvalidArtifact,
    /// The artifact collection was empty or above its configured count limit.
    #[error("publication artifact count is invalid")]
    ArtifactCount,
    /// Two artifact claims used the same closed identity.
    #[error("publication artifact identity is duplicated")]
    DuplicateArtifact,
    /// Aggregate artifact bytes overflowed or exceeded the configured limit.
    #[error("publication artifact bytes exceed the configured limit")]
    ArtifactBytesExceeded,
    /// Structural or semantic generation lineage was contradictory.
    #[error("publication lineage is invalid")]
    InvalidLineage,
    /// Manifest JSON was invalid or contained an unknown field.
    #[error("publication manifest is invalid")]
    InvalidManifest,
    /// Manifest schema is not supported by this binary.
    #[error("publication manifest schema is unsupported")]
    UnsupportedManifestSchema,
    /// Manifest bytes were valid JSON but not the exact canonical encoding.
    #[error("publication manifest is not canonical")]
    NonCanonicalManifest,
    /// Manifest bytes exceeded the configured decode ceiling.
    #[error("publication manifest exceeds the configured limit")]
    ManifestTooLarge,
    /// Marker JSON was invalid or contained an unknown field.
    #[error("publication marker is invalid")]
    InvalidMarker,
    /// Marker schema is not supported by this binary.
    #[error("publication marker schema is unsupported")]
    UnsupportedMarkerSchema,
    /// Marker bytes were valid JSON but not the exact canonical encoding.
    #[error("publication marker is not canonical")]
    NonCanonicalMarker,
    /// Marker bytes exceeded the configured decode ceiling.
    #[error("publication marker exceeds the configured limit")]
    MarkerTooLarge,
    /// Repository or generation identity differed from the expected location.
    #[error("publication identity does not match its location")]
    IdentityMismatch,
    /// Marker checksum did not bind the exact canonical manifest.
    #[error("publication marker does not match the manifest")]
    ManifestDigestMismatch,
    /// Durable artifact observations did not exactly match the manifest.
    #[error("publication artifact inventory does not match the manifest")]
    ArtifactInventoryMismatch,
    /// Canonical JSON serialization failed.
    #[error("publication encoding failed")]
    Encode,
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;

    use super::*;

    fn repository() -> RepositoryId {
        RepositoryId::from_bytes([7; 16])
    }

    fn generation(byte: u8) -> GenerationId {
        GenerationId::from_bytes([byte; 20])
    }

    fn artifact(kind: GenerationArtifactKind, byte: u8) -> GenerationArtifact {
        GenerationArtifact::new(kind, u64::from(byte) + 1, content_hash(&[byte]))
            .expect("fixture artifact is non-empty")
    }

    fn manifest(
        stage: PublicationStage,
        parent: Option<GenerationId>,
        refines: Option<GenerationId>,
    ) -> PublicationManifest {
        PublicationManifest::new(
            repository(),
            generation(3),
            parent,
            stage,
            refines,
            vec![
                artifact(GenerationArtifactKind::SearchIndex, 2),
                artifact(GenerationArtifactKind::Oracle, 1),
            ],
            PublicationLimits::default(),
            &Cancellation::new(),
        )
        .expect("fixture manifest is valid")
    }

    #[test]
    fn manifest_is_canonical_and_binds_two_stage_lineage() {
        let parent = generation(2);
        let structural = manifest(PublicationStage::Structural, Some(parent), None);
        assert_eq!(
            structural
                .artifacts()
                .iter()
                .map(|artifact| artifact.kind())
                .collect::<Vec<_>>(),
            vec![
                GenerationArtifactKind::Oracle,
                GenerationArtifactKind::SearchIndex
            ]
        );

        let semantic = manifest(PublicationStage::Semantic, Some(parent), Some(parent));
        assert_eq!(semantic.refines(), Some(parent));
        assert_eq!(
            semantic
                .canonical_hash(PublicationLimits::default(), &Cancellation::new())
                .expect("canonical manifest hashes"),
            content_hash(
                &semantic
                    .canonical_json(PublicationLimits::default(), &Cancellation::new())
                    .expect("canonical manifest encodes")
            )
        );

        let error = PublicationManifest::new(
            repository(),
            generation(3),
            Some(parent),
            PublicationStage::Semantic,
            None,
            vec![artifact(GenerationArtifactKind::Oracle, 1)],
            PublicationLimits::default(),
            &Cancellation::new(),
        )
        .expect_err("semantic publication requires an exact refinement");
        assert_eq!(error, PublicationError::InvalidLineage);
    }

    #[test]
    fn manifest_decode_rejects_noncanonical_unknown_and_oversized_input() {
        let limits = PublicationLimits::default();
        let cancellation = Cancellation::new();
        let manifest = manifest(PublicationStage::Structural, None, None);
        let canonical = manifest
            .canonical_json(limits, &cancellation)
            .expect("fixture manifest encodes");
        assert_eq!(
            PublicationManifest::decode(&canonical, limits, &cancellation)
                .expect("canonical manifest decodes"),
            manifest
        );

        let mut spaced = canonical.clone();
        spaced.push(b'\n');
        assert_eq!(
            PublicationManifest::decode(&spaced, limits, &cancellation),
            Err(PublicationError::NonCanonicalManifest)
        );

        let mut value: serde_json::Value =
            serde_json::from_slice(&canonical).expect("fixture JSON decodes");
        value
            .as_object_mut()
            .expect("fixture is an object")
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        let unknown = serde_json::to_vec(&value).expect("mutated fixture encodes");
        assert_eq!(
            PublicationManifest::decode(&unknown, limits, &cancellation),
            Err(PublicationError::InvalidManifest)
        );

        let tiny = PublicationLimits::new(8, 16, 128, 1024).expect("tiny limits are valid");
        assert_eq!(
            PublicationManifest::decode(&canonical, tiny, &cancellation),
            Err(PublicationError::ManifestTooLarge)
        );
    }

    #[test]
    fn completion_requires_exact_manifest_and_artifact_inventory() {
        let limits = PublicationLimits::default();
        let cancellation = Cancellation::new();
        let candidate = manifest(PublicationStage::Structural, None, None);
        let manifest_bytes = candidate
            .canonical_json(limits, &cancellation)
            .expect("fixture manifest encodes");
        let marker = PublicationMarker::for_manifest(&candidate, limits, &cancellation)
            .expect("fixture marker builds");
        let marker_bytes = marker
            .canonical_json(limits, &cancellation)
            .expect("fixture marker encodes");
        let expectation = PublicationExpectation::new(repository(), generation(3));

        let verified = verify_publication(
            expectation,
            &manifest_bytes,
            &marker_bytes,
            candidate.artifacts().to_vec(),
            limits,
            &cancellation,
        )
        .expect("exact candidate verifies");
        assert_eq!(verified.manifest(), &candidate);

        assert_eq!(
            verify_publication(
                expectation,
                &manifest_bytes,
                &marker_bytes,
                vec![artifact(GenerationArtifactKind::Oracle, 1)],
                limits,
                &cancellation,
            ),
            Err(PublicationError::ArtifactInventoryMismatch)
        );

        let other = manifest(PublicationStage::Structural, Some(generation(1)), None);
        let other_bytes = other
            .canonical_json(limits, &cancellation)
            .expect("other manifest encodes");
        assert_eq!(
            verify_publication(
                expectation,
                &other_bytes,
                &marker_bytes,
                other.artifacts().to_vec(),
                limits,
                &cancellation,
            ),
            Err(PublicationError::ManifestDigestMismatch)
        );
    }

    #[test]
    fn recovery_distinguishes_absent_incomplete_ready_and_corrupt() {
        let limits = PublicationLimits::default();
        let cancellation = Cancellation::new();
        let expectation = PublicationExpectation::new(repository(), generation(3));
        assert_eq!(
            classify_recovery(expectation, None, None, Vec::new(), limits, &cancellation)
                .expect("empty recovery classifies"),
            RecoveryState::Absent
        );

        let manifest = manifest(PublicationStage::Structural, None, None);
        let manifest_bytes = manifest
            .canonical_json(limits, &cancellation)
            .expect("fixture manifest encodes");
        assert_eq!(
            classify_recovery(
                expectation,
                Some(&manifest_bytes),
                None,
                vec![manifest.artifacts()[0]],
                limits,
                &cancellation,
            )
            .expect("partial recovery classifies"),
            RecoveryState::Incomplete
        );

        let marker = PublicationMarker::for_manifest(&manifest, limits, &cancellation)
            .expect("fixture marker builds")
            .canonical_json(limits, &cancellation)
            .expect("fixture marker encodes");
        assert!(matches!(
            classify_recovery(
                expectation,
                Some(&manifest_bytes),
                Some(&marker),
                manifest.artifacts().to_vec(),
                limits,
                &cancellation,
            )
            .expect("ready recovery classifies"),
            RecoveryState::Ready(_)
        ));
        assert_eq!(
            classify_recovery(
                expectation,
                Some(&manifest_bytes),
                Some(&marker),
                vec![manifest.artifacts()[0]],
                limits,
                &cancellation,
            )
            .expect("corrupt recovery classifies"),
            RecoveryState::Corrupt(PublicationCorruption::ArtifactInventoryMismatch)
        );
        assert_eq!(
            classify_recovery(
                expectation,
                None,
                Some(&marker),
                manifest.artifacts().to_vec(),
                limits,
                &cancellation,
            )
            .expect("marker without manifest classifies"),
            RecoveryState::Corrupt(PublicationCorruption::InvalidManifest)
        );
    }

    #[test]
    fn verification_observes_cancellation_before_decode() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let result = classify_recovery(
            PublicationExpectation::new(repository(), generation(3)),
            Some(b"not-json"),
            None,
            Vec::new(),
            PublicationLimits::default(),
            &cancellation,
        );
        assert_eq!(
            result.expect_err("cancellation wins"),
            cancellation.check().expect_err("token remains cancelled")
        );
    }

    #[test]
    fn artifact_limits_reject_duplicates_and_aggregate_overflow() {
        let limits = PublicationLimits::new(2, 4096, 512, 10).expect("fixture limits are valid");
        let cancellation = Cancellation::new();
        let duplicate = PublicationManifest::new(
            repository(),
            generation(3),
            None,
            PublicationStage::Structural,
            None,
            vec![
                artifact(GenerationArtifactKind::Oracle, 1),
                artifact(GenerationArtifactKind::Oracle, 2),
            ],
            limits,
            &cancellation,
        );
        assert_eq!(duplicate, Err(PublicationError::DuplicateArtifact));

        let too_large = PublicationManifest::new(
            repository(),
            generation(3),
            None,
            PublicationStage::Structural,
            None,
            vec![
                GenerationArtifact::new(GenerationArtifactKind::Oracle, 6, content_hash(b"one"))
                    .expect("fixture artifact is valid"),
                GenerationArtifact::new(
                    GenerationArtifactKind::SearchIndex,
                    6,
                    content_hash(b"two"),
                )
                .expect("fixture artifact is valid"),
            ],
            limits,
            &cancellation,
        );
        assert_eq!(too_large, Err(PublicationError::ArtifactBytesExceeded));
    }
}
