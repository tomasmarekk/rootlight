//! Verification and rollback orchestration for opt-in offline update inputs.
//!
//! Metadata signatures cover the exact bounded JSON bytes. Artifact hashing is
//! streamed, and activation remains behind a platform installer boundary.

use std::{
    collections::BTreeSet,
    io::{self, Cursor, Read},
};

use data_encoding::BASE64;
use ed25519_compact::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

mod filesystem;

pub use filesystem::{
    ACTIVE_VERSION_FILE, CandidateHealthCheck, CandidateHealthError, FilesystemUpdateError,
    FilesystemUpdateOutcome, INSTALL_MANIFEST_FILE, PackageInstallOutcome, PackageUninstallOutcome,
    ProcessCandidateHealthCheck, TrustedUpdatePolicy, UPDATE_HEALTH_STATE_DIR_ENV,
    UPDATE_LOCK_FILE, UPDATE_POLICY_FILE, UPDATE_TRANSACTION_FILE, UpdateInputPaths,
    UpdateRuntimeStatus, UpdateTransactionPhase, apply_update_package,
    apply_update_package_with_policy, install_package_with_policy, recover_update,
    uninstall_package, update_runtime_status,
};

/// Schema version for signed update metadata.
pub const UPDATE_METADATA_SCHEMA_VERSION: &str = "1.0";
/// Maximum accepted signed metadata size.
pub const MAX_UPDATE_METADATA_BYTES: usize = 64 * 1024;
/// Maximum accepted update artifact size.
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Maximum accepted artifact-specific CycloneDX SBOM size.
pub const MAX_UPDATE_SBOM_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum accepted signed provenance bundle size.
pub const MAX_UPDATE_PROVENANCE_BYTES: u64 = 16 * 1024 * 1024;
/// Maximum accepted license and notice bundle size.
pub const MAX_UPDATE_LICENSE_BUNDLE_BYTES: u64 = 128 * 1024 * 1024;

const SHA256_HEX_BYTES: usize = 64;
const PUBLIC_KEY_HEX_BYTES: usize = 64;
const SIGNATURE_HEX_BYTES: usize = 128;
const MAX_UPDATE_LABEL_BYTES: usize = 128;
const MAX_ARTIFACT_NAME_BYTES: usize = 255;
const MAX_HEALTH_TIMEOUT_SECONDS: u32 = 300;
const MIN_HEALTH_TIMEOUT_SECONDS: u32 = 1;
const UPDATE_STAGING_OVERHEAD_BYTES: u64 = 16 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const ARTIFACT_SIGNATURE_DOMAIN: &[u8] = b"rootlight.release-artifact-signature/1\0";
const CYCLONEDX_TARGET_PROPERTY: &str = "cdx:rustc:sbom:target:triple";
const ROOTLIGHT_SOURCE_REVISION_PROPERTY: &str = "rootlight:source:revision";
const ROOTLIGHT_TARGET_PROPERTY: &str = "rootlight:target:triple";
const SLSA_PROVENANCE_V1: &str = "https://slsa.dev/provenance/v1";
const IN_TOTO_STATEMENT_V1: &str = "https://in-toto.io/Statement/v1";

/// Trusted Ed25519 public key used to verify update metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdatePublicKey([u8; 32]);

impl UpdatePublicKey {
    /// Decodes an exact lowercase hexadecimal public key.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError::InvalidPublicKey`] for a non-canonical key.
    pub fn from_hex(value: &str) -> Result<Self, UpdateError> {
        decode_hex_array::<32>(value, PUBLIC_KEY_HEX_BYTES)
            .map(Self)
            .ok_or(UpdateError::InvalidPublicKey)
    }

    /// Returns the raw public key bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Detached Ed25519 signature over exact metadata bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachedUpdateSignature([u8; 64]);

impl DetachedUpdateSignature {
    /// Decodes an exact lowercase hexadecimal detached signature.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError::InvalidSignatureEncoding`] for non-canonical input.
    pub fn from_hex(value: &str) -> Result<Self, UpdateError> {
        decode_hex_array::<64>(value, SIGNATURE_HEX_BYTES)
            .map(Self)
            .ok_or(UpdateError::InvalidSignatureEncoding)
    }

    /// Returns the raw signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

/// Detached Ed25519 signature over the domain-separated release artifact identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DetachedArtifactSignature([u8; 64]);

impl DetachedArtifactSignature {
    /// Decodes an exact lowercase hexadecimal artifact signature.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateError::InvalidArtifactSignatureEncoding`] for
    /// non-canonical input.
    pub fn from_hex(value: &str) -> Result<Self, UpdateError> {
        decode_hex_array::<64>(value, SIGNATURE_HEX_BYTES)
            .map(Self)
            .ok_or(UpdateError::InvalidArtifactSignatureEncoding)
    }

    /// Returns the raw signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

/// Signatures and trusted key that authorize one release update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSignatures {
    metadata: DetachedUpdateSignature,
    artifact: DetachedArtifactSignature,
    public_key: UpdatePublicKey,
}

impl UpdateSignatures {
    /// Creates the signature set used by complete release verification.
    #[must_use]
    pub const fn new(
        metadata: DetachedUpdateSignature,
        artifact: DetachedArtifactSignature,
        public_key: UpdatePublicKey,
    ) -> Self {
        Self {
            metadata,
            artifact,
            public_key,
        }
    }
}

/// Bounded supporting release evidence consumed during update verification.
pub struct UpdateSupportingEvidence<'a> {
    sbom: &'a mut dyn Read,
    provenance: &'a mut dyn Read,
    license_bundle: &'a mut dyn Read,
}

impl<'a> UpdateSupportingEvidence<'a> {
    /// Groups the artifact-specific SBOM, provenance, and license streams.
    #[must_use]
    pub fn new(
        sbom: &'a mut dyn Read,
        provenance: &'a mut dyn Read,
        license_bundle: &'a mut dyn Read,
    ) -> Self {
        Self {
            sbom,
            provenance,
            license_bundle,
        }
    }
}

/// Reproducibility statement attached to one release artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproducibilityLevel {
    /// Independent builds produced identical bytes.
    BitForBit,
    /// Documented normalization produced identical content.
    Normalized,
    /// Only source and build-input provenance is currently reproducible.
    ProvenanceOnly,
}

/// Signed artifact identity and resource bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactMetadata {
    /// Safe basename of the artifact supplied by the user or release tooling.
    pub file_name: String,
    /// Lowercase SHA-256 of the artifact bytes.
    pub sha256: String,
    /// Exact artifact byte count.
    pub size_bytes: u64,
    /// Lowercase SHA-256 of the artifact-specific CycloneDX SBOM.
    pub sbom_sha256: String,
    /// Lowercase SHA-256 of the artifact provenance statement.
    pub provenance_sha256: String,
    /// Lowercase SHA-256 of the license and notice bundle.
    pub license_bundle_sha256: String,
    /// Observed reproducibility level for this artifact.
    pub reproducibility: ReproducibilityLevel,
}

/// Compatibility and rollback requirements carried by signed metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateCompatibility {
    /// Lowest catalog schema accepted by the candidate release.
    pub minimum_catalog_schema: u32,
    /// Highest catalog schema accepted by the candidate release.
    pub maximum_catalog_schema: u32,
    /// Required local protocol major.
    pub protocol_major: u32,
    /// Lowest local protocol minor accepted by the candidate release.
    pub minimum_protocol_minor: u32,
    /// Highest local protocol minor accepted by the candidate release.
    pub maximum_protocol_minor: u32,
    /// Additional migration workspace required before activation.
    pub migration_required_bytes: u64,
    /// Whether the candidate supports restoring the current catalog and binary.
    pub rollback_supported: bool,
    /// Bounded health-check deadline after activation.
    pub health_timeout_seconds: u32,
}

