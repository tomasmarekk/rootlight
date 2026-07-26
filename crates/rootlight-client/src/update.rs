//! Verification and rollback orchestration for opt-in offline update inputs.
//!
//! Metadata signatures cover the exact bounded JSON bytes. Artifact hashing is
//! streamed, and activation remains behind a platform installer boundary.

use std::io::{self, Read};

use ed25519_compact::{PublicKey, Signature};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

/// Schema version for signed update metadata.
pub const UPDATE_METADATA_SCHEMA_VERSION: &str = "1.0";
/// Maximum accepted signed metadata size.
pub const MAX_UPDATE_METADATA_BYTES: usize = 64 * 1024;
/// Maximum accepted update artifact size.
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

const SHA256_HEX_BYTES: usize = 64;
const PUBLIC_KEY_HEX_BYTES: usize = 64;
const SIGNATURE_HEX_BYTES: usize = 128;
const MAX_UPDATE_LABEL_BYTES: usize = 128;
const MAX_ARTIFACT_NAME_BYTES: usize = 255;
const MAX_HEALTH_TIMEOUT_SECONDS: u32 = 300;
const MIN_HEALTH_TIMEOUT_SECONDS: u32 = 1;
const UPDATE_STAGING_OVERHEAD_BYTES: u64 = 16 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;

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
    /// The metadata exceeds its hard byte limit.
    #[error("update metadata exceeds its byte limit")]
    MetadataTooLarge,
    /// The metadata signature does not verify.
    #[error("update metadata signature is invalid")]
    InvalidSignature,
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

fn validate_metadata(
    metadata: &UpdateMetadata,
    context: &UpdateContext,
) -> Result<u64, UpdateError> {
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
        || context.rollout_bucket >= 100
        || metadata.compatibility.minimum_catalog_schema
            > metadata.compatibility.maximum_catalog_schema
        || metadata.compatibility.minimum_protocol_minor
            > metadata.compatibility.maximum_protocol_minor
        || !(MIN_HEALTH_TIMEOUT_SECONDS..=MAX_HEALTH_TIMEOUT_SECONDS)
            .contains(&metadata.compatibility.health_timeout_seconds)
    {
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
    if !metadata.compatibility.rollback_supported {
        return Err(UpdateError::RollbackUnavailable);
    }
    if context.rollout_bucket >= metadata.rollout_percentage {
        return Err(UpdateError::RolloutDeferred);
    }
    if metadata.artifact.size_bytes == 0 || metadata.artifact.size_bytes > MAX_UPDATE_ARTIFACT_BYTES
    {
        return Err(UpdateError::ArtifactTooLarge);
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