/// Strict signed metadata for one platform release artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateMetadata {
    /// Update metadata schema version.
    pub schema_version: String,
    /// Trusted-key selector that contains no key material.
    pub key_id: String,
    /// Semantic release version.
    pub version: String,
    /// Release channel, such as `stable`.
    pub channel: String,
    /// Rust target operating-system component.
    pub platform: String,
    /// Rust target architecture component.
    pub architecture: String,
    /// Earliest Unix second at which this metadata is valid.
    pub valid_from_unix_seconds: u64,
    /// Unix second at which this metadata expires.
    pub expires_unix_seconds: u64,
    /// Percentage of deterministic installation buckets currently enabled.
    pub rollout_percentage: u8,
    /// Artifact identity and supporting release evidence.
    pub artifact: ArtifactMetadata,
    /// Required runtime, storage, migration, and rollback compatibility.
    pub compatibility: UpdateCompatibility,
}

/// Trusted local state used to evaluate signed update metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateContext {
    /// Updates remain disabled unless the user opts in.
    pub updates_enabled: bool,
    /// Currently active semantic version.
    pub current_version: String,
    /// Last known-good semantic version retained for rollback.
    pub last_good_version: String,
    /// User-selected release channel.
    pub channel: String,
    /// Current Rust target operating-system component.
    pub platform: String,
    /// Current Rust target architecture component.
    pub architecture: String,
    /// Current Unix second from the trusted local clock source.
    pub now_unix_seconds: u64,
    /// Current catalog schema version.
    pub catalog_schema: u32,
    /// Current local protocol major.
    pub protocol_major: u32,
    /// Current local protocol minor.
    pub protocol_minor: u32,
    /// Free bytes available on the owned installation volume.
    pub available_disk_bytes: u64,
    /// Deterministic installation rollout bucket from zero through ninety-nine.
    pub rollout_bucket: u8,
}

/// Ordered activation step retained in a verified update plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStep {
    /// Write the verified artifact beside the active installation.
    StageSideBySide,
    /// Retain the active binary and compatible state as last known good.
    PreserveLastGood,
    /// Atomically select the staged installation.
    Activate,
    /// Probe the new daemon within the signed deadline.
    HealthCheck,
    /// Commit success or restore the last known-good installation.
    CommitOrRollback,
}

/// Deterministic activation and rollback plan for a verified update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlan {
    /// Candidate semantic version.
    pub version: String,
    /// Version retained for rollback.
    pub last_good_version: String,
    /// Exact artifact basename.
    pub artifact_file_name: String,
    /// Conservative staging and migration disk requirement.
    pub required_disk_bytes: u64,
    /// Signed health-check deadline.
    pub health_timeout_seconds: u32,
    /// Ordered installer operations.
    pub steps: Vec<UpdateStep>,
}

/// Successful verification of signed metadata and exact artifact bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedUpdate {
    /// Trusted key selector from signed metadata.
    pub key_id: String,
    /// Candidate semantic version.
    pub version: String,
    /// Lowercase SHA-256 of the exact signed metadata bytes.
    pub metadata_sha256: String,
    /// Lowercase SHA-256 of the verified artifact bytes.
    pub artifact_sha256: String,
    /// Supporting SBOM digest.
    pub sbom_sha256: String,
    /// Supporting provenance digest.
    pub provenance_sha256: String,
    /// Supporting license-bundle digest.
    pub license_bundle_sha256: String,
    /// Observed reproducibility level.
    pub reproducibility: ReproducibilityLevel,
    /// Activation plan accepted by all preflight checks.
    pub plan: UpdatePlan,
}

/// Verification or policy rejection for update inputs.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// Update checks were not explicitly enabled by the user.
    #[error("updates are disabled")]
    Disabled,
    /// The public key is not canonical.
    #[error("update public key is invalid")]
    InvalidPublicKey,
    /// The detached signature encoding is not canonical.
    #[error("update signature encoding is invalid")]
    InvalidSignatureEncoding,
    /// The detached artifact-signature encoding is not canonical.
    #[error("update artifact signature encoding is invalid")]
    InvalidArtifactSignatureEncoding,
    /// The metadata exceeds its hard byte limit.
    #[error("update metadata exceeds its byte limit")]
    MetadataTooLarge,
    /// The metadata signature does not verify.
    #[error("update metadata signature is invalid")]
    InvalidSignature,
    /// The artifact signature does not verify against the trusted update key.
    #[error("update artifact signature is invalid")]
    InvalidArtifactSignature,
    /// The signed JSON cannot be decoded under the strict schema.
    #[error("update metadata is malformed")]
    MalformedMetadata,
    /// The metadata schema version is unsupported.
    #[error("update metadata schema is unsupported")]
    UnsupportedSchema,
    /// A metadata identity, range, digest, or bound is invalid.
    #[error("update metadata contract is invalid")]
    InvalidMetadata,
    /// The signed metadata is not valid yet.
    #[error("update metadata is not valid yet")]
    NotYetValid,
    /// The signed metadata expired.
    #[error("update metadata expired")]
    Expired,
    /// The selected channel does not match signed metadata.
    #[error("update channel does not match")]
    ChannelMismatch,
    /// The current platform does not match signed metadata.
    #[error("update platform does not match")]
    PlatformMismatch,
    /// The current architecture does not match signed metadata.
    #[error("update architecture does not match")]
    ArchitectureMismatch,
    /// The candidate version does not advance the current version.
    #[error("update version is not newer")]
    VersionNotNewer,
    /// The retained rollback version is not a valid semantic version.
    #[error("last known-good version is invalid")]
    InvalidRollbackVersion,
    /// The current catalog schema is outside the signed compatibility window.
    #[error("catalog schema is incompatible with update")]
    CatalogIncompatible,
    /// The current local protocol is outside the signed compatibility window.
    #[error("local protocol is incompatible with update")]
    ProtocolIncompatible,
    /// The candidate release cannot retain the last known-good state.
    #[error("update rollback is unavailable")]
    RollbackUnavailable,
    /// The installation is outside the signed rollout percentage.
    #[error("update rollout is deferred")]
    RolloutDeferred,
    /// The owned installation volume lacks required staging space.
    #[error("update requires more disk space")]
    InsufficientDisk,
    /// The signed artifact size exceeds the hard product limit.
    #[error("update artifact exceeds its byte limit")]
    ArtifactTooLarge,
    /// Artifact bytes could not be read.
    #[error("update artifact read failed")]
    ArtifactRead(#[source] io::Error),
    /// Artifact byte count differs from signed metadata.
    #[error("update artifact size does not match")]
    ArtifactSizeMismatch,
    /// Artifact digest differs from signed metadata.
    #[error("update artifact digest does not match")]
    ArtifactDigestMismatch,
    /// The artifact-specific SBOM exceeds its hard byte limit.
    #[error("update SBOM exceeds its byte limit")]
    SbomTooLarge,
    /// The signed provenance bundle exceeds its hard byte limit.
    #[error("update provenance exceeds its byte limit")]
    ProvenanceTooLarge,
    /// The license and notice bundle exceeds its hard byte limit.
    #[error("update license bundle exceeds its byte limit")]
    LicenseBundleTooLarge,
    /// The artifact-specific SBOM could not be read.
    #[error("update SBOM read failed")]
    SbomRead(#[source] io::Error),
    /// The signed provenance bundle could not be read.
    #[error("update provenance read failed")]
    ProvenanceRead(#[source] io::Error),
    /// The license and notice bundle could not be read.
    #[error("update license bundle read failed")]
    LicenseBundleRead(#[source] io::Error),
    /// The artifact-specific SBOM digest differs from signed metadata.
    #[error("update SBOM digest does not match")]
    SbomDigestMismatch,
    /// The signed provenance digest differs from signed metadata.
    #[error("update provenance digest does not match")]
    ProvenanceDigestMismatch,
    /// The license and notice digest differs from signed metadata.
    #[error("update license bundle digest does not match")]
    LicenseBundleDigestMismatch,
    /// The artifact-specific CycloneDX document violates update policy.
    #[error("update SBOM policy is not satisfied")]
    InvalidSbom,
    /// The signed SLSA provenance bundle violates update policy.
    #[error("update provenance policy is not satisfied")]
    InvalidProvenance,
    /// The license and notice bundle violates update policy.
    #[error("update license bundle is invalid")]
    InvalidLicenseBundle,
    /// Canonical metadata JSON could not be serialized.
    #[error("update metadata serialization failed")]
    MetadataSerialization(#[source] serde_json::Error),
}

/// Serializes validated signable metadata to the exact canonical JSON bytes.
///
/// Canonical bytes are compact UTF-8 JSON in [`UpdateMetadata`] field order
/// with no trailing newline. Release signing and runtime verification must use
/// these exact returned bytes without parsing and reserializing them.
///
/// # Errors
///
/// Returns [`UpdateError::InvalidMetadata`] for an invalid signable contract,
/// [`UpdateError::UnsupportedSchema`] for an unsupported schema, or
/// [`UpdateError::MetadataSerialization`] if serialization fails.
pub fn canonical_update_metadata_bytes(metadata: &UpdateMetadata) -> Result<Vec<u8>, UpdateError> {
    validate_metadata_contract(metadata)?;
    let bytes = serde_json::to_vec(metadata).map_err(UpdateError::MetadataSerialization)?;
    if bytes.is_empty() || bytes.len() > MAX_UPDATE_METADATA_BYTES {
        return Err(UpdateError::MetadataTooLarge);
    }
    Ok(bytes)
}

/// Builds the canonical domain-separated message signed for exact artifact bytes.
///
/// The message contains the raw SHA-256 digest and unsigned 64-bit artifact
/// length. Metadata and artifact signatures therefore remain distinct even
/// when the same protected Ed25519 identity authorizes both.
///
/// # Errors
///
/// Returns [`UpdateError`] when the metadata contract or artifact digest is
/// invalid.
pub fn canonical_artifact_signature_message(
    metadata: &UpdateMetadata,
) -> Result<Vec<u8>, UpdateError> {
    validate_metadata_contract(metadata)?;
    let digest = decode_sha256(&metadata.artifact.sha256)?;
    let mut message = Vec::with_capacity(ARTIFACT_SIGNATURE_DOMAIN.len() + 32 + 8);
    message.extend_from_slice(ARTIFACT_SIGNATURE_DOMAIN);
    message.extend_from_slice(&digest);
    message.extend_from_slice(&metadata.artifact.size_bytes.to_be_bytes());
    Ok(message)
}

/// Verifies the complete offline update evidence chain before installation.
///
/// This stricter production entry point verifies signed metadata, exact
/// artifact bytes, a distinct artifact signature, the artifact-specific
/// CycloneDX SBOM, SLSA provenance, and the license/notice bundle. Supporting
/// artifacts are streamed under fixed caps and their digests are bound by the
/// signed metadata.
///
/// # Errors
///
/// Returns [`UpdateError`] for any metadata, signature, artifact, supporting
/// digest, SBOM, provenance, license, compatibility, or resource-policy
/// failure.
pub fn verify_update_with_evidence(
    metadata_bytes: &[u8],
    signatures: UpdateSignatures,
    artifact: &mut impl Read,
    supporting: &mut UpdateSupportingEvidence<'_>,
    context: &UpdateContext,
) -> Result<VerifiedUpdate, UpdateError> {
    let verified = verify_update(
        metadata_bytes,
        signatures.metadata,
        signatures.public_key,
        artifact,
        context,
    )?;
    let metadata: UpdateMetadata =
        serde_json::from_slice(metadata_bytes).map_err(|_| UpdateError::MalformedMetadata)?;
    let public_key = PublicKey::from_slice(signatures.public_key.as_bytes())
        .map_err(|_| UpdateError::InvalidPublicKey)?;
    let signature = Signature::from_slice(signatures.artifact.as_bytes())
        .map_err(|_| UpdateError::InvalidArtifactSignatureEncoding)?;
    let artifact_message = canonical_artifact_signature_message(&metadata)?;
    public_key
        .verify(&artifact_message, &signature)
        .map_err(|_| UpdateError::InvalidArtifactSignature)?;

    let sbom_bytes = read_bounded_supporting_artifact(
        supporting.sbom,
        MAX_UPDATE_SBOM_BYTES,
        UpdateError::SbomRead,
        UpdateError::SbomTooLarge,
    )?;
    require_supporting_digest(
        &sbom_bytes,
        &metadata.artifact.sbom_sha256,
        UpdateError::SbomDigestMismatch,
    )?;
    let sbom_identity = validate_release_sbom(&sbom_bytes, &metadata)?;

    let provenance_bytes = read_bounded_supporting_artifact(
        supporting.provenance,
        MAX_UPDATE_PROVENANCE_BYTES,
        UpdateError::ProvenanceRead,
        UpdateError::ProvenanceTooLarge,
    )?;
    require_supporting_digest(
        &provenance_bytes,
        &metadata.artifact.provenance_sha256,
        UpdateError::ProvenanceDigestMismatch,
    )?;
    validate_release_provenance(&provenance_bytes, &metadata, &sbom_identity.source_revision)?;

    let license_bytes = read_bounded_supporting_artifact(
        supporting.license_bundle,
        MAX_UPDATE_LICENSE_BUNDLE_BYTES,
        UpdateError::LicenseBundleRead,
        UpdateError::LicenseBundleTooLarge,
    )?;
    require_supporting_digest(
        &license_bytes,
        &metadata.artifact.license_bundle_sha256,
        UpdateError::LicenseBundleDigestMismatch,
    )?;
    validate_license_bundle(&license_bytes)?;

    Ok(verified)
}

/// Verifies signed metadata and a bounded artifact stream without network access.
///
/// The signature covers `metadata_bytes` exactly. Verification rejects disabled
/// updates before parsing metadata or reading artifact bytes.
///
/// # Errors
///
/// Returns [`UpdateError`] for signature, schema, version, expiry, rollout,
/// platform, compatibility, resource, size, read, or digest failures.
pub fn verify_update(
    metadata_bytes: &[u8],
    signature: DetachedUpdateSignature,
    public_key: UpdatePublicKey,
    artifact: &mut impl Read,
    context: &UpdateContext,
) -> Result<VerifiedUpdate, UpdateError> {
    if !context.updates_enabled {
        return Err(UpdateError::Disabled);
    }
    if metadata_bytes.is_empty() || metadata_bytes.len() > MAX_UPDATE_METADATA_BYTES {
        return Err(UpdateError::MetadataTooLarge);
    }
    let public_key =
        PublicKey::from_slice(public_key.as_bytes()).map_err(|_| UpdateError::InvalidPublicKey)?;
    let signature = Signature::from_slice(signature.as_bytes())
        .map_err(|_| UpdateError::InvalidSignatureEncoding)?;
    public_key
        .verify(metadata_bytes, &signature)
        .map_err(|_| UpdateError::InvalidSignature)?;

    let metadata: UpdateMetadata =
        serde_json::from_slice(metadata_bytes).map_err(|_| UpdateError::MalformedMetadata)?;
    let required_disk_bytes = validate_metadata(&metadata, context)?;
    let declared_artifact_digest = decode_sha256(&metadata.artifact.sha256)?;
    let observed_artifact_digest = hash_exact_artifact(
        artifact,
        metadata.artifact.size_bytes,
        declared_artifact_digest,
    )?;
    let metadata_sha256 = encode_sha256(Sha256::digest(metadata_bytes).into());
    let artifact_sha256 = encode_sha256(observed_artifact_digest);

    Ok(VerifiedUpdate {
        key_id: metadata.key_id,
        version: metadata.version.clone(),
        metadata_sha256,
        artifact_sha256,
        sbom_sha256: metadata.artifact.sbom_sha256,
        provenance_sha256: metadata.artifact.provenance_sha256,
        license_bundle_sha256: metadata.artifact.license_bundle_sha256,
        reproducibility: metadata.artifact.reproducibility,
        plan: UpdatePlan {
            version: metadata.version,
            last_good_version: context.last_good_version.clone(),
            artifact_file_name: metadata.artifact.file_name,
            required_disk_bytes,
            health_timeout_seconds: metadata.compatibility.health_timeout_seconds,
            steps: vec![
                UpdateStep::StageSideBySide,
                UpdateStep::PreserveLastGood,
                UpdateStep::Activate,
                UpdateStep::HealthCheck,
                UpdateStep::CommitOrRollback,
            ],
        },
    })
}

#[derive(Debug)]
struct ReleaseSbomIdentity {
    source_revision: String,
}

fn read_bounded_supporting_artifact(
    reader: &mut (impl Read + ?Sized),
    limit: u64,
    read_error: fn(io::Error) -> UpdateError,
    too_large: UpdateError,
) -> Result<Vec<u8>, UpdateError> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(too_large);
    }
    Ok(bytes)
}

fn require_supporting_digest(
    bytes: &[u8],
    expected: &str,
    mismatch: UpdateError,
) -> Result<(), UpdateError> {
    let expected = decode_sha256(expected)?;
    let observed: [u8; 32] = Sha256::digest(bytes).into();
    if observed != expected {
        return Err(mismatch);
    }
    Ok(())
}

fn validate_release_sbom(
    bytes: &[u8],
    update: &UpdateMetadata,
) -> Result<ReleaseSbomIdentity, UpdateError> {
    let document: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| UpdateError::InvalidSbom)?;
    if document["bomFormat"] != "CycloneDX"
        || document["specVersion"] != "1.5"
        || document["version"] != 1
    {
        return Err(UpdateError::InvalidSbom);
    }
    let metadata = document["metadata"]
        .as_object()
        .ok_or(UpdateError::InvalidSbom)?;
    let target = update_target_triple(update)?;
    if property_value(metadata.get("properties"), CYCLONEDX_TARGET_PROPERTY) != Some(target)
        || property_value(metadata.get("properties"), "rootlight:build:profile") != Some("release")
    {
        return Err(UpdateError::InvalidSbom);
    }
    let source_revision = property_value(
        metadata.get("properties"),
        ROOTLIGHT_SOURCE_REVISION_PROPERTY,
    )
    .ok_or(UpdateError::InvalidSbom)?;
    if !is_canonical_source_revision(source_revision) {
        return Err(UpdateError::InvalidSbom);
    }
    let component = metadata
        .get("component")
        .and_then(serde_json::Value::as_object)
        .ok_or(UpdateError::InvalidSbom)?;
    if component.get("name").and_then(serde_json::Value::as_str) != Some("rootlight-distribution")
        || component.get("version").and_then(serde_json::Value::as_str)
            != Some(update.version.as_str())
        || property_value(component.get("properties"), ROOTLIGHT_TARGET_PROPERTY) != Some(target)
        || property_value(
            component.get("properties"),
            ROOTLIGHT_SOURCE_REVISION_PROPERTY,
        ) != Some(source_revision)
    {
        return Err(UpdateError::InvalidSbom);
    }
    let child_names = component
        .get("components")
        .and_then(serde_json::Value::as_array)
        .ok_or(UpdateError::InvalidSbom)?
        .iter()
        .filter_map(|child| child.get("name").and_then(serde_json::Value::as_str))
        .collect::<BTreeSet<_>>();
    for required in [
        "rootlight",
        "rootlight-adapter-host",
        "rootlight-daemon",
        "rootlight-launcher",
        "rootlight-mcp",
        "LICENSE",
        "NOTICE",
    ] {
        if !child_names.contains(required) {
            return Err(UpdateError::InvalidSbom);
        }
    }
    Ok(ReleaseSbomIdentity {
        source_revision: source_revision.to_owned(),
    })
}

fn property_value<'a>(
    properties: Option<&'a serde_json::Value>,
    expected_name: &str,
) -> Option<&'a str> {
    properties?
        .as_array()?
        .iter()
        .find(|property| property["name"] == expected_name)?
        .get("value")?
        .as_str()
}

fn update_target_triple(update: &UpdateMetadata) -> Result<&'static str, UpdateError> {
    match (update.platform.as_str(), update.architecture.as_str()) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        _ => Err(UpdateError::InvalidMetadata),
    }
}

fn is_canonical_source_revision(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_release_provenance(
    bytes: &[u8],
    update: &UpdateMetadata,
    source_revision: &str,
) -> Result<(), UpdateError> {
    let bundle: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| UpdateError::InvalidProvenance)?;
    let envelope = bundle
        .get("dsseEnvelope")
        .and_then(serde_json::Value::as_object)
        .ok_or(UpdateError::InvalidProvenance)?;
    let payload = envelope
        .get("payload")
        .and_then(serde_json::Value::as_str)
        .ok_or(UpdateError::InvalidProvenance)?;
    let payload = BASE64
        .decode(payload.as_bytes())
        .map_err(|_| UpdateError::InvalidProvenance)?;
    if payload.is_empty()
        || u64::try_from(payload.len()).unwrap_or(u64::MAX) > MAX_UPDATE_PROVENANCE_BYTES
    {
        return Err(UpdateError::InvalidProvenance);
    }
    let statement: serde_json::Value =
        serde_json::from_slice(&payload).map_err(|_| UpdateError::InvalidProvenance)?;
    if statement["_type"] != IN_TOTO_STATEMENT_V1
        || statement["predicateType"] != SLSA_PROVENANCE_V1
        || !provenance_subject_matches(&statement, update)
        || !json_contains_exact_string(&statement["predicate"], source_revision)
        || !contains_nonempty_array(&bundle, "tlogEntries")
    {
        return Err(UpdateError::InvalidProvenance);
    }
    Ok(())
}

fn provenance_subject_matches(statement: &serde_json::Value, update: &UpdateMetadata) -> bool {
    statement["subject"].as_array().is_some_and(|subjects| {
        subjects.iter().any(|subject| {
            subject["name"] == update.artifact.file_name
                && subject["digest"]["sha256"] == update.artifact.sha256
        })
    })
}

fn json_contains_exact_string(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| json_contains_exact_string(value, expected)),
        serde_json::Value::Object(values) => values
            .values()
            .any(|value| json_contains_exact_string(value, expected)),
        serde_json::Value::String(value) => value == expected,
        _ => false,
    }
}

fn contains_nonempty_array(value: &serde_json::Value, key: &str) -> bool {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .any(|value| contains_nonempty_array(value, key)),
        serde_json::Value::Object(values) => {
            values
                .get(key)
                .is_some_and(|value| value.as_array().is_some_and(|entries| !entries.is_empty()))
                || values
                    .values()
                    .any(|value| contains_nonempty_array(value, key))
        }
        _ => false,
    }
}

fn validate_license_bundle(bytes: &[u8]) -> Result<(), UpdateError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| UpdateError::InvalidLicenseBundle)?;
    if archive.is_empty() || archive.len() > 512 {
        return Err(UpdateError::InvalidLicenseBundle);
    }
    let mut names = BTreeSet::new();
    let mut uncompressed_bytes = 0_u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|_| UpdateError::InvalidLicenseBundle)?;
        let name = entry.name();
        if name.is_empty()
            || name.contains('\\')
            || name.starts_with('/')
            || name
                .split('/')
                .any(|component| component.is_empty() || matches!(component, "." | ".."))
            || !names.insert(name.to_owned())
        {
            return Err(UpdateError::InvalidLicenseBundle);
        }
        if entry.is_dir()
            || entry
                .unix_mode()
                .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            return Err(UpdateError::InvalidLicenseBundle);
        }
        uncompressed_bytes = uncompressed_bytes
            .checked_add(entry.size())
            .ok_or(UpdateError::InvalidLicenseBundle)?;
        if uncompressed_bytes > MAX_UPDATE_LICENSE_BUNDLE_BYTES {
            return Err(UpdateError::InvalidLicenseBundle);
        }
    }
    if !names.contains("LICENSE")
        || !names.contains("NOTICE")
        || names
            .iter()
            .filter(|name| name.starts_with("licenses/"))
            .count()
            < 4
    {
        return Err(UpdateError::InvalidLicenseBundle);
    }
    Ok(())
}

fn validate_metadata(
    metadata: &UpdateMetadata,
    context: &UpdateContext,
) -> Result<u64, UpdateError> {
    validate_metadata_contract(metadata)?;
    if context.rollout_bucket >= 100 {
        return Err(UpdateError::InvalidMetadata);
    }
    if context.now_unix_seconds < metadata.valid_from_unix_seconds {
        return Err(UpdateError::NotYetValid);
    }
    if context.now_unix_seconds >= metadata.expires_unix_seconds {
        return Err(UpdateError::Expired);
    }
    if metadata.channel != context.channel {
        return Err(UpdateError::ChannelMismatch);
    }
    if metadata.platform != context.platform {
        return Err(UpdateError::PlatformMismatch);
    }
    if metadata.architecture != context.architecture {
        return Err(UpdateError::ArchitectureMismatch);
    }

    let candidate = Version::parse(&metadata.version).map_err(|_| UpdateError::InvalidMetadata)?;
    let current =
        Version::parse(&context.current_version).map_err(|_| UpdateError::InvalidMetadata)?;
    Version::parse(&context.last_good_version).map_err(|_| UpdateError::InvalidRollbackVersion)?;
    if candidate <= current {
        return Err(UpdateError::VersionNotNewer);
    }
    if !(metadata.compatibility.minimum_catalog_schema
        ..=metadata.compatibility.maximum_catalog_schema)
        .contains(&context.catalog_schema)
    {
        return Err(UpdateError::CatalogIncompatible);
    }
    if context.protocol_major != metadata.compatibility.protocol_major
        || !(metadata.compatibility.minimum_protocol_minor
            ..=metadata.compatibility.maximum_protocol_minor)
            .contains(&context.protocol_minor)
    {
        return Err(UpdateError::ProtocolIncompatible);
    }
    if context.rollout_bucket >= metadata.rollout_percentage {
        return Err(UpdateError::RolloutDeferred);
    }

    let required_disk_bytes = metadata
        .artifact
        .size_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(metadata.compatibility.migration_required_bytes))
        .and_then(|bytes| bytes.checked_add(UPDATE_STAGING_OVERHEAD_BYTES))
        .ok_or(UpdateError::InvalidMetadata)?;
    if required_disk_bytes > context.available_disk_bytes {
        return Err(UpdateError::InsufficientDisk);
    }
    Ok(required_disk_bytes)
}

fn validate_metadata_contract(metadata: &UpdateMetadata) -> Result<(), UpdateError> {
    if metadata.schema_version != UPDATE_METADATA_SCHEMA_VERSION {
        return Err(UpdateError::UnsupportedSchema);
    }
    if !valid_label(&metadata.key_id)
        || !valid_label(&metadata.channel)
        || !valid_label(&metadata.platform)
        || !valid_label(&metadata.architecture)
        || !valid_artifact_name(&metadata.artifact.file_name)
        || decode_sha256(&metadata.artifact.sha256).is_err()
        || decode_sha256(&metadata.artifact.sbom_sha256).is_err()
        || decode_sha256(&metadata.artifact.provenance_sha256).is_err()
        || decode_sha256(&metadata.artifact.license_bundle_sha256).is_err()
        || metadata.valid_from_unix_seconds >= metadata.expires_unix_seconds
        || metadata.rollout_percentage == 0
        || metadata.rollout_percentage > 100
        || metadata.compatibility.minimum_catalog_schema
            > metadata.compatibility.maximum_catalog_schema
        || metadata.compatibility.minimum_protocol_minor
            > metadata.compatibility.maximum_protocol_minor
        || !(MIN_HEALTH_TIMEOUT_SECONDS..=MAX_HEALTH_TIMEOUT_SECONDS)
            .contains(&metadata.compatibility.health_timeout_seconds)
    {
        return Err(UpdateError::InvalidMetadata);
    }
    if !metadata.compatibility.rollback_supported {
        return Err(UpdateError::RollbackUnavailable);
    }
    if metadata.artifact.size_bytes == 0 || metadata.artifact.size_bytes > MAX_UPDATE_ARTIFACT_BYTES
    {
        return Err(UpdateError::ArtifactTooLarge);
    }
    let candidate = Version::parse(&metadata.version).map_err(|_| UpdateError::InvalidMetadata)?;
    if candidate.to_string() != metadata.version {
        return Err(UpdateError::InvalidMetadata);
    }
    Ok(())
}

fn hash_exact_artifact(
    artifact: &mut impl Read,
    declared_bytes: u64,
    declared_digest: [u8; 32],
) -> Result<[u8; 32], UpdateError> {
    let mut hasher = Sha256::new();
    let mut remaining = declared_bytes;
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    while remaining > 0 {
        let wanted = usize::try_from(remaining.min(HASH_BUFFER_BYTES as u64))
            .map_err(|_| UpdateError::ArtifactTooLarge)?;
        let read = artifact
            .read(&mut buffer[..wanted])
            .map_err(UpdateError::ArtifactRead)?;
        if read == 0 {
            return Err(UpdateError::ArtifactSizeMismatch);
        }
        hasher.update(&buffer[..read]);
        remaining = remaining
            .checked_sub(u64::try_from(read).map_err(|_| UpdateError::ArtifactTooLarge)?)
            .ok_or(UpdateError::ArtifactSizeMismatch)?;
    }
    let mut trailing = [0_u8; 1];
    if artifact
        .read(&mut trailing)
        .map_err(UpdateError::ArtifactRead)?
        != 0
    {
        return Err(UpdateError::ArtifactSizeMismatch);
    }
    let observed: [u8; 32] = hasher.finalize().into();
    if observed != declared_digest {
        return Err(UpdateError::ArtifactDigestMismatch);
    }
    Ok(observed)
}

fn decode_sha256(value: &str) -> Result<[u8; 32], UpdateError> {
    decode_hex_array::<32>(value, SHA256_HEX_BYTES).ok_or(UpdateError::InvalidMetadata)
}

fn decode_hex_array<const N: usize>(value: &str, expected_length: usize) -> Option<[u8; N]> {
    if value.len() != expected_length || expected_length != N.checked_mul(2)? {
        return None;
    }
    let mut decoded = [0_u8; N];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        decoded[index] = high.checked_mul(16)?.checked_add(low)?;
    }
    Some(decoded)
}

const fn decode_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

fn encode_sha256(value: [u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(SHA256_HEX_BYTES);
    for byte in value {
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_UPDATE_LABEL_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn valid_artifact_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ARTIFACT_NAME_BYTES
        && value != "."
        && value != ".."
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Opaque platform-installer failure that cannot disclose command or path text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("platform update operation failed")]
pub struct UpdateInstallError;

/// Platform boundary used after cryptographic and compatibility verification.
pub trait UpdateInstaller {
    /// Stages the verified artifact without changing the active installation.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateInstallError`] without changing the active installation.
    fn stage(&mut self, update: &VerifiedUpdate) -> Result<(), UpdateInstallError>;

    /// Atomically selects the staged installation while retaining last known good.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateInstallError`] when activation does not complete.
    fn activate(&mut self) -> Result<(), UpdateInstallError>;

    /// Checks the activated installation within the signed health deadline.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateInstallError`] when the health probe cannot complete.
    fn health_check(&mut self) -> Result<bool, UpdateInstallError>;

    /// Commits the healthy installation and keeps the documented rollback state.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateInstallError`] when commit does not complete.
    fn commit(&mut self) -> Result<(), UpdateInstallError>;

    /// Restores the retained last known-good installation.
    ///
    /// # Errors
    ///
    /// Returns [`UpdateInstallError`] when rollback cannot be confirmed.
    fn rollback(&mut self) -> Result<(), UpdateInstallError>;
}

/// Successful application of a previously verified update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateApplyOutcome {
    /// Activated semantic version.
    pub version: String,
    /// Health passed before commit.
    pub health_passed: bool,
    /// Successful application did not require rollback.
    pub rolled_back: bool,
}

/// Closed failure outcome for activation and mandatory rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum UpdateApplyError {
    /// Staging failed before active state changed.
    #[error("update staging failed")]
    StageFailed,
    /// Activation failed and the last known-good installation was restored.
    #[error("update activation failed and rollback completed")]
    ActivationFailed,
    /// Health failed and the last known-good installation was restored.
    #[error("update health check failed and rollback completed")]
    HealthFailed,
    /// Health could not be checked and the last known-good installation was restored.
    #[error("update health check was unavailable and rollback completed")]
    HealthUnavailable,
    /// Commit failed and the last known-good installation was restored.
    #[error("update commit failed and rollback completed")]
    CommitFailed,
    /// A required rollback did not complete.
    #[error("update rollback failed")]
    RollbackFailed,
}

/// Applies a verified update and rolls back every post-staging failure.
///
/// # Errors
///
/// Returns [`UpdateApplyError`] when staging, activation, health, commit, or
/// rollback fails. Any failure after staging triggers rollback before return.
pub fn apply_verified_update(
    update: &VerifiedUpdate,
    installer: &mut impl UpdateInstaller,
) -> Result<UpdateApplyOutcome, UpdateApplyError> {
    installer
        .stage(update)
        .map_err(|_| UpdateApplyError::StageFailed)?;
    if installer.activate().is_err() {
        rollback(installer)?;
        return Err(UpdateApplyError::ActivationFailed);
    }
    match installer.health_check() {
        Ok(true) => {}
        Ok(false) => {
            rollback(installer)?;
            return Err(UpdateApplyError::HealthFailed);
        }
        Err(_) => {
            rollback(installer)?;
            return Err(UpdateApplyError::HealthUnavailable);
        }
    }
    if installer.commit().is_err() {
        rollback(installer)?;
        return Err(UpdateApplyError::CommitFailed);
    }
    Ok(UpdateApplyOutcome {
        version: update.version.clone(),
        health_passed: true,
        rolled_back: false,
    })
}

fn rollback(installer: &mut impl UpdateInstaller) -> Result<(), UpdateApplyError> {
    installer
        .rollback()
        .map_err(|_| UpdateApplyError::RollbackFailed)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ed25519_compact::{KeyPair, Seed};
    use serde_json::json;
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn context() -> UpdateContext {
        UpdateContext {
            updates_enabled: true,
            current_version: "1.2.3".to_owned(),
            last_good_version: "1.2.3".to_owned(),
            channel: "stable".to_owned(),
            platform: "windows".to_owned(),
            architecture: "x86_64".to_owned(),
            now_unix_seconds: 2_000,
            catalog_schema: 3,
            protocol_major: 1,
            protocol_minor: 7,
            available_disk_bytes: 64 * 1024 * 1024,
            rollout_bucket: 10,
        }
    }

    fn signed_fixture(
        artifact: &[u8],
    ) -> (
        Vec<u8>,
        DetachedUpdateSignature,
        UpdatePublicKey,
        UpdateContext,
    ) {
        let artifact_sha256 = encode_sha256(Sha256::digest(artifact).into());
        let metadata = json!({
            "schema_version": "1.0",
            "key_id": "rootlight-release-2026",
            "version": "1.3.0",
            "channel": "stable",
            "platform": "windows",
            "architecture": "x86_64",
            "valid_from_unix_seconds": 1000,
            "expires_unix_seconds": 3000,
            "rollout_percentage": 100,
            "artifact": {
                "file_name": "rootlight-x86_64-pc-windows-msvc.zip",
                "sha256": artifact_sha256,
                "size_bytes": artifact.len(),
                "sbom_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "provenance_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                "license_bundle_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "reproducibility": "provenance_only"
            },
            "compatibility": {
                "minimum_catalog_schema": 2,
                "maximum_catalog_schema": 4,
                "protocol_major": 1,
                "minimum_protocol_minor": 6,
                "maximum_protocol_minor": 8,
                "migration_required_bytes": 4096,
                "rollback_supported": true,
                "health_timeout_seconds": 30
            }
        });
        let metadata = serde_json::to_vec(&metadata).expect("metadata serializes");
        let key_pair = KeyPair::from_seed(Seed::new([7_u8; 32]));
        let signature = key_pair.sk.sign(&metadata, None);
        (
            metadata,
            DetachedUpdateSignature(*signature),
            UpdatePublicKey(*key_pair.pk),
            context(),
        )
    }

    struct ReleaseEvidenceFixture {
        artifact: Vec<u8>,
        artifact_signature: DetachedArtifactSignature,
        context: UpdateContext,
        license_bundle: Vec<u8>,
        metadata: Vec<u8>,
        metadata_signature: DetachedUpdateSignature,
        provenance: Vec<u8>,
        public_key: UpdatePublicKey,
        sbom: Vec<u8>,
    }

    fn release_evidence_fixture() -> ReleaseEvidenceFixture {
        let artifact = b"verified release artifact".to_vec();
        let artifact_sha256 = encode_sha256(Sha256::digest(&artifact).into());
        let source_revision = "0123456789abcdef0123456789abcdef01234567";
        let target = "x86_64-pc-windows-msvc";
        let sbom = serde_json::to_vec(&json!({
            "bomFormat": "CycloneDX",
            "components": [],
            "dependencies": [],
            "metadata": {
                "component": {
                    "bom-ref": "urn:rootlight:distribution:1.3.0:x86_64-pc-windows-msvc",
                    "components": [
                        {"name": "rootlight"},
                        {"name": "rootlight-adapter-host"},
                        {"name": "rootlight-daemon"},
                        {"name": "rootlight-launcher"},
                        {"name": "rootlight-mcp"},
                        {"name": "LICENSE"},
                        {"name": "NOTICE"}
                    ],
                    "name": "rootlight-distribution",
                    "properties": [
                        {"name": ROOTLIGHT_SOURCE_REVISION_PROPERTY, "value": source_revision},
                        {"name": ROOTLIGHT_TARGET_PROPERTY, "value": target}
                    ],
                    "type": "application",
                    "version": "1.3.0"
                },
                "properties": [
                    {"name": CYCLONEDX_TARGET_PROPERTY, "value": target},
                    {"name": "rootlight:build:profile", "value": "release"},
                    {"name": ROOTLIGHT_SOURCE_REVISION_PROPERTY, "value": source_revision}
                ]
            },
            "specVersion": "1.5",
            "version": 1
        }))
        .expect("SBOM fixture serializes");
        let statement = serde_json::to_vec(&json!({
            "_type": IN_TOTO_STATEMENT_V1,
            "subject": [{
                "name": "rootlight-x86_64-pc-windows-msvc.zip",
                "digest": {"sha256": artifact_sha256}
            }],
            "predicateType": SLSA_PROVENANCE_V1,
            "predicate": {
                "buildDefinition": {
                    "resolvedDependencies": [{
                        "uri": "git+https://github.com/tomasmarekk/rootlight",
                        "digest": {"gitCommit": source_revision}
                    }]
                }
            }
        }))
        .expect("provenance statement serializes");
        let provenance = serde_json::to_vec(&json!({
            "dsseEnvelope": {
                "payload": BASE64.encode(&statement),
                "payloadType": "application/vnd.in-toto+json",
                "signatures": [{"keyid": "", "sig": "fixture"}]
            },
            "verificationMaterial": {
                "tlogEntries": [{"logIndex": "1"}]
            }
        }))
        .expect("provenance bundle serializes");
        let license_bundle = license_bundle_fixture();
        let metadata = UpdateMetadata {
            schema_version: UPDATE_METADATA_SCHEMA_VERSION.to_owned(),
            key_id: "rootlight-release-2026".to_owned(),
            version: "1.3.0".to_owned(),
            channel: "stable".to_owned(),
            platform: "windows".to_owned(),
            architecture: "x86_64".to_owned(),
            valid_from_unix_seconds: 1_000,
            expires_unix_seconds: 3_000,
            rollout_percentage: 100,
            artifact: ArtifactMetadata {
                file_name: "rootlight-x86_64-pc-windows-msvc.zip".to_owned(),
                sha256: artifact_sha256,
                size_bytes: u64::try_from(artifact.len()).expect("artifact length fits"),
                sbom_sha256: encode_sha256(Sha256::digest(&sbom).into()),
                provenance_sha256: encode_sha256(Sha256::digest(&provenance).into()),
                license_bundle_sha256: encode_sha256(Sha256::digest(&license_bundle).into()),
                reproducibility: ReproducibilityLevel::BitForBit,
            },
            compatibility: UpdateCompatibility {
                minimum_catalog_schema: 2,
                maximum_catalog_schema: 4,
                protocol_major: 1,
                minimum_protocol_minor: 6,
                maximum_protocol_minor: 8,
                migration_required_bytes: 4_096,
                rollback_supported: true,
                health_timeout_seconds: 30,
            },
        };
        let metadata_bytes =
            canonical_update_metadata_bytes(&metadata).expect("metadata fixture is canonical");
        let key_pair = KeyPair::from_seed(Seed::new([7_u8; 32]));
        let metadata_signature = key_pair.sk.sign(&metadata_bytes, None);
        let artifact_message =
            canonical_artifact_signature_message(&metadata).expect("artifact message is canonical");
        let artifact_signature = key_pair.sk.sign(&artifact_message, None);
        ReleaseEvidenceFixture {
            artifact,
            artifact_signature: DetachedArtifactSignature(*artifact_signature),
            context: context(),
            license_bundle,
            metadata: metadata_bytes,
            metadata_signature: DetachedUpdateSignature(*metadata_signature),
            provenance,
            public_key: UpdatePublicKey(*key_pair.pk),
            sbom,
        }
    }

    fn license_bundle_fixture() -> Vec<u8> {
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default();
        for name in [
            "LICENSE",
            "NOTICE",
            "licenses/tree-sitter-cpp-LICENSE",
            "licenses/tree-sitter-java-LICENSE",
            "licenses/tree-sitter-kotlin-LICENSE",
            "licenses/tree-sitter-typescript-LICENSE",
        ] {
            writer
                .start_file(name, options)
                .expect("license entry starts");
            std::io::Write::write_all(&mut writer, b"fixture license\n")
                .expect("license entry writes");
        }
        writer
            .finish()
            .expect("license fixture finishes")
            .into_inner()
    }

    #[test]
    fn exact_signed_artifact_produces_a_side_by_side_plan() {
        let artifact = b"verified release artifact";
        let (metadata, signature, public_key, context) = signed_fixture(artifact);

        let verified = verify_update(
            &metadata,
            signature,
            public_key,
            &mut Cursor::new(artifact),
            &context,
        )
        .expect("signed update verifies");

        assert_eq!(verified.version, "1.3.0");
        assert_eq!(
            verified.plan.steps,
            [
                UpdateStep::StageSideBySide,
                UpdateStep::PreserveLastGood,
                UpdateStep::Activate,
                UpdateStep::HealthCheck,
                UpdateStep::CommitOrRollback,
            ]
        );
        assert!(verified.plan.required_disk_bytes > artifact.len() as u64);
    }

    #[test]
    fn complete_release_evidence_chain_verifies_before_install() {
        let fixture = release_evidence_fixture();
        let mut artifact = Cursor::new(&fixture.artifact);
        let mut sbom = Cursor::new(&fixture.sbom);
        let mut provenance = Cursor::new(&fixture.provenance);
        let mut licenses = Cursor::new(&fixture.license_bundle);
        let mut supporting =
            UpdateSupportingEvidence::new(&mut sbom, &mut provenance, &mut licenses);

        let verified = verify_update_with_evidence(
            &fixture.metadata,
            UpdateSignatures::new(
                fixture.metadata_signature,
                fixture.artifact_signature,
                fixture.public_key,
            ),
            &mut artifact,
            &mut supporting,
            &fixture.context,
        )
        .expect("complete release evidence verifies");

        assert_eq!(verified.version, "1.3.0");
        assert_eq!(
            verified.artifact_sha256,
            encode_sha256(Sha256::digest(&fixture.artifact).into())
        );
    }

    #[test]
    fn artifact_signature_and_supporting_digests_are_mandatory() {
        let fixture = release_evidence_fixture();
        let mut artifact = Cursor::new(&fixture.artifact);
        let mut sbom = Cursor::new(&fixture.sbom);
        let mut provenance = Cursor::new(&fixture.provenance);
        let mut licenses = Cursor::new(&fixture.license_bundle);
        let mut supporting =
            UpdateSupportingEvidence::new(&mut sbom, &mut provenance, &mut licenses);
        assert!(matches!(
            verify_update_with_evidence(
                &fixture.metadata,
                UpdateSignatures::new(
                    fixture.metadata_signature,
                    DetachedArtifactSignature([0_u8; 64]),
                    fixture.public_key,
                ),
                &mut artifact,
                &mut supporting,
                &fixture.context,
            ),
            Err(UpdateError::InvalidArtifactSignature)
        ));

        let fixture = release_evidence_fixture();
        let mut tampered_sbom = fixture.sbom.clone();
        tampered_sbom[0] ^= 1;
        let mut artifact = Cursor::new(&fixture.artifact);
        let mut sbom = Cursor::new(tampered_sbom);
        let mut provenance = Cursor::new(&fixture.provenance);
        let mut licenses = Cursor::new(&fixture.license_bundle);
        let mut supporting =
            UpdateSupportingEvidence::new(&mut sbom, &mut provenance, &mut licenses);
        assert!(matches!(
            verify_update_with_evidence(
                &fixture.metadata,
                UpdateSignatures::new(
                    fixture.metadata_signature,
                    fixture.artifact_signature,
                    fixture.public_key,
                ),
                &mut artifact,
                &mut supporting,
                &fixture.context,
            ),
            Err(UpdateError::SbomDigestMismatch)
        ));
    }

    #[test]
    fn malformed_release_policy_documents_are_rejected() {
        let fixture = release_evidence_fixture();
        let metadata: UpdateMetadata =
            serde_json::from_slice(&fixture.metadata).expect("metadata fixture decodes");
        assert!(matches!(
            validate_release_sbom(b"{}", &metadata),
            Err(UpdateError::InvalidSbom)
        ));
        assert!(matches!(
            validate_release_provenance(b"{}", &metadata, "0".repeat(40).as_str()),
            Err(UpdateError::InvalidProvenance)
        ));
        assert!(matches!(
            validate_license_bundle(b"not-a-zip"),
            Err(UpdateError::InvalidLicenseBundle)
        ));
    }

    #[test]
    fn disabled_update_rejects_before_reading_artifact() {
        struct FailingReader;

        impl Read for FailingReader {
            fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
                panic!("disabled updates must not read artifact bytes");
            }
        }

        let artifact = b"verified release artifact";
        let (metadata, signature, public_key, mut context) = signed_fixture(artifact);
        context.updates_enabled = false;

        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut FailingReader,
                &context
            ),
            Err(UpdateError::Disabled)
        ));
    }

    #[test]
    fn tampered_metadata_and_artifacts_are_rejected() {
        let artifact = b"verified release artifact";
        let (mut metadata, signature, public_key, context) = signed_fixture(artifact);
        metadata.push(b' ');
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(artifact),
                &context
            ),
            Err(UpdateError::InvalidSignature)
        ));

        let (metadata, signature, public_key, context) = signed_fixture(artifact);
        let mut tampered_artifact = artifact.to_vec();
        tampered_artifact[0] ^= 1;
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(tampered_artifact),
                &context
            ),
            Err(UpdateError::ArtifactDigestMismatch)
        ));
    }

    #[test]
    fn downgrade_expiry_and_incompatible_migration_fail_before_artifact_read() {
        let artifact = b"verified release artifact";
        let (metadata, signature, public_key, mut context) = signed_fixture(artifact);
        context.current_version = "1.3.0".to_owned();
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(artifact),
                &context
            ),
            Err(UpdateError::VersionNotNewer)
        ));

        let (metadata, signature, public_key, mut context) = signed_fixture(artifact);
        context.now_unix_seconds = 3_000;
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(artifact),
                &context
            ),
            Err(UpdateError::Expired)
        ));

        let (metadata, signature, public_key, mut context) = signed_fixture(artifact);
        context.available_disk_bytes = 1;
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(artifact),
                &context
            ),
            Err(UpdateError::InsufficientDisk)
        ));

        let (metadata, signature, public_key, mut context) = signed_fixture(artifact);
        context.catalog_schema = 5;
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(artifact),
                &context
            ),
            Err(UpdateError::CatalogIncompatible)
        ));

        let (metadata, signature, public_key, mut context) = signed_fixture(artifact);
        context.platform = "linux".to_owned();
        assert!(matches!(
            verify_update(
                &metadata,
                signature,
                public_key,
                &mut Cursor::new(artifact),
                &context
            ),
            Err(UpdateError::PlatformMismatch)
        ));
    }

    struct Installer {
        calls: Vec<&'static str>,
        health: Result<bool, UpdateInstallError>,
        rollback_fails: bool,
    }

    impl Default for Installer {
        fn default() -> Self {
            Self {
                calls: Vec::new(),
                health: Ok(true),
                rollback_fails: false,
            }
        }
    }

    impl UpdateInstaller for Installer {
        fn stage(&mut self, _update: &VerifiedUpdate) -> Result<(), UpdateInstallError> {
            self.calls.push("stage");
            Ok(())
        }

        fn activate(&mut self) -> Result<(), UpdateInstallError> {
            self.calls.push("activate");
            Ok(())
        }

        fn health_check(&mut self) -> Result<bool, UpdateInstallError> {
            self.calls.push("health");
            self.health
        }

        fn commit(&mut self) -> Result<(), UpdateInstallError> {
            self.calls.push("commit");
            Ok(())
        }

        fn rollback(&mut self) -> Result<(), UpdateInstallError> {
            self.calls.push("rollback");
            if self.rollback_fails {
                Err(UpdateInstallError)
            } else {
                Ok(())
            }
        }
    }

    fn verified_fixture() -> VerifiedUpdate {
        let artifact = b"verified release artifact";
        let (metadata, signature, public_key, context) = signed_fixture(artifact);
        verify_update(
            &metadata,
            signature,
            public_key,
            &mut Cursor::new(artifact),
            &context,
        )
        .expect("signed update verifies")
    }

    #[test]
    fn failed_health_always_rolls_back_before_return() {
        let update = verified_fixture();
        let mut installer = Installer {
            health: Ok(false),
            ..Installer::default()
        };

        assert_eq!(
            apply_verified_update(&update, &mut installer),
            Err(UpdateApplyError::HealthFailed)
        );
        assert_eq!(installer.calls, ["stage", "activate", "health", "rollback"]);
    }

    #[test]
    fn rollback_failure_is_never_hidden() {
        let update = verified_fixture();
        let mut installer = Installer {
            health: Ok(false),
            rollback_fails: true,
            ..Installer::default()
        };

        assert_eq!(
            apply_verified_update(&update, &mut installer),
            Err(UpdateApplyError::RollbackFailed)
        );
        assert_eq!(installer.calls.last(), Some(&"rollback"));
    }

    #[test]
    fn healthy_update_commits_without_rollback() {
        let update = verified_fixture();
        let mut installer = Installer {
            health: Ok(true),
            ..Installer::default()
        };

        let outcome =
            apply_verified_update(&update, &mut installer).expect("healthy update commits");

        assert_eq!(outcome.version, "1.3.0");
        assert_eq!(installer.calls, ["stage", "activate", "health", "commit"]);
    }
}
