//! Production side-by-side installation, recovery, and status for signed updates.
//!
//! Archive bytes are copied and rehashed through one no-follow source handle.
//! Private capability directories own extraction, publication, and durable state.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{self, Cursor, Read, Seek as _, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use std::fs::OpenOptions;

use cap_std::{ambient_authority, fs::Dir};
use rootlight_vfs::platform::{PlatformError, PrivateDirectory, PublishError};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use zip::{CompressionMethod, ZipArchive};

use super::{
    DetachedArtifactSignature, DetachedUpdateSignature, MAX_UPDATE_ARTIFACT_BYTES,
    MAX_UPDATE_LICENSE_BUNDLE_BYTES, MAX_UPDATE_METADATA_BYTES, MAX_UPDATE_PROVENANCE_BYTES,
    MAX_UPDATE_SBOM_BYTES, UpdateContext, UpdateError, UpdatePublicKey, UpdateSignatures,
    UpdateSupportingEvidence, VerifiedUpdate, verify_update_with_evidence,
};

/// Strict policy state read by production update application.
pub const UPDATE_POLICY_FILE: &str = "update-policy.json";
/// Durable update transaction state consulted by the stable launcher.
pub const UPDATE_TRANSACTION_FILE: &str = "update-transaction.json";
/// Persistent operating-system update lock.
pub const UPDATE_LOCK_FILE: &str = "update.lock";
/// Active payload selector read by the stable launcher.
pub const ACTIVE_VERSION_FILE: &str = "active-version";
/// Ownership and rollback state for installed package files.
pub const INSTALL_MANIFEST_FILE: &str = "install-manifest.json";
/// Internal candidate-probe input containing a private clone of current state.
pub const UPDATE_HEALTH_STATE_DIR_ENV: &str = "ROOTLIGHT_UPDATE_HEALTH_STATE_DIR";

const UPDATE_POLICY_SCHEMA: &str = "rootlight.update-policy/1";
const UPDATE_TRANSACTION_SCHEMA: &str = "rootlight.update-transaction/1";
const INSTALL_MANIFEST_SCHEMA_V1: &str = "rootlight.install-ownership/1";
const INSTALL_MANIFEST_SCHEMA_V2: &str = "rootlight.install-ownership/2";
const PACKAGE_MANIFEST_SCHEMA_V1: &str = "rootlight.package-manifest/1";
const PACKAGE_MANIFEST_SCHEMA_V2: &str = "rootlight.package-manifest/2";
const PACKAGE_MANIFEST_SCHEMA_V3: &str = "rootlight.package-manifest/3";
const PACKAGE_MANIFEST_NAME: &str = "package-manifest.json";
#[cfg(windows)]
const DEFERRED_UNINSTALL_SCHEMA: &str = "rootlight.deferred-uninstall/1";
const DEFERRED_UNINSTALL_FILE: &str = "deferred-uninstall.json";
#[cfg(windows)]
const DEFERRED_UNINSTALL_TOKEN_ENV: &str = "ROOTLIGHT_DEFERRED_UNINSTALL_TOKEN";
#[cfg(windows)]
const DEFERRED_UNINSTALL_LAUNCHER_PID_ENV: &str = "ROOTLIGHT_DEFERRED_UNINSTALL_LAUNCHER_PID";
const VERSIONS_DIRECTORY: &str = "versions";
const LAUNCHER_DIRECTORY: &str = "current/bin";
const ARTIFACT_COPY_NAME: &str = ".update-artifact.copy";
const BOOTSTRAP_ARTIFACT_COPY_NAME: &str = ".bootstrap-artifact.copy";
const MAX_POLICY_BYTES: u64 = 64 * 1024;
const MAX_STATE_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_PACKAGE_ENTRIES: usize = 2_048;
const MAX_OWNED_PATHS: usize = 4_096;
const MAX_PATH_BYTES: usize = 512;
const MAX_LABEL_BYTES: usize = 128;
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const RETAINED_VERSIONS: u8 = 2;
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_HEALTH_PROBE_STATE_ENTRIES: usize = 65_536;
const MAX_HEALTH_PROBE_STATE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_HEALTH_PROBE_STATE_DEPTH: usize = 128;
const LEGACY_EXPECTED_BINARIES: [&str; 5] = [
    "rootlight",
    "rootlight-adapter-host",
    "rootlight-daemon",
    "rootlight-mcp",
    "rootlight-semantic-host",
];
const EXPECTED_BINARIES: [&str; 6] = [
    "rootlight",
    "rootlight-adapter-host",
    "rootlight-daemon",
    "rootlight-mcp",
    "rootlight-semantic-host",
    "rootlight-web",
];
const WEB_ASSET_PREFIX: &str = "share/rootlight/web/";
const WEB_ASSET_MANIFEST: &str = "share/rootlight/web/asset-manifest.json";
const WEB_ASSET_ENTRYPOINT: &str = "share/rootlight/web/index.html";

/// Exact filesystem paths for one signed offline update input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInputPaths {
    metadata: PathBuf,
    metadata_signature: PathBuf,
    artifact_signature: PathBuf,
    artifact: PathBuf,
    sbom: PathBuf,
    provenance: PathBuf,
    license_bundle: PathBuf,
}

impl UpdateInputPaths {
    /// Creates update inputs whose files will be opened and validated by the runtime.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        metadata: PathBuf,
        metadata_signature: PathBuf,
        artifact_signature: PathBuf,
        artifact: PathBuf,
        sbom: PathBuf,
        provenance: PathBuf,
        license_bundle: PathBuf,
    ) -> Self {
        Self {
            metadata,
            metadata_signature,
            artifact_signature,
            artifact,
            sbom,
            provenance,
            license_bundle,
        }
    }

    /// Returns the signed metadata path.
    #[must_use]
    pub fn metadata(&self) -> &Path {
        &self.metadata
    }

    /// Returns the detached signature path.
    #[must_use]
    pub fn metadata_signature(&self) -> &Path {
        &self.metadata_signature
    }

    /// Returns the detached artifact-signature path.
    #[must_use]
    pub fn artifact_signature(&self) -> &Path {
        &self.artifact_signature
    }

    /// Returns the exact package archive path.
    #[must_use]
    pub fn artifact(&self) -> &Path {
        &self.artifact
    }

    /// Returns the artifact-specific CycloneDX SBOM path.
    #[must_use]
    pub fn sbom(&self) -> &Path {
        &self.sbom
    }

    /// Returns the SLSA provenance bundle path.
    #[must_use]
    pub fn provenance(&self) -> &Path {
        &self.provenance
    }

    /// Returns the license and notice bundle path.
    #[must_use]
    pub fn license_bundle(&self) -> &Path {
        &self.license_bundle
    }
}

/// Locally provisioned trust and compatibility policy for update application.
///
/// Production CLI application reads this value only from the private install
/// state. Explicit construction exists for installer and release lifecycle tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustedUpdatePolicy {
    updates_enabled: bool,
    key_id: String,
    public_key: UpdatePublicKey,
    channel: String,
    catalog_schema: u32,
    protocol_major: u32,
    protocol_minor: u32,
    rollout_bucket: u8,
}

impl TrustedUpdatePolicy {
    /// Constructs and validates an explicit trusted update policy.
    ///
    /// # Errors
    ///
    /// Returns [`FilesystemUpdateError::InvalidPolicy`] for invalid labels,
    /// protocol values, catalog schema, or rollout bucket.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        updates_enabled: bool,
        key_id: String,
        public_key: UpdatePublicKey,
        channel: String,
        catalog_schema: u32,
        protocol_major: u32,
        protocol_minor: u32,
        rollout_bucket: u8,
    ) -> Result<Self, FilesystemUpdateError> {
        let policy = Self {
            updates_enabled,
            key_id,
            public_key,
            channel,
            catalog_schema,
            protocol_major,
            protocol_minor,
            rollout_bucket,
        };
        policy.validate()?;
        Ok(policy)
    }

    fn validate(&self) -> Result<(), FilesystemUpdateError> {
        if !valid_label(&self.key_id)
            || !valid_label(&self.channel)
            || self.catalog_schema == 0
            || self.protocol_major == 0
            || self.rollout_bucket >= 100
        {
            return Err(FilesystemUpdateError::InvalidPolicy);
        }
        Ok(())
    }
}

/// Candidate payload health boundary used after side-by-side publication.
pub trait CandidateHealthCheck {
    /// Checks a candidate installation without routing through the active launcher.
    ///
    /// # Errors
    ///
    /// Returns [`CandidateHealthError`] when the isolated probe cannot start,
    /// exceeds `timeout`, exits unsuccessfully, or cannot be reaped.
    fn check(
        &mut self,
        candidate_version_root: &Path,
        catalog_state_root: &Path,
        timeout: Duration,
    ) -> Result<(), CandidateHealthError>;
}

/// Isolated process health check using the candidate CLI and daemon siblings.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessCandidateHealthCheck;

impl CandidateHealthCheck for ProcessCandidateHealthCheck {
    fn check(
        &mut self,
        candidate_version_root: &Path,
        catalog_state_root: &Path,
        timeout: Duration,
    ) -> Result<(), CandidateHealthError> {
        let binary = candidate_version_root
            .join("bin")
            .join(platform_executable_name("rootlight"));
        validate_regular_payload(&binary).map_err(|_| CandidateHealthError)?;
        let probe = candidate_health_tempdir()?;
        let cloned_state = probe.path().join("state");
        clone_candidate_state(catalog_state_root, &cloned_state)?;
        let mut child = Command::new(binary)
            .arg("--update-health-probe")
            .env(UPDATE_HEALTH_STATE_DIR_ENV, &cloned_state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| CandidateHealthError)?;
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(CandidateHealthError)?;
        loop {
            if let Some(status) = child.try_wait().map_err(|_| CandidateHealthError)? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(CandidateHealthError)
                };
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                child.wait().map_err(|_| CandidateHealthError)?;
                return Err(CandidateHealthError);
            }
            std::thread::sleep(HEALTH_POLL_INTERVAL);
        }
    }
}

fn candidate_health_tempdir() -> Result<tempfile::TempDir, CandidateHealthError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("rootlight-update-catalog-probe-");
    #[cfg(target_os = "macos")]
    {
        // The isolated clone is opened through the same no-follow boundary as
        // live state, so avoid macOS's symlinked default `/var` temp path.
        builder
            .tempdir_in(Path::new("/private/tmp"))
            .map_err(|_| CandidateHealthError)
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder.tempdir().map_err(|_| CandidateHealthError)
    }
}

/// Source-free failure from an isolated candidate health check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("candidate health check failed")]
pub struct CandidateHealthError;

fn clone_candidate_state(source: &Path, destination: &Path) -> Result<(), CandidateHealthError> {
    fs::create_dir(destination).map_err(|_| CandidateHealthError)?;
    if !source.exists() {
        return Ok(());
    }
    let root_metadata = fs::symlink_metadata(source).map_err(|_| CandidateHealthError)?;
    if !root_metadata.is_dir() || root_metadata.file_type().is_symlink() {
        return Err(CandidateHealthError);
    }

    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf(), 0_usize)];
    let mut observed_entries = 0_usize;
    let mut observed_bytes = 0_u64;
    while let Some((source_directory, destination_directory, depth)) = pending.pop() {
        if depth > MAX_HEALTH_PROBE_STATE_DEPTH {
            return Err(CandidateHealthError);
        }
        for entry in fs::read_dir(&source_directory).map_err(|_| CandidateHealthError)? {
            let entry = entry.map_err(|_| CandidateHealthError)?;
            observed_entries = observed_entries
                .checked_add(1)
                .filter(|count| *count <= MAX_HEALTH_PROBE_STATE_ENTRIES)
                .ok_or(CandidateHealthError)?;
            let source_path = entry.path();
            let destination_path = destination_directory.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path).map_err(|_| CandidateHealthError)?;
            if metadata.file_type().is_symlink() {
                return Err(CandidateHealthError);
            }
            if metadata.is_dir() {
                fs::create_dir(&destination_path).map_err(|_| CandidateHealthError)?;
                pending.try_reserve(1).map_err(|_| CandidateHealthError)?;
                pending.push((
                    source_path,
                    destination_path,
                    depth.checked_add(1).ok_or(CandidateHealthError)?,
                ));
                continue;
            }
            if !metadata.is_file() {
                return Err(CandidateHealthError);
            }
            observed_bytes = observed_bytes
                .checked_add(metadata.len())
                .filter(|bytes| *bytes <= MAX_HEALTH_PROBE_STATE_BYTES)
                .ok_or(CandidateHealthError)?;
            let mut input = File::open(&source_path).map_err(|_| CandidateHealthError)?;
            let mut output = File::create(&destination_path).map_err(|_| CandidateHealthError)?;
            let copied = io::copy(
                &mut Read::by_ref(&mut input).take(metadata.len().saturating_add(1)),
                &mut output,
            )
            .map_err(|_| CandidateHealthError)?;
            if copied != metadata.len() {
                return Err(CandidateHealthError);
            }
            output.sync_all().map_err(|_| CandidateHealthError)?;
        }
    }
    Ok(())
}

/// Durable update transaction phase shared with the stable launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateTransactionPhase {
    /// The transaction owns a candidate staging or published version.
    Staged,
    /// The isolated candidate health probe may be running.
    HealthChecking,
    /// Candidate health passed and durable active-state commit may be partial.
    CommitPrepared,
    /// Active selector and install manifest both name the healthy candidate.
    Committed,
    /// Recovery must preserve the prior last-good version.
    RollbackPrepared,
}

/// Source-free status for one side-by-side installation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRuntimeStatus {
    /// Currently published active package version.
    pub active_version: String,
    /// Package version selected by the installation manifest.
    pub package_version: String,
    /// Semantic version compiled into the running Rootlight binary.
    pub binary_version: String,
    /// Whether the active package and running binary use the same version.
    pub package_matches_binary: bool,
    /// Retained rollback version.
    pub last_good_version: String,
    /// Pending durable transaction phase, when present.
    pub transaction_phase: Option<UpdateTransactionPhase>,
    /// Candidate version named by a pending transaction.
    pub candidate_version: Option<String>,
    /// Whether [`recover_update`] must run before another application.
    pub recovery_required: bool,
}

/// Successful production application of an exact signed package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemUpdateOutcome {
    /// Newly active semantic version.
    pub version: String,
    /// Version retained as last known good.
    pub previous_version: String,
    /// SHA-256 of the exact signed metadata bytes.
    pub metadata_sha256: String,
    /// SHA-256 of the copied and extracted package archive.
    pub artifact_sha256: String,
    /// Whether stale durable state was recovered before application.
    pub recovered_before_apply: bool,
}

/// Successful bootstrap installation of one exact package archive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageInstallOutcome {
    /// Newly active semantic version.
    pub version: String,
    /// SHA-256 observed while copying the exact package.
    pub artifact_sha256: String,
    /// Number of owned regular files recorded in the install manifest.
    pub owned_file_count: usize,
}

/// Successful removal of package-owned files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageUninstallOutcome {
    /// User-owned data remained outside package removal.
    pub user_data_preserved: bool,
    /// Number of installed version directories removed or queued for removal.
    pub removed_versions: usize,
    /// Whether an installed Windows launcher will finish removal after process exit.
    pub deferred_cleanup: bool,
}

/// Bootstraps a side-by-side installation from one exact package archive.
///
/// `expected_sha256` is a trusted lowercase digest supplied by the installer
/// channel. The private update policy is persisted inside the new installation;
/// the archive never supplies its own trust anchor.
///
/// # Errors
///
/// Returns [`FilesystemUpdateError`] for an existing owned layout, insecure
/// root, digest mismatch, invalid archive, extraction, launcher publication,
/// state serialization, or durability failure.
pub fn install_package_with_policy(
    install_root: &Path,
    artifact: &Path,
    expected_sha256: &str,
    policy: &TrustedUpdatePolicy,
) -> Result<PackageInstallOutcome, FilesystemUpdateError> {
    policy.validate()?;
    if !lower_hex(expected_sha256, 64) {
        return Err(FilesystemUpdateError::InvalidInput);
    }
    let source = open_ambient_regular_no_follow(artifact)?;
    let metadata = source.metadata().map_err(FilesystemUpdateError::Io)?;
    if metadata.len() == 0 || metadata.len() > MAX_UPDATE_ARTIFACT_BYTES {
        return Err(FilesystemUpdateError::InputTooLarge);
    }
    if hash_file_exact(&source, MAX_UPDATE_ARTIFACT_BYTES)? != expected_sha256 {
        return Err(FilesystemUpdateError::ArtifactCopyMismatch);
    }
    prepare_private_install_root(install_root)?;
    let result = (|| {
        let root = Dir::open_ambient_dir(install_root, ambient_authority())
            .map_err(FilesystemUpdateError::Io)?;
        PrivateDirectory::verify_parent(&root).map_err(FilesystemUpdateError::PrivateTree)?;
        let state = PrivateDirectory::create(&root, OsStr::new("state"))
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let versions = PrivateDirectory::create(&root, OsStr::new(VERSIONS_DIRECTORY))
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let current = PrivateDirectory::create(&root, OsStr::new("current"))
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let current_bin = current
            .create_directory(OsStr::new("bin"))
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let layout = InstallLayout {
            root: install_root.to_path_buf(),
            state,
            versions,
        };
        let (copy, observed_sha256) = copy_bootstrap_artifact(&layout, source, metadata.len())?;
        if observed_sha256 != expected_sha256 {
            return Err(FilesystemUpdateError::ArtifactCopyMismatch);
        }
        let package = inspect_bootstrap_package(copy, current_target()?)?;
        let version = package.version.clone();
        let staging = PrivateDirectory::create(
            layout.versions.capability(),
            OsStr::new(&staging_name(&version, expected_sha256)?),
        )
        .map_err(FilesystemUpdateError::PrivateTree)?;
        extract_package(layout.open_bootstrap_copy()?, &package, &staging)?;
        let published = staging
            .publish_noreplace(layout.versions.capability(), OsStr::new(&version))
            .map_err(map_publish_error)?;
        published
            .sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)?;
        install_launchers(
            layout.open_bootstrap_copy()?,
            &package,
            &current_bin,
            install_root,
        )?;
        current_bin
            .sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)?;
        current
            .sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)?;

        let mut owned_paths = package
            .installed_paths(&version)
            .ok_or(FilesystemUpdateError::InvalidArchive)?;
        owned_paths.extend(
            package
                .expected_binaries()
                .iter()
                .map(|binary| format!("current/bin/{}", platform_executable_name(binary))),
        );
        owned_paths.extend(
            [
                "state/active-version",
                "state/install-manifest.json",
                "state/update-policy.json",
                "state/update.lock",
            ]
            .into_iter()
            .map(str::to_owned),
        );
        owned_paths.sort();
        owned_paths.dedup();
        if owned_paths.len() > MAX_OWNED_PATHS {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        let manifest = InstallManifest {
            schema: INSTALL_MANIFEST_SCHEMA_V2.to_owned(),
            target: current_target()?.to_owned(),
            active_version: version.clone(),
            last_good_version: Some(version.clone()),
            owned_paths,
            platform_resources: Vec::new(),
        };
        layout.write_policy(policy)?;
        layout.write_install_manifest(&manifest)?;
        layout.write_active_version(&version)?;
        let lock = layout.acquire_lock()?;
        drop(lock);
        layout.remove_state_file_if_exists(BOOTSTRAP_ARTIFACT_COPY_NAME)?;
        Ok(PackageInstallOutcome {
            version,
            artifact_sha256: observed_sha256,
            owned_file_count: manifest.owned_paths.len(),
        })
    })();
    if result.is_err() && cleanup_failed_bootstrap(install_root).is_err() {
        return Err(FilesystemUpdateError::RecoveryFailed);
    }
    result
}

/// Removes only package-owned installation files while preserving `user/`.
///
/// Uninstall refuses installations with registered platform resources because
/// those require their platform-specific deregistration owner.
///
/// # Errors
///
/// Returns [`FilesystemUpdateError`] for lock, recovery, foreign resources,
/// insecure owned paths, removal, or durability failure.
pub fn uninstall_package(
    install_root: &Path,
) -> Result<PackageUninstallOutcome, FilesystemUpdateError> {
    let layout = InstallLayout::open(install_root)?;
    let lock = layout.acquire_lock()?;
    layout.recover_locked()?;
    let manifest = layout.read_install_manifest()?;
    if !manifest.platform_resources.is_empty() {
        return Err(FilesystemUpdateError::PlatformResourcesRemain);
    }
    let versions = manifest.installed_versions()?;
    #[cfg(windows)]
    if let Some(token) = deferred_uninstall_token()? {
        let active = layout.read_active_version()?;
        if manifest.active_version != active
            || !current_executable_is_owned_payload(install_root, &manifest, &versions)?
        {
            return Err(FilesystemUpdateError::InconsistentState);
        }
        let request = DeferredUninstallRequest {
            schema: DEFERRED_UNINSTALL_SCHEMA.to_owned(),
            token,
            target: manifest.target,
            launcher_pid: std::env::var(DEFERRED_UNINSTALL_LAUNCHER_PID_ENV)
                .map_err(|_| FilesystemUpdateError::InvalidInstall)?
                .parse()
                .map_err(|_| FilesystemUpdateError::InvalidInstall)?,
            payload_pid: std::process::id(),
            versions: versions.iter().cloned().collect(),
        };
        request.validate()?;
        layout.write_state_atomic(
            DEFERRED_UNINSTALL_FILE,
            &serialize_bounded(&request, MAX_STATE_BYTES)?,
        )?;
        drop(lock);
        return Ok(PackageUninstallOutcome {
            user_data_preserved: true,
            removed_versions: versions.len(),
            deferred_cleanup: true,
        });
    }
    for version in &versions {
        layout.remove_version_if_exists(version)?;
    }
    let root = Dir::open_ambient_dir(install_root, ambient_authority())
        .map_err(FilesystemUpdateError::Io)?;
    remove_owned_current(&root, &manifest)?;
    for name in [
        UPDATE_POLICY_FILE,
        ACTIVE_VERSION_FILE,
        INSTALL_MANIFEST_FILE,
        UPDATE_TRANSACTION_FILE,
        ARTIFACT_COPY_NAME,
        BOOTSTRAP_ARTIFACT_COPY_NAME,
        DEFERRED_UNINSTALL_FILE,
    ] {
        layout.remove_state_file_if_exists(name)?;
    }
    drop(lock);
    layout.remove_state_file_if_exists(UPDATE_LOCK_FILE)?;
    drop(layout);
    root.remove_dir("state")
        .map_err(FilesystemUpdateError::Io)?;
    root.remove_dir(VERSIONS_DIRECTORY)
        .map_err(FilesystemUpdateError::Io)?;
    Ok(PackageUninstallOutcome {
        user_data_preserved: true,
        removed_versions: versions.len(),
        deferred_cleanup: false,
    })
}

/// Applies an exact signed package using only private installed trust policy.
///
/// The production policy is loaded from `state/update-policy.json`; no caller
/// context or public-key path can override it.
///
/// # Errors
///
/// Returns [`FilesystemUpdateError`] for layout, locking, recovery, policy,
/// signature, archive, extraction, health, publication, or durability failure.
pub fn apply_update_package(
    install_root: &Path,
    catalog_state_root: &Path,
    inputs: &UpdateInputPaths,
    health: &mut impl CandidateHealthCheck,
) -> Result<FilesystemUpdateOutcome, FilesystemUpdateError> {
    let layout = InstallLayout::open(install_root)?;
    let _lock = layout.acquire_lock()?;
    let recovered = layout.recover_locked()?;
    let policy = layout.read_policy()?;
    let result = layout.apply_locked(inputs, &policy, catalog_state_root, health, recovered);
    if result.is_err() {
        layout.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
    }
    result
}

/// Applies an exact signed package with an explicitly injected trusted policy.
///
/// This entry point is intended for installer and release lifecycle harnesses.
/// Production CLI code must call [`apply_update_package`] instead.
///
/// # Errors
///
/// Returns [`FilesystemUpdateError`] for invalid injected policy, layout,
/// locking, recovery, verification, archive, health, or durable publication.
pub fn apply_update_package_with_policy(
    install_root: &Path,
    catalog_state_root: &Path,
    inputs: &UpdateInputPaths,
    policy: &TrustedUpdatePolicy,
    health: &mut impl CandidateHealthCheck,
) -> Result<FilesystemUpdateOutcome, FilesystemUpdateError> {
    policy.validate()?;
    let layout = InstallLayout::open(install_root)?;
    let _lock = layout.acquire_lock()?;
    let recovered = layout.recover_locked()?;
    let result = layout.apply_locked(inputs, policy, catalog_state_root, health, recovered);
    if result.is_err() {
        layout.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
    }
    result
}

/// Recovers or finalizes a durable update transaction.
///
/// Pre-commit and ambiguous state rolls back to the retained previous version.
/// A commit-prepared transaction finalizes only when both active state files
/// already agree on the healthy candidate.
///
/// # Errors
///
/// Returns [`FilesystemUpdateError`] for layout, lock, malformed state,
/// rollback publication, cleanup, or durability failure.
pub fn recover_update(install_root: &Path) -> Result<UpdateRuntimeStatus, FilesystemUpdateError> {
    let layout = InstallLayout::open(install_root)?;
    let _lock = layout.acquire_lock()?;
    layout.recover_locked()?;
    layout.status_locked()
}

/// Reads strict bounded update state without mutating the installation.
///
/// # Errors
///
/// Returns [`FilesystemUpdateError`] for invalid layout, manifest, selector,
/// or transaction state.
pub fn update_runtime_status(
    install_root: &Path,
) -> Result<UpdateRuntimeStatus, FilesystemUpdateError> {
    let layout = InstallLayout::open(install_root)?;
    let _lock = layout.acquire_lock()?;
    layout.status_locked()
}

struct InstallLayout {
    root: PathBuf,
    state: PrivateDirectory<'static>,
    versions: PrivateDirectory<'static>,
}

struct UpdateVerificationInputs<'input, 'tree> {
    metadata: &'input [u8],
    metadata_signature: DetachedUpdateSignature,
    artifact_signature: DetachedArtifactSignature,
    source: File,
    copy: rootlight_vfs::platform::PrivateFile<'tree>,
    sbom: &'input [u8],
    provenance: &'input [u8],
    license_bundle: &'input [u8],
}

impl InstallLayout {
    fn open(root: &Path) -> Result<Self, FilesystemUpdateError> {
        PrivateDirectory::require_supported().map_err(FilesystemUpdateError::PrivateTree)?;
        if !root.is_absolute() {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        validate_directory_no_reparse(root)?;
        let root_capability =
            Dir::open_ambient_dir(root, ambient_authority()).map_err(FilesystemUpdateError::Io)?;
        PrivateDirectory::verify_parent(&root_capability)
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let state = PrivateDirectory::open(&root_capability, OsStr::new("state"))
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let versions = PrivateDirectory::open(&root_capability, OsStr::new(VERSIONS_DIRECTORY))
            .map_err(FilesystemUpdateError::PrivateTree)?;
        validate_directory_no_reparse(&root.join("current"))?;
        Ok(Self {
            root: root.to_path_buf(),
            state,
            versions,
        })
    }

    fn acquire_lock(&self) -> Result<UpdateLock, FilesystemUpdateError> {
        match self.state.create_file(OsStr::new(UPDATE_LOCK_FILE)) {
            Ok(file) => {
                file.sync_all()
                    .map_err(FilesystemUpdateError::PrivateTree)?;
                drop(file);
                self.state
                    .sync_all()
                    .map_err(FilesystemUpdateError::PrivateTree)?;
            }
            Err(error) if error.is_already_exists() => {}
            Err(error) => return Err(FilesystemUpdateError::PrivateTree(error)),
        }
        validate_cap_regular(self.state.capability(), UPDATE_LOCK_FILE)?;
        let file = self
            .state
            .capability()
            .open(UPDATE_LOCK_FILE)
            .map_err(FilesystemUpdateError::Io)?
            .into_std();
        match file.try_lock() {
            Ok(()) => Ok(UpdateLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(FilesystemUpdateError::Busy),
            Err(std::fs::TryLockError::Error(source)) => Err(FilesystemUpdateError::Io(source)),
        }
    }

    fn read_policy(&self) -> Result<TrustedUpdatePolicy, FilesystemUpdateError> {
        let bytes = self
            .state
            .read_file_bounded(OsStr::new(UPDATE_POLICY_FILE), MAX_POLICY_BYTES)
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let wire: TrustedUpdatePolicyWire =
            serde_json::from_slice(&bytes).map_err(|_| FilesystemUpdateError::InvalidPolicy)?;
        if wire.schema != UPDATE_POLICY_SCHEMA {
            return Err(FilesystemUpdateError::InvalidPolicy);
        }
        TrustedUpdatePolicy::new(
            wire.updates_enabled,
            wire.key_id,
            UpdatePublicKey::from_hex(&wire.public_key_hex)
                .map_err(FilesystemUpdateError::Verification)?,
            wire.channel,
            wire.catalog_schema,
            wire.protocol_major,
            wire.protocol_minor,
            wire.rollout_bucket,
        )
    }

    fn write_policy(&self, policy: &TrustedUpdatePolicy) -> Result<(), FilesystemUpdateError> {
        policy.validate()?;
        let wire = TrustedUpdatePolicyWire {
            schema: UPDATE_POLICY_SCHEMA.to_owned(),
            updates_enabled: policy.updates_enabled,
            key_id: policy.key_id.clone(),
            public_key_hex: encode_hex(policy.public_key.as_bytes()),
            channel: policy.channel.clone(),
            catalog_schema: policy.catalog_schema,
            protocol_major: policy.protocol_major,
            protocol_minor: policy.protocol_minor,
            rollout_bucket: policy.rollout_bucket,
        };
        let bytes = serialize_bounded(&wire, MAX_POLICY_BYTES)?;
        self.write_state_atomic(UPDATE_POLICY_FILE, &bytes)
    }

    fn apply_locked(
        &self,
        inputs: &UpdateInputPaths,
        policy: &TrustedUpdatePolicy,
        catalog_state_root: &Path,
        health: &mut impl CandidateHealthCheck,
        recovered_before_apply: bool,
    ) -> Result<FilesystemUpdateOutcome, FilesystemUpdateError> {
        let mut install = self.read_install_manifest()?;
        let active = self.read_active_version()?;
        if install.active_version != active {
            return Err(FilesystemUpdateError::InconsistentState);
        }

        let metadata = read_ambient_regular_bounded(
            &inputs.metadata,
            u64::try_from(MAX_UPDATE_METADATA_BYTES)
                .map_err(|_| FilesystemUpdateError::InputTooLarge)?,
        )?;
        let metadata_signature = read_metadata_signature(&inputs.metadata_signature)?;
        let artifact_signature = read_artifact_signature(&inputs.artifact_signature)?;
        let sbom = read_ambient_regular_bounded(&inputs.sbom, MAX_UPDATE_SBOM_BYTES)?;
        let provenance =
            read_ambient_regular_bounded(&inputs.provenance, MAX_UPDATE_PROVENANCE_BYTES)?;
        let license_bundle =
            read_ambient_regular_bounded(&inputs.license_bundle, MAX_UPDATE_LICENSE_BUNDLE_BYTES)?;
        let source = open_ambient_regular_no_follow(&inputs.artifact)?;
        let source_metadata = source.metadata().map_err(FilesystemUpdateError::Io)?;
        if source_metadata.len() == 0 || source_metadata.len() > MAX_UPDATE_ARTIFACT_BYTES {
            return Err(FilesystemUpdateError::InputTooLarge);
        }
        let copy = self.create_artifact_copy()?;
        let verification_inputs = UpdateVerificationInputs {
            metadata: &metadata,
            metadata_signature,
            artifact_signature,
            source,
            copy,
            sbom: &sbom,
            provenance: &provenance,
            license_bundle: &license_bundle,
        };
        let (verified, copied_file) =
            self.verify_and_copy(verification_inputs, &install, policy)?;
        if verified.key_id != policy.key_id {
            self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
            return Err(FilesystemUpdateError::InvalidPolicy);
        }
        let archive_name = inputs
            .artifact
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or(FilesystemUpdateError::InvalidArchive)?;
        if archive_name != verified.plan.artifact_file_name {
            self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
            return Err(FilesystemUpdateError::InvalidArchive);
        }
        let package = inspect_package(copied_file, &verified, &install.target)?;
        let previous_version = install.active_version.clone();
        let staging_name = staging_name(&verified.version, &verified.metadata_sha256)?;
        let candidate_owned_paths = package
            .installed_paths(&verified.version)
            .ok_or(FilesystemUpdateError::InvalidArchive)?;
        let mut transaction = UpdateTransaction {
            schema: UPDATE_TRANSACTION_SCHEMA.to_owned(),
            phase: UpdateTransactionPhase::Staged,
            previous_version: previous_version.clone(),
            candidate_version: verified.version.clone(),
            target: install.target.clone(),
            staging_name,
            metadata_sha256: verified.metadata_sha256.clone(),
            artifact_sha256: verified.artifact_sha256.clone(),
            candidate_owned_paths,
        };
        transaction.validate()?;
        self.write_transaction(&transaction)?;

        let result = self.stage_and_activate(
            &verified,
            &package,
            &mut transaction,
            catalog_state_root,
            health,
            &mut install,
        );
        if let Err(error) = result {
            let recovery = self.rollback_transaction(&transaction);
            self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
            return match recovery {
                Ok(()) => Err(error),
                Err(_) => Err(FilesystemUpdateError::RecoveryFailed),
            };
        }
        self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
        Ok(FilesystemUpdateOutcome {
            version: verified.version,
            previous_version,
            metadata_sha256: verified.metadata_sha256,
            artifact_sha256: verified.artifact_sha256,
            recovered_before_apply,
        })
    }

    fn verify_and_copy(
        &self,
        inputs: UpdateVerificationInputs<'_, '_>,
        install: &InstallManifest,
        policy: &TrustedUpdatePolicy,
    ) -> Result<(VerifiedUpdate, File), FilesystemUpdateError> {
        let UpdateVerificationInputs {
            metadata,
            metadata_signature,
            artifact_signature,
            mut source,
            mut copy,
            sbom,
            provenance,
            license_bundle,
        } = inputs;
        let context = self.trusted_context(install, policy)?;
        let mut reader = CopyingReader {
            source: &mut source,
            destination: &mut copy,
        };
        let signatures =
            UpdateSignatures::new(metadata_signature, artifact_signature, policy.public_key);
        let mut sbom = Cursor::new(sbom);
        let mut provenance = Cursor::new(provenance);
        let mut license_bundle = Cursor::new(license_bundle);
        let mut supporting =
            UpdateSupportingEvidence::new(&mut sbom, &mut provenance, &mut license_bundle);
        let verified = verify_update_with_evidence(
            metadata,
            signatures,
            &mut reader,
            &mut supporting,
            &context,
        )
        .map_err(FilesystemUpdateError::Verification)?;
        copy.sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)?;
        drop(copy);
        self.state
            .sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let copied = self
            .state
            .capability()
            .open(ARTIFACT_COPY_NAME)
            .map_err(FilesystemUpdateError::Io)?
            .into_std();
        let observed = hash_file_exact(&copied, MAX_UPDATE_ARTIFACT_BYTES)?;
        if observed != verified.artifact_sha256 {
            return Err(FilesystemUpdateError::ArtifactCopyMismatch);
        }
        Ok((verified, copied))
    }

    fn trusted_context(
        &self,
        install: &InstallManifest,
        policy: &TrustedUpdatePolicy,
    ) -> Result<UpdateContext, FilesystemUpdateError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| FilesystemUpdateError::Clock)?
            .as_secs();
        let available =
            fs2::available_space(&self.root).map_err(FilesystemUpdateError::DiskSpace)?;
        Ok(UpdateContext {
            updates_enabled: policy.updates_enabled,
            current_version: install.active_version.clone(),
            last_good_version: install.last_good_version().to_owned(),
            channel: policy.channel.clone(),
            platform: current_platform()?.to_owned(),
            architecture: current_architecture()?.to_owned(),
            now_unix_seconds: now,
            catalog_schema: policy.catalog_schema,
            protocol_major: policy.protocol_major,
            protocol_minor: policy.protocol_minor,
            available_disk_bytes: available,
            rollout_bucket: policy.rollout_bucket,
        })
    }

    fn stage_and_activate(
        &self,
        verified: &VerifiedUpdate,
        package: &PackageManifest,
        transaction: &mut UpdateTransaction,
        catalog_state_root: &Path,
        health: &mut impl CandidateHealthCheck,
        install: &mut InstallManifest,
    ) -> Result<(), FilesystemUpdateError> {
        if self.version_exists(&verified.version)? {
            return Err(FilesystemUpdateError::VersionAlreadyInstalled);
        }
        self.remove_staging_if_exists(&transaction.staging_name)?;
        let staging = PrivateDirectory::create(
            self.versions.capability(),
            OsStr::new(&transaction.staging_name),
        )
        .map_err(FilesystemUpdateError::PrivateTree)?;
        let archive = self.open_artifact_copy()?;
        extract_package(archive, package, &staging)?;
        staging
            .sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)?;
        match staging.publish_noreplace(self.versions.capability(), OsStr::new(&verified.version)) {
            Ok(published) => published
                .sync_all()
                .map_err(FilesystemUpdateError::PrivateTree)?,
            Err(PublishError::NotCommitted { source }) => {
                return Err(FilesystemUpdateError::PrivateTree(source));
            }
            Err(PublishError::CommittedButDurabilityUnknown { .. }) => {
                return Err(FilesystemUpdateError::PublicationDurabilityUnknown);
            }
            Err(_) => return Err(FilesystemUpdateError::PublicationDurabilityUnknown),
        }

        transaction.phase = UpdateTransactionPhase::HealthChecking;
        self.write_transaction(transaction)?;
        health
            .check(
                &self.root.join(VERSIONS_DIRECTORY).join(&verified.version),
                catalog_state_root,
                Duration::from_secs(u64::from(verified.plan.health_timeout_seconds)),
            )
            .map_err(FilesystemUpdateError::Health)?;
        transaction.phase = UpdateTransactionPhase::CommitPrepared;
        self.write_transaction(transaction)?;

        install
            .owned_paths
            .extend(transaction.candidate_owned_paths.iter().cloned());
        install.owned_paths.sort();
        install.owned_paths.dedup();
        if install.owned_paths.len() > MAX_OWNED_PATHS {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        install.last_good_version = Some(transaction.previous_version.clone());
        install.active_version = transaction.candidate_version.clone();
        install.schema = INSTALL_MANIFEST_SCHEMA_V2.to_owned();
        self.prune_unretained_versions(install, transaction)?;
        self.write_install_manifest(install)?;
        self.write_active_version(&transaction.candidate_version)?;

        transaction.phase = UpdateTransactionPhase::Committed;
        self.write_transaction(transaction)?;
        self.remove_state_file_if_exists(UPDATE_TRANSACTION_FILE)?;
        Ok(())
    }

    fn recover_locked(&self) -> Result<bool, FilesystemUpdateError> {
        let Some(transaction) = self.read_transaction()? else {
            self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
            return Ok(false);
        };
        let mut install = self.read_install_manifest()?;
        let active = self.read_active_version()?;
        let can_finalize = transaction.phase == UpdateTransactionPhase::Committed
            || (transaction.phase == UpdateTransactionPhase::CommitPrepared
                && active == transaction.candidate_version
                && install.active_version == transaction.candidate_version
                && install.last_good_version() == transaction.previous_version);
        if can_finalize {
            if active != transaction.candidate_version
                || install.active_version != transaction.candidate_version
            {
                self.rollback_transaction_with_manifest(&transaction, &mut install)?;
            } else {
                self.remove_state_file_if_exists(UPDATE_TRANSACTION_FILE)?;
            }
        } else {
            self.rollback_transaction_with_manifest(&transaction, &mut install)?;
        }
        self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
        Ok(true)
    }

    fn rollback_transaction(
        &self,
        transaction: &UpdateTransaction,
    ) -> Result<(), FilesystemUpdateError> {
        let mut install = self.read_install_manifest()?;
        self.rollback_transaction_with_manifest(transaction, &mut install)
    }

    fn rollback_transaction_with_manifest(
        &self,
        transaction: &UpdateTransaction,
        install: &mut InstallManifest,
    ) -> Result<(), FilesystemUpdateError> {
        let mut rollback = transaction.clone();
        rollback.phase = UpdateTransactionPhase::RollbackPrepared;
        self.write_transaction(&rollback)?;
        if !install.owns_version(&transaction.previous_version) {
            return Err(FilesystemUpdateError::RecoveryFailed);
        }
        install
            .owned_paths
            .retain(|path| !transaction.candidate_owned_paths.contains(path));
        install.active_version = transaction.previous_version.clone();
        install.last_good_version = Some(transaction.previous_version.clone());
        install.schema = INSTALL_MANIFEST_SCHEMA_V2.to_owned();
        self.write_install_manifest(install)?;
        self.write_active_version(&transaction.previous_version)?;
        self.remove_version_if_exists(&transaction.candidate_version)?;
        self.remove_staging_if_exists(&transaction.staging_name)?;
        self.remove_state_file_if_exists(UPDATE_TRANSACTION_FILE)
    }

    fn status_locked(&self) -> Result<UpdateRuntimeStatus, FilesystemUpdateError> {
        let install = self.read_install_manifest()?;
        let active = self.read_active_version()?;
        if install.active_version != active {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        let transaction = self.read_transaction()?;
        let binary_version = env!("CARGO_PKG_VERSION").to_owned();
        Ok(UpdateRuntimeStatus {
            active_version: active.clone(),
            package_matches_binary: active == binary_version,
            package_version: active,
            binary_version,
            last_good_version: install.last_good_version().to_owned(),
            transaction_phase: transaction.as_ref().map(|value| value.phase),
            candidate_version: transaction
                .as_ref()
                .map(|value| value.candidate_version.clone()),
            recovery_required: transaction.is_some(),
        })
    }

    fn read_install_manifest(&self) -> Result<InstallManifest, FilesystemUpdateError> {
        let bytes = self
            .state
            .read_file_bounded(OsStr::new(INSTALL_MANIFEST_FILE), MAX_STATE_BYTES)
            .map_err(FilesystemUpdateError::PrivateTree)?;
        let manifest: InstallManifest =
            serde_json::from_slice(&bytes).map_err(|_| FilesystemUpdateError::InvalidInstall)?;
        manifest.validate()?;
        Ok(manifest)
    }

    fn write_install_manifest(
        &self,
        manifest: &InstallManifest,
    ) -> Result<(), FilesystemUpdateError> {
        manifest.validate()?;
        let bytes = serialize_bounded(manifest, MAX_STATE_BYTES)?;
        self.write_state_atomic(INSTALL_MANIFEST_FILE, &bytes)
    }

    fn read_active_version(&self) -> Result<String, FilesystemUpdateError> {
        let bytes = self
            .state
            .read_file_bounded(OsStr::new(ACTIVE_VERSION_FILE), 128)
            .map_err(FilesystemUpdateError::PrivateTree)?;
        parse_active_version(&bytes)
    }

    fn write_active_version(&self, version: &str) -> Result<(), FilesystemUpdateError> {
        canonical_version(version)?;
        self.write_state_atomic(ACTIVE_VERSION_FILE, format!("{version}\n").as_bytes())
    }

    fn read_transaction(&self) -> Result<Option<UpdateTransaction>, FilesystemUpdateError> {
        let bytes = match self
            .state
            .read_file_bounded(OsStr::new(UPDATE_TRANSACTION_FILE), MAX_STATE_BYTES)
        {
            Ok(bytes) => bytes,
            Err(PlatformError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(FilesystemUpdateError::PrivateTree(error)),
        };
        let transaction: UpdateTransaction = serde_json::from_slice(&bytes)
            .map_err(|_| FilesystemUpdateError::InvalidTransaction)?;
        transaction.validate()?;
        Ok(Some(transaction))
    }

    fn write_transaction(
        &self,
        transaction: &UpdateTransaction,
    ) -> Result<(), FilesystemUpdateError> {
        transaction.validate()?;
        let bytes = serialize_bounded(transaction, MAX_STATE_BYTES)?;
        self.write_state_atomic(UPDATE_TRANSACTION_FILE, &bytes)
    }

    fn write_state_atomic(
        &self,
        destination: &str,
        bytes: &[u8],
    ) -> Result<(), FilesystemUpdateError> {
        let temporary = format!(".{destination}.new");
        self.remove_state_file_if_exists(&temporary)?;
        {
            let mut file = self
                .state
                .create_file(OsStr::new(&temporary))
                .map_err(FilesystemUpdateError::PrivateTree)?;
            file.write_all(bytes).map_err(FilesystemUpdateError::Io)?;
            file.sync_all()
                .map_err(FilesystemUpdateError::PrivateTree)?;
        }
        self.state
            .capability()
            .rename(&temporary, self.state.capability(), destination)
            .map_err(FilesystemUpdateError::Io)?;
        self.state
            .sync_all()
            .map_err(FilesystemUpdateError::PrivateTree)
    }

    fn create_artifact_copy(
        &self,
    ) -> Result<rootlight_vfs::platform::PrivateFile<'_>, FilesystemUpdateError> {
        self.remove_state_file_if_exists(ARTIFACT_COPY_NAME)?;
        self.state
            .create_file(OsStr::new(ARTIFACT_COPY_NAME))
            .map_err(FilesystemUpdateError::PrivateTree)
    }

    fn open_artifact_copy(&self) -> Result<File, FilesystemUpdateError> {
        validate_cap_regular(self.state.capability(), ARTIFACT_COPY_NAME)?;
        self.state
            .capability()
            .open(ARTIFACT_COPY_NAME)
            .map(cap_std::fs::File::into_std)
            .map_err(FilesystemUpdateError::Io)
    }

    fn open_bootstrap_copy(&self) -> Result<File, FilesystemUpdateError> {
        validate_cap_regular(self.state.capability(), BOOTSTRAP_ARTIFACT_COPY_NAME)?;
        self.state
            .capability()
            .open(BOOTSTRAP_ARTIFACT_COPY_NAME)
            .map(cap_std::fs::File::into_std)
            .map_err(FilesystemUpdateError::Io)
    }

    fn remove_state_file_if_exists(&self, name: &str) -> Result<(), FilesystemUpdateError> {
        match self.state.capability().symlink_metadata(name) {
            Ok(metadata)
                if metadata.file_type().is_file() && !metadata.file_type().is_symlink() =>
            {
                self.state
                    .capability()
                    .remove_file(name)
                    .map_err(FilesystemUpdateError::Io)?;
                self.state
                    .sync_all()
                    .map_err(FilesystemUpdateError::PrivateTree)
            }
            Ok(_) => Err(FilesystemUpdateError::InsecureState),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(FilesystemUpdateError::Io(source)),
        }
    }

    fn version_exists(&self, version: &str) -> Result<bool, FilesystemUpdateError> {
        match self.versions.capability().symlink_metadata(version) {
            Ok(_) => Ok(true),
            Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(source) => Err(FilesystemUpdateError::Io(source)),
        }
    }

    fn remove_version_if_exists(&self, version: &str) -> Result<(), FilesystemUpdateError> {
        canonical_version(version)?;
        let directory =
            match PrivateDirectory::open(self.versions.capability(), OsStr::new(version)) {
                Ok(directory) => directory,
                Err(PlatformError::Io { source, .. })
                    if source.kind() == io::ErrorKind::NotFound =>
                {
                    return Ok(());
                }
                Err(error) => return Err(FilesystemUpdateError::PrivateTree(error)),
            };
        directory
            .remove()
            .map_err(FilesystemUpdateError::PrivateTree)
    }

    fn remove_staging_if_exists(&self, name: &str) -> Result<(), FilesystemUpdateError> {
        if !valid_staging_name(name) {
            return Err(FilesystemUpdateError::InvalidTransaction);
        }
        let directory = match PrivateDirectory::open(self.versions.capability(), OsStr::new(name)) {
            Ok(directory) => directory,
            Err(PlatformError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(());
            }
            Err(error) => return Err(FilesystemUpdateError::PrivateTree(error)),
        };
        directory
            .remove()
            .map_err(FilesystemUpdateError::PrivateTree)
    }

    fn prune_unretained_versions(
        &self,
        install: &mut InstallManifest,
        transaction: &UpdateTransaction,
    ) -> Result<(), FilesystemUpdateError> {
        let retained = [
            transaction.candidate_version.as_str(),
            transaction.previous_version.as_str(),
        ];
        let versions = install.installed_versions()?;
        for version in versions {
            if !retained.contains(&version.as_str()) {
                self.remove_version_if_exists(&version)?;
                let prefix = format!("versions/{version}/");
                install
                    .owned_paths
                    .retain(|path| !path.starts_with(&prefix));
            }
        }
        Ok(())
    }
}

struct UpdateLock {
    file: File,
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

struct CopyingReader<'a, R, W> {
    source: &'a mut R,
    destination: &'a mut W,
}

impl<R: Read, W: Write> Read for CopyingReader<'_, R, W> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.source.read(buffer)?;
        if read != 0 {
            self.destination.write_all(&buffer[..read])?;
        }
        Ok(read)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TrustedUpdatePolicyWire {
    schema: String,
    updates_enabled: bool,
    key_id: String,
    public_key_hex: String,
    channel: String,
    catalog_schema: u32,
    protocol_major: u32,
    protocol_minor: u32,
    rollout_bucket: u8,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PlatformResource {
    kind: String,
    id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstallManifest {
    schema: String,
    target: String,
    active_version: String,
    #[serde(default)]
    last_good_version: Option<String>,
    owned_paths: Vec<String>,
    platform_resources: Vec<PlatformResource>,
}

impl InstallManifest {
    fn validate(&self) -> Result<(), FilesystemUpdateError> {
        if !matches!(
            self.schema.as_str(),
            INSTALL_MANIFEST_SCHEMA_V1 | INSTALL_MANIFEST_SCHEMA_V2
        ) || self.target != current_target()?
            || self.owned_paths.len() > MAX_OWNED_PATHS
            || self.platform_resources.len() > 16
        {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        canonical_version(&self.active_version)?;
        canonical_version(self.last_good_version())?;
        let mut previous = None;
        for path in &self.owned_paths {
            if !valid_relative_path(path) || previous.is_some_and(|value| value >= path) {
                return Err(FilesystemUpdateError::InvalidInstall);
            }
            previous = Some(path);
        }
        if !self.owns_version(&self.active_version)
            || !self.owns_version(self.last_good_version())
            || self
                .platform_resources
                .iter()
                .any(|resource| !valid_label(&resource.kind) || !valid_label(&resource.id))
        {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        Ok(())
    }

    fn last_good_version(&self) -> &str {
        self.last_good_version
            .as_deref()
            .unwrap_or(&self.active_version)
    }

    fn owns_version(&self, version: &str) -> bool {
        let expected = format!(
            "versions/{version}/bin/{}",
            platform_executable_name("rootlight")
        );
        self.owned_paths
            .binary_search_by(|candidate| candidate.as_str().cmp(&expected))
            .is_ok()
    }

    fn installed_versions(&self) -> Result<BTreeSet<String>, FilesystemUpdateError> {
        let mut versions = BTreeSet::new();
        for path in &self.owned_paths {
            let mut components = Path::new(path).components();
            if components.next() != Some(Component::Normal(OsStr::new("versions"))) {
                continue;
            }
            let version = components
                .next()
                .and_then(|component| match component {
                    Component::Normal(value) => value.to_str(),
                    _ => None,
                })
                .ok_or(FilesystemUpdateError::InvalidInstall)?;
            canonical_version(version)?;
            versions.insert(version.to_owned());
        }
        Ok(versions)
    }
}

#[cfg(windows)]
#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct DeferredUninstallRequest {
    schema: String,
    token: String,
    target: String,
    launcher_pid: u32,
    payload_pid: u32,
    versions: Vec<String>,
}

#[cfg(windows)]
impl DeferredUninstallRequest {
    fn validate(&self) -> Result<(), FilesystemUpdateError> {
        if self.schema != DEFERRED_UNINSTALL_SCHEMA
            || !lower_hex(&self.token, 32)
            || self.target != current_target()?
            || self.launcher_pid == 0
            || self.payload_pid == 0
            || self.launcher_pid == self.payload_pid
            || self.versions.is_empty()
            || self.versions.len() > MAX_OWNED_PATHS
        {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        let mut previous = None;
        for version in &self.versions {
            canonical_version(version)?;
            if previous.is_some_and(|value| value >= version) {
                return Err(FilesystemUpdateError::InvalidInstall);
            }
            previous = Some(version);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct UpdateTransaction {
    schema: String,
    phase: UpdateTransactionPhase,
    previous_version: String,
    candidate_version: String,
    target: String,
    staging_name: String,
    metadata_sha256: String,
    artifact_sha256: String,
    candidate_owned_paths: Vec<String>,
}

impl UpdateTransaction {
    fn validate(&self) -> Result<(), FilesystemUpdateError> {
        canonical_version(&self.previous_version)?;
        canonical_version(&self.candidate_version)?;
        if self.schema != UPDATE_TRANSACTION_SCHEMA
            || self.previous_version == self.candidate_version
            || self.target != current_target()?
            || !valid_staging_name(&self.staging_name)
            || !lower_hex(&self.metadata_sha256, 64)
            || !lower_hex(&self.artifact_sha256, 64)
            || self.candidate_owned_paths.is_empty()
            || self.candidate_owned_paths.len() > MAX_OWNED_PATHS
            || self
                .candidate_owned_paths
                .iter()
                .any(|path| !valid_relative_path(path))
        {
            return Err(FilesystemUpdateError::InvalidTransaction);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifest {
    schema: String,
    target: String,
    version: String,
    source_revision: String,
    autostart_default: String,
    autostart_kind: String,
    autostart_resource: String,
    user_data_policy: String,
    ownership_manifest: String,
    active_version_file: String,
    launcher_binary: String,
    #[serde(default)]
    mcp_launcher_binary: Option<String>,
    versions_directory: String,
    launcher_directory: String,
    update_lock_file: String,
    update_transaction_file: String,
    retained_versions: u8,
    entries: Vec<PackageManifestEntry>,
}

impl PackageManifest {
    fn validate(
        &self,
        verified: &VerifiedUpdate,
        target: &str,
    ) -> Result<(), FilesystemUpdateError> {
        self.validate_identity(&verified.version, target)
    }

    fn validate_identity(
        &self,
        expected_version: &str,
        target: &str,
    ) -> Result<(), FilesystemUpdateError> {
        canonical_version(&self.version)?;
        let launcher = format!(
            "launcher/{}",
            platform_executable_name("rootlight-launcher")
        );
        let mcp_launcher = format!(
            "launcher/{}",
            platform_executable_name("rootlight-mcp-launcher")
        );
        let launcher_schema_is_valid = match self.schema.as_str() {
            PACKAGE_MANIFEST_SCHEMA_V1 => self.mcp_launcher_binary.is_none(),
            PACKAGE_MANIFEST_SCHEMA_V2 => self.mcp_launcher_binary.as_ref() == Some(&mcp_launcher),
            PACKAGE_MANIFEST_SCHEMA_V3 => self.mcp_launcher_binary.as_ref() == Some(&mcp_launcher),
            _ => false,
        };
        if !launcher_schema_is_valid
            || self.target != target
            || self.version != expected_version
            || !(lower_hex(&self.source_revision, 40) || lower_hex(&self.source_revision, 64))
            || self.autostart_default != "disabled"
            || !valid_label(&self.autostart_kind)
            || !valid_label(&self.autostart_resource)
            || self.user_data_policy != "preserve"
            || self.ownership_manifest != "state/install-manifest.json"
            || self.active_version_file != "state/active-version"
            || self.launcher_binary != launcher
            || self
                .mcp_launcher_binary
                .as_ref()
                .is_some_and(|path| path != &mcp_launcher || path == &self.launcher_binary)
            || self.versions_directory != VERSIONS_DIRECTORY
            || self.launcher_directory != LAUNCHER_DIRECTORY
            || self.update_lock_file != "state/update.lock"
            || self.update_transaction_file != "state/update-transaction.json"
            || self.retained_versions != RETAINED_VERSIONS
            || self.entries.is_empty()
            || self.entries.len() > MAX_PACKAGE_ENTRIES
        {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
        let mut previous = None;
        let mut binaries = BTreeSet::new();
        let mut launchers = BTreeSet::new();
        let mut web_assets = BTreeSet::new();
        for entry in &self.entries {
            entry.validate()?;
            if previous.is_some_and(|value| value >= entry.path.as_str()) {
                return Err(FilesystemUpdateError::InvalidArchive);
            }
            previous = Some(entry.path.as_str());
            if entry.kind == "binary" {
                binaries.insert(entry.path.clone());
            }
            if matches!(entry.kind.as_str(), "launcher" | "mcp_launcher") {
                launchers.insert((entry.path.clone(), entry.kind.clone()));
            }
            if entry.kind == "web_asset" {
                web_assets.insert(entry.path.clone());
            }
        }
        let expected = self
            .expected_binaries()
            .iter()
            .map(|binary| format!("bin/{}", platform_executable_name(binary)))
            .collect::<BTreeSet<_>>();
        let mut expected_launchers =
            BTreeSet::from([(self.launcher_binary.clone(), "launcher".to_owned())]);
        if let Some(path) = &self.mcp_launcher_binary {
            expected_launchers.insert((path.clone(), "mcp_launcher".to_owned()));
        }
        let web_assets_are_valid = if self.schema == PACKAGE_MANIFEST_SCHEMA_V3 {
            web_assets.contains(WEB_ASSET_MANIFEST)
                && web_assets.contains(WEB_ASSET_ENTRYPOINT)
                && web_assets
                    .iter()
                    .all(|path| path.starts_with(WEB_ASSET_PREFIX))
        } else {
            web_assets.is_empty()
        };
        if binaries != expected || launchers != expected_launchers || !web_assets_are_valid {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
        Ok(())
    }

    fn expected_binaries(&self) -> &'static [&'static str] {
        if self.schema == PACKAGE_MANIFEST_SCHEMA_V3 {
            &EXPECTED_BINARIES
        } else {
            &LEGACY_EXPECTED_BINARIES
        }
    }

    fn installed_paths(&self, version: &str) -> Option<Vec<String>> {
        let mut paths = self
            .entries
            .iter()
            .filter(|entry| {
                entry.path != self.launcher_binary
                    && self
                        .mcp_launcher_binary
                        .as_ref()
                        .is_none_or(|path| entry.path != *path)
            })
            .map(|entry| format!("versions/{version}/{}", entry.path))
            .collect::<Vec<_>>();
        paths.push(format!("versions/{version}/{PACKAGE_MANIFEST_NAME}"));
        paths.sort();
        (paths.len() <= MAX_OWNED_PATHS).then_some(paths)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageManifestEntry {
    path: String,
    kind: String,
    bytes: u64,
    sha256: String,
    unix_mode: u32,
}

impl PackageManifestEntry {
    fn validate(&self) -> Result<(), FilesystemUpdateError> {
        if !valid_relative_path(&self.path)
            || !matches!(
                self.kind.as_str(),
                "binary"
                    | "launcher"
                    | "mcp_launcher"
                    | "autostart_template"
                    | "license"
                    | "notice"
                    | "third_party_license"
                    | "web_asset"
            )
            || self.bytes == 0
            || self.bytes > MAX_ENTRY_BYTES
            || !lower_hex(&self.sha256, 64)
            || !matches!(self.unix_mode, 0o644 | 0o755)
            || matches!(self.kind.as_str(), "binary" | "launcher" | "mcp_launcher")
                != (self.unix_mode == 0o755)
        {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
        Ok(())
    }
}

fn inspect_package(
    file: File,
    verified: &VerifiedUpdate,
    target: &str,
) -> Result<PackageManifest, FilesystemUpdateError> {
    let mut archive = ZipArchive::new(file).map_err(FilesystemUpdateError::Zip)?;
    if archive.is_empty() || archive.len() > MAX_PACKAGE_ENTRIES.saturating_add(1) {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    let manifest_index = find_archive_entry(&mut archive, PACKAGE_MANIFEST_NAME)?
        .ok_or(FilesystemUpdateError::InvalidArchive)?;
    let manifest_bytes =
        read_zip_entry_bounded(&mut archive, manifest_index, MAX_PACKAGE_MANIFEST_BYTES)?;
    let manifest: PackageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| FilesystemUpdateError::InvalidArchive)?;
    manifest.validate(verified, target)?;
    validate_archive_inventory(&mut archive, &manifest)?;
    Ok(manifest)
}

fn inspect_bootstrap_package(
    file: File,
    target: &str,
) -> Result<PackageManifest, FilesystemUpdateError> {
    let mut archive = ZipArchive::new(file).map_err(FilesystemUpdateError::Zip)?;
    if archive.is_empty() || archive.len() > MAX_PACKAGE_ENTRIES.saturating_add(1) {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    let manifest_index = find_archive_entry(&mut archive, PACKAGE_MANIFEST_NAME)?
        .ok_or(FilesystemUpdateError::InvalidArchive)?;
    let manifest_bytes =
        read_zip_entry_bounded(&mut archive, manifest_index, MAX_PACKAGE_MANIFEST_BYTES)?;
    let manifest: PackageManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|_| FilesystemUpdateError::InvalidArchive)?;
    manifest.validate_identity(&manifest.version, target)?;
    validate_archive_inventory(&mut archive, &manifest)?;
    Ok(manifest)
}

fn install_launchers(
    file: File,
    manifest: &PackageManifest,
    current_bin: &PrivateDirectory<'_>,
    install_root: &Path,
) -> Result<(), FilesystemUpdateError> {
    let mut archive = ZipArchive::new(file).map_err(FilesystemUpdateError::Zip)?;
    let expected_binaries = manifest.expected_binaries();
    #[cfg(windows)]
    {
        let primary_name = platform_executable_name(expected_binaries[0]);
        write_launcher(
            &mut archive,
            manifest,
            current_bin,
            &primary_name,
            &manifest.launcher_binary,
            "launcher",
        )?;
        let primary = install_root.join(LAUNCHER_DIRECTORY).join(primary_name);
        for binary in expected_binaries
            .iter()
            .skip(1)
            .filter(|binary| manifest.mcp_launcher_binary.is_none() || **binary != "rootlight-mcp")
        {
            fs::hard_link(
                &primary,
                install_root
                    .join(LAUNCHER_DIRECTORY)
                    .join(platform_executable_name(binary)),
            )
            .map_err(FilesystemUpdateError::Io)?;
        }
        if let Some(mcp_launcher) = &manifest.mcp_launcher_binary {
            write_launcher(
                &mut archive,
                manifest,
                current_bin,
                &platform_executable_name("rootlight-mcp"),
                mcp_launcher,
                "mcp_launcher",
            )?;
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = install_root;
        for binary in expected_binaries {
            let (launcher, kind) = if binary == "rootlight-mcp" {
                manifest
                    .mcp_launcher_binary
                    .as_ref()
                    .map_or((&manifest.launcher_binary, "launcher"), |path| {
                        (path, "mcp_launcher")
                    })
            } else {
                (&manifest.launcher_binary, "launcher")
            };
            write_launcher(
                &mut archive,
                manifest,
                current_bin,
                &platform_executable_name(binary),
                launcher,
                kind,
            )?;
        }
        Ok(())
    }
}

fn write_launcher(
    archive: &mut ZipArchive<File>,
    manifest: &PackageManifest,
    current_bin: &PrivateDirectory<'_>,
    installed_name: &str,
    archive_path: &str,
    kind: &str,
) -> Result<(), FilesystemUpdateError> {
    let declared = manifest
        .entries
        .iter()
        .find(|entry| entry.path == archive_path && entry.kind == kind)
        .ok_or(FilesystemUpdateError::InvalidArchive)?;
    let index =
        find_archive_entry(archive, archive_path)?.ok_or(FilesystemUpdateError::InvalidArchive)?;
    let mut entry = archive
        .by_index(index)
        .map_err(FilesystemUpdateError::Zip)?;
    write_zip_file(
        current_bin,
        installed_name,
        &mut entry,
        declared.bytes,
        Some(&declared.sha256),
        0o755,
    )
}

fn copy_bootstrap_artifact(
    layout: &InstallLayout,
    mut source: File,
    expected_bytes: u64,
) -> Result<(File, String), FilesystemUpdateError> {
    layout.remove_state_file_if_exists(BOOTSTRAP_ARTIFACT_COPY_NAME)?;
    let mut copy = layout
        .state
        .create_file(OsStr::new(BOOTSTRAP_ARTIFACT_COPY_NAME))
        .map_err(FilesystemUpdateError::PrivateTree)?;
    let digest = copy_bounded_hash(&mut source, &mut copy, expected_bytes)?;
    copy.sync_all()
        .map_err(FilesystemUpdateError::PrivateTree)?;
    drop(copy);
    layout
        .state
        .sync_all()
        .map_err(FilesystemUpdateError::PrivateTree)?;
    Ok((layout.open_bootstrap_copy()?, digest))
}

fn map_publish_error(error: PublishError) -> FilesystemUpdateError {
    match error {
        PublishError::NotCommitted { source } => FilesystemUpdateError::PrivateTree(source),
        PublishError::CommittedButDurabilityUnknown { .. } => {
            FilesystemUpdateError::PublicationDurabilityUnknown
        }
        _ => FilesystemUpdateError::PublicationDurabilityUnknown,
    }
}

fn validate_archive_inventory(
    archive: &mut ZipArchive<File>,
    manifest: &PackageManifest,
) -> Result<(), FilesystemUpdateError> {
    let expected = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut observed = BTreeSet::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(FilesystemUpdateError::Zip)?;
        let name = entry.name();
        if !valid_relative_path(name)
            || !observed.insert(name.to_owned())
            || entry.is_dir()
            || entry.compression() != CompressionMethod::Stored
            || entry.compressed_size() != entry.size()
        {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
        if name == PACKAGE_MANIFEST_NAME {
            if entry.size() > MAX_PACKAGE_MANIFEST_BYTES {
                return Err(FilesystemUpdateError::InvalidArchive);
            }
            continue;
        }
        let declared = expected
            .get(name)
            .ok_or(FilesystemUpdateError::InvalidArchive)?;
        if entry.size() != declared.bytes
            || entry.unix_mode().is_some_and(|mode| {
                mode & 0o170000 == 0o120000 || mode & 0o777 != declared.unix_mode
            })
        {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
    }
    let expected_names = expected.keys().copied().collect::<BTreeSet<_>>();
    observed.remove(PACKAGE_MANIFEST_NAME);
    if observed.iter().map(String::as_str).collect::<BTreeSet<_>>() != expected_names {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    Ok(())
}

fn extract_package(
    file: File,
    manifest: &PackageManifest,
    staging: &PrivateDirectory<'_>,
) -> Result<(), FilesystemUpdateError> {
    let mut archive = ZipArchive::new(file).map_err(FilesystemUpdateError::Zip)?;
    let expected = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect::<BTreeMap<_, _>>();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(FilesystemUpdateError::Zip)?;
        let name = entry.name().to_owned();
        if name == manifest.launcher_binary
            || manifest
                .mcp_launcher_binary
                .as_ref()
                .is_some_and(|path| name == *path)
        {
            let declared = expected
                .get(name.as_str())
                .ok_or(FilesystemUpdateError::InvalidArchive)?;
            hash_zip_entry(&mut entry, declared)?;
            continue;
        }
        if name == PACKAGE_MANIFEST_NAME {
            let manifest_bytes = entry.size();
            if manifest_bytes > MAX_PACKAGE_MANIFEST_BYTES {
                return Err(FilesystemUpdateError::InvalidArchive);
            }
            write_zip_file(
                staging,
                PACKAGE_MANIFEST_NAME,
                &mut entry,
                manifest_bytes,
                None,
                0o644,
            )?;
            continue;
        }
        let declared = expected
            .get(name.as_str())
            .ok_or(FilesystemUpdateError::InvalidArchive)?;
        write_zip_file(
            staging,
            &name,
            &mut entry,
            declared.bytes,
            Some(&declared.sha256),
            declared.unix_mode,
        )?;
    }
    staging
        .sync_all()
        .map_err(FilesystemUpdateError::PrivateTree)
}

fn write_zip_file(
    staging: &PrivateDirectory<'_>,
    relative: &str,
    reader: &mut impl Read,
    expected_bytes: u64,
    expected_sha256: Option<&str>,
    unix_mode: u32,
) -> Result<(), FilesystemUpdateError> {
    let path = Path::new(relative);
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(name) => Ok(name.to_os_string()),
            _ => Err(FilesystemUpdateError::InvalidArchive),
        })
        .collect::<Result<Vec<_>, _>>()?;
    write_zip_file_components(
        staging,
        &components,
        reader,
        expected_bytes,
        expected_sha256,
        unix_mode,
    )
}

fn write_zip_file_components(
    parent: &PrivateDirectory<'_>,
    components: &[OsString],
    reader: &mut impl Read,
    expected_bytes: u64,
    expected_sha256: Option<&str>,
    unix_mode: u32,
) -> Result<(), FilesystemUpdateError> {
    let (first, remaining) = components
        .split_first()
        .ok_or(FilesystemUpdateError::InvalidArchive)?;
    if remaining.is_empty() {
        return write_zip_file_in_directory(
            parent,
            first,
            reader,
            expected_bytes,
            expected_sha256,
            unix_mode,
        );
    }
    let child = match parent.create_directory(first) {
        Ok(directory) => directory,
        Err(error) if error.is_already_exists() => {
            PrivateDirectory::open(parent.capability(), first)
                .map_err(FilesystemUpdateError::PrivateTree)?
        }
        Err(error) => return Err(FilesystemUpdateError::PrivateTree(error)),
    };
    write_zip_file_components(
        &child,
        remaining,
        reader,
        expected_bytes,
        expected_sha256,
        unix_mode,
    )
}

fn write_zip_file_in_directory(
    parent: &PrivateDirectory<'_>,
    file_name: &OsStr,
    reader: &mut impl Read,
    expected_bytes: u64,
    expected_sha256: Option<&str>,
    unix_mode: u32,
) -> Result<(), FilesystemUpdateError> {
    let mut output = parent
        .create_file(file_name)
        .map_err(FilesystemUpdateError::PrivateTree)?;
    let observed = copy_bounded_hash(reader, &mut output, expected_bytes)?;
    output
        .sync_all()
        .map_err(FilesystemUpdateError::PrivateTree)?;
    drop(output);
    if expected_sha256.is_some_and(|expected| expected != observed) {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    #[cfg(unix)]
    {
        use cap_std::fs::PermissionsExt as _;

        parent
            .capability()
            .set_permissions(file_name, cap_std::fs::Permissions::from_mode(unix_mode))
            .map_err(FilesystemUpdateError::Io)?;
    }
    #[cfg(not(unix))]
    let _ = unix_mode;
    parent
        .sync_all()
        .map_err(FilesystemUpdateError::PrivateTree)
}

fn hash_zip_entry(
    reader: &mut impl Read,
    declared: &PackageManifestEntry,
) -> Result<(), FilesystemUpdateError> {
    let observed = copy_bounded_hash(reader, &mut io::sink(), declared.bytes)?;
    if observed != declared.sha256 {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    Ok(())
}

fn copy_bounded_hash(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected_bytes: u64,
) -> Result<String, FilesystemUpdateError> {
    if expected_bytes > MAX_ENTRY_BYTES {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    let mut remaining = expected_bytes;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    while remaining > 0 {
        let wanted =
            usize::try_from(remaining.min(
                u64::try_from(buffer.len()).map_err(|_| FilesystemUpdateError::InputTooLarge)?,
            ))
            .map_err(|_| FilesystemUpdateError::InputTooLarge)?;
        let read = reader
            .read(&mut buffer[..wanted])
            .map_err(FilesystemUpdateError::Io)?;
        if read == 0 {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
        writer
            .write_all(&buffer[..read])
            .map_err(FilesystemUpdateError::Io)?;
        hasher.update(&buffer[..read]);
        remaining = remaining
            .checked_sub(u64::try_from(read).map_err(|_| FilesystemUpdateError::InputTooLarge)?)
            .ok_or(FilesystemUpdateError::InvalidArchive)?;
    }
    let mut trailing = [0_u8; 1];
    if reader
        .read(&mut trailing)
        .map_err(FilesystemUpdateError::Io)?
        != 0
    {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    Ok(hex_sha256(hasher.finalize().into()))
}

fn read_zip_entry_bounded(
    archive: &mut ZipArchive<File>,
    index: usize,
    maximum: u64,
) -> Result<Vec<u8>, FilesystemUpdateError> {
    let entry = archive
        .by_index(index)
        .map_err(FilesystemUpdateError::Zip)?;
    if entry.size() > maximum {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    let capacity =
        usize::try_from(entry.size()).map_err(|_| FilesystemUpdateError::InputTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    entry
        .take(
            maximum
                .checked_add(1)
                .ok_or(FilesystemUpdateError::InputTooLarge)?,
        )
        .read_to_end(&mut bytes)
        .map_err(FilesystemUpdateError::Io)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(FilesystemUpdateError::InvalidArchive);
    }
    Ok(bytes)
}

fn find_archive_entry(
    archive: &mut ZipArchive<File>,
    expected: &str,
) -> Result<Option<usize>, FilesystemUpdateError> {
    let mut found = None;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(FilesystemUpdateError::Zip)?;
        if entry.name() == expected && found.replace(index).is_some() {
            return Err(FilesystemUpdateError::InvalidArchive);
        }
    }
    Ok(found)
}

fn read_metadata_signature(path: &Path) -> Result<DetachedUpdateSignature, FilesystemUpdateError> {
    let value = read_signature_hex(path)?;
    DetachedUpdateSignature::from_hex(&value).map_err(FilesystemUpdateError::Verification)
}

fn read_artifact_signature(
    path: &Path,
) -> Result<DetachedArtifactSignature, FilesystemUpdateError> {
    let value = read_signature_hex(path)?;
    DetachedArtifactSignature::from_hex(&value).map_err(FilesystemUpdateError::Verification)
}

fn read_signature_hex(path: &Path) -> Result<String, FilesystemUpdateError> {
    let bytes = read_ambient_regular_bounded(path, 256)?;
    let value = std::str::from_utf8(&bytes).map_err(|_| FilesystemUpdateError::InvalidSignature)?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(FilesystemUpdateError::InvalidSignature);
    }
    Ok(value.to_owned())
}

fn read_ambient_regular_bounded(
    path: &Path,
    maximum: u64,
) -> Result<Vec<u8>, FilesystemUpdateError> {
    let file = open_ambient_regular_no_follow(path)?;
    let metadata = file.metadata().map_err(FilesystemUpdateError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        return Err(FilesystemUpdateError::InputTooLarge);
    }
    let capacity =
        usize::try_from(metadata.len()).map_err(|_| FilesystemUpdateError::InputTooLarge)?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(
        maximum
            .checked_add(1)
            .ok_or(FilesystemUpdateError::InputTooLarge)?,
    )
    .read_to_end(&mut bytes)
    .map_err(FilesystemUpdateError::Io)?;
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(FilesystemUpdateError::InputTooLarge);
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_ambient_regular_no_follow(path: &Path) -> Result<File, FilesystemUpdateError> {
    use rustix::fs::{Mode, OFlags};

    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|source| FilesystemUpdateError::Io(io::Error::from(source)))?;
    let file = File::from(descriptor);
    if !file
        .metadata()
        .map_err(FilesystemUpdateError::Io)?
        .file_type()
        .is_file()
    {
        return Err(FilesystemUpdateError::InvalidInput);
    }
    Ok(file)
}

#[cfg(windows)]
fn open_ambient_regular_no_follow(path: &Path) -> Result<File, FilesystemUpdateError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
    };

    let file = OpenOptions::new()
        .read(true)
        .access_mode(FILE_GENERIC_READ.0)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .map_err(FilesystemUpdateError::Io)?;
    let metadata = file.metadata().map_err(FilesystemUpdateError::Io)?;
    if !metadata.file_type().is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        return Err(FilesystemUpdateError::InvalidInput);
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_ambient_regular_no_follow(_path: &Path) -> Result<File, FilesystemUpdateError> {
    Err(FilesystemUpdateError::UnsupportedPlatform)
}

fn hash_file_exact(file: &File, maximum: u64) -> Result<String, FilesystemUpdateError> {
    let mut reader = file;
    let original_position = reader
        .stream_position()
        .map_err(FilesystemUpdateError::Io)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(FilesystemUpdateError::Io)?;
    let result = (|| {
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let mut buffer = [0_u8; HASH_BUFFER_BYTES];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(FilesystemUpdateError::Io)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).map_err(|_| FilesystemUpdateError::InputTooLarge)?)
                .ok_or(FilesystemUpdateError::InputTooLarge)?;
            if total > maximum {
                return Err(FilesystemUpdateError::InputTooLarge);
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex_sha256(hasher.finalize().into()))
    })();
    reader
        .seek(SeekFrom::Start(original_position))
        .map_err(FilesystemUpdateError::Io)?;
    result
}

fn prepare_private_install_root(path: &Path) -> Result<(), FilesystemUpdateError> {
    if !path.is_absolute() {
        return Err(FilesystemUpdateError::InvalidInstall);
    }
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            validate_directory_no_reparse(path)?;
            for owned in ["state", VERSIONS_DIRECTORY, "current"] {
                if path.join(owned).exists() {
                    return Err(FilesystemUpdateError::VersionAlreadyInstalled);
                }
            }
        }
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(FilesystemUpdateError::InstallParentMissing);
        }
        Err(source) => return Err(FilesystemUpdateError::Io(source)),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(FilesystemUpdateError::Io)?;
    }
    #[cfg(windows)]
    {
        let user_data = path.join("user");
        if user_data.exists() {
            validate_directory_no_reparse(&user_data)?;
            apply_private_windows_user_dacl(&user_data)?;
        }
        apply_private_windows_root_dacl(path)?;
    }
    let directory =
        Dir::open_ambient_dir(path, ambient_authority()).map_err(FilesystemUpdateError::Io)?;
    PrivateDirectory::verify_parent(&directory).map_err(FilesystemUpdateError::PrivateTree)
}

fn cleanup_failed_bootstrap(path: &Path) -> Result<(), FilesystemUpdateError> {
    let root =
        Dir::open_ambient_dir(path, ambient_authority()).map_err(FilesystemUpdateError::Io)?;
    PrivateDirectory::verify_parent(&root).map_err(FilesystemUpdateError::PrivateTree)?;
    for name in ["current", VERSIONS_DIRECTORY, "state"] {
        match root.symlink_metadata(name) {
            Ok(metadata) => {
                if !metadata.is_dir() || metadata.file_type().is_symlink() {
                    return Err(FilesystemUpdateError::RecoveryFailed);
                }
                PrivateDirectory::open(&root, OsStr::new(name))
                    .map_err(FilesystemUpdateError::PrivateTree)?
                    .remove()
                    .map_err(FilesystemUpdateError::PrivateTree)?;
            }
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(FilesystemUpdateError::Io(source)),
        }
    }
    Ok(())
}

fn remove_owned_current(
    root: &Dir,
    manifest: &InstallManifest,
) -> Result<(), FilesystemUpdateError> {
    let current = PrivateDirectory::open(root, OsStr::new("current"))
        .map_err(FilesystemUpdateError::PrivateTree)?;
    let bin = PrivateDirectory::open(current.capability(), OsStr::new("bin"))
        .map_err(FilesystemUpdateError::PrivateTree)?;
    let names = manifest
        .owned_paths
        .iter()
        .filter_map(|path| path.strip_prefix("current/bin/"))
        .collect::<BTreeSet<_>>();
    if names.is_empty() || names.len() > EXPECTED_BINARIES.len() {
        return Err(FilesystemUpdateError::InvalidInstall);
    }
    for name in &names {
        if name.contains('/') {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
        validate_cap_regular(bin.capability(), name)?;
    }
    for name in names {
        bin.capability()
            .remove_file(name)
            .map_err(FilesystemUpdateError::Io)?;
    }
    bin.sync_all().map_err(FilesystemUpdateError::PrivateTree)?;
    drop(bin);
    current
        .capability()
        .remove_dir("bin")
        .map_err(FilesystemUpdateError::Io)?;
    current
        .sync_all()
        .map_err(FilesystemUpdateError::PrivateTree)?;
    drop(current);
    root.remove_dir("current")
        .map_err(FilesystemUpdateError::Io)
}

#[cfg(windows)]
fn apply_private_windows_root_dacl(path: &Path) -> Result<(), FilesystemUpdateError> {
    apply_private_windows_dacl(path, false)
}

#[cfg(windows)]
fn apply_private_windows_user_dacl(path: &Path) -> Result<(), FilesystemUpdateError> {
    apply_private_windows_dacl(path, true)
}

#[cfg(windows)]
fn apply_private_windows_dacl(path: &Path, inheritable: bool) -> Result<(), FilesystemUpdateError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows_permissions::{
        LocalBox, SecurityDescriptor,
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetNamedSecurityInfo,
    };

    let token = OwnedToken::from_current_process(TOKEN_QUERY)
        .map_err(|_| FilesystemUpdateError::InsecureState)?;
    let sid = token
        .user()
        .and_then(|value| value.to_string())
        .map_err(|_| FilesystemUpdateError::InsecureState)?;
    let inheritance = if inheritable { "OICI" } else { "" };
    let descriptor_text = format!("D:P(A;{inheritance};FA;;;{sid})");
    let descriptor: LocalBox<SecurityDescriptor> = descriptor_text
        .parse()
        .map_err(|_| FilesystemUpdateError::InsecureState)?;
    let dacl = descriptor
        .dacl()
        .ok_or(FilesystemUpdateError::InsecureState)?;
    SetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(FilesystemUpdateError::Io)
}

fn validate_cap_regular(directory: &Dir, name: &str) -> Result<(), FilesystemUpdateError> {
    let metadata = directory
        .symlink_metadata(name)
        .map_err(FilesystemUpdateError::Io)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(FilesystemUpdateError::InsecureState);
    }
    Ok(())
}

fn validate_directory_no_reparse(path: &Path) -> Result<(), FilesystemUpdateError> {
    let metadata = fs::symlink_metadata(path).map_err(FilesystemUpdateError::Io)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(FilesystemUpdateError::InvalidInstall);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
    }
    Ok(())
}

fn validate_regular_payload(path: &Path) -> Result<(), FilesystemUpdateError> {
    let metadata = fs::symlink_metadata(path).map_err(FilesystemUpdateError::Io)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 {
        return Err(FilesystemUpdateError::InvalidInstall);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(FilesystemUpdateError::InvalidInstall);
        }
    }
    Ok(())
}

fn serialize_bounded(
    value: &impl Serialize,
    maximum: u64,
) -> Result<Vec<u8>, FilesystemUpdateError> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(FilesystemUpdateError::SerializeState)?;
    bytes.push(b'\n');
    if u64::try_from(bytes.len()).map_or(true, |length| length > maximum) {
        return Err(FilesystemUpdateError::StateTooLarge);
    }
    Ok(bytes)
}

fn parse_active_version(bytes: &[u8]) -> Result<String, FilesystemUpdateError> {
    let value = std::str::from_utf8(bytes).map_err(|_| FilesystemUpdateError::InvalidInstall)?;
    let version = value
        .strip_suffix('\n')
        .ok_or(FilesystemUpdateError::InvalidInstall)?;
    if version.contains(['\r', '\n']) {
        return Err(FilesystemUpdateError::InvalidInstall);
    }
    canonical_version(version)?;
    Ok(version.to_owned())
}

fn staging_name(version: &str, metadata_sha256: &str) -> Result<String, FilesystemUpdateError> {
    canonical_version(version)?;
    let prefix = metadata_sha256
        .get(..16)
        .filter(|value| lower_hex(value, 16))
        .ok_or(FilesystemUpdateError::InvalidTransaction)?;
    let name = format!(".update-{version}-{prefix}");
    if !valid_staging_name(&name) {
        return Err(FilesystemUpdateError::InvalidTransaction);
    }
    Ok(name)
}

fn valid_staging_name(value: &str) -> bool {
    value.starts_with(".update-")
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_relative_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PATH_BYTES
        && !value.contains('\\')
        && !value.contains(':')
        && Path::new(value)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LABEL_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn canonical_version(value: &str) -> Result<Version, FilesystemUpdateError> {
    let version = Version::parse(value).map_err(|_| FilesystemUpdateError::InvalidVersion)?;
    if value.len() > 128 || version.to_string() != value {
        return Err(FilesystemUpdateError::InvalidVersion);
    }
    Ok(version)
}

fn lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

#[cfg(windows)]
fn deferred_uninstall_token() -> Result<Option<String>, FilesystemUpdateError> {
    let Some(value) = std::env::var_os(DEFERRED_UNINSTALL_TOKEN_ENV) else {
        return Ok(None);
    };
    let token = value
        .into_string()
        .map_err(|_| FilesystemUpdateError::InvalidInstall)?;
    if !lower_hex(&token, 32) {
        return Err(FilesystemUpdateError::InvalidInstall);
    }
    Ok(Some(token))
}

#[cfg(windows)]
fn current_executable_is_owned_payload(
    install_root: &Path,
    manifest: &InstallManifest,
    versions: &BTreeSet<String>,
) -> Result<bool, FilesystemUpdateError> {
    let root = fs::canonicalize(install_root).map_err(FilesystemUpdateError::Io)?;
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(FilesystemUpdateError::Io)?;
    validate_regular_payload(&executable)?;
    let relative = executable
        .strip_prefix(&root)
        .map_err(|_| FilesystemUpdateError::InvalidInstall)?;
    let mut components = relative.components();
    if components.next() != Some(Component::Normal(OsStr::new(VERSIONS_DIRECTORY))) {
        return Ok(false);
    }
    let Some(Component::Normal(version)) = components.next() else {
        return Ok(false);
    };
    let Some(version) = version.to_str() else {
        return Ok(false);
    };
    Ok(versions.contains(version)
        && manifest.owns_version(version)
        && components.next() == Some(Component::Normal(OsStr::new("bin")))
        && components.next()
            == Some(Component::Normal(OsStr::new(
                platform_executable_name("rootlight").as_str(),
            )))
        && components.next().is_none())
}

fn hex_sha256(value: [u8; 32]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(64);
    for byte in value {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn encode_hex<const N: usize>(value: &[u8; N]) -> String {
    use std::fmt::Write as _;

    let mut output = String::with_capacity(N.saturating_mul(2));
    for byte in value {
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn platform_executable_name(binary: &str) -> String {
    if cfg!(windows) {
        format!("{binary}.exe")
    } else {
        binary.to_owned()
    }
}

fn current_platform() -> Result<&'static str, FilesystemUpdateError> {
    #[cfg(target_os = "windows")]
    return Ok("windows");
    #[cfg(target_os = "linux")]
    return Ok("linux");
    #[cfg(target_os = "macos")]
    return Ok("macos");
    #[allow(unreachable_code)]
    Err(FilesystemUpdateError::UnsupportedPlatform)
}

fn current_architecture() -> Result<&'static str, FilesystemUpdateError> {
    #[cfg(target_arch = "x86_64")]
    return Ok("x86_64");
    #[cfg(target_arch = "aarch64")]
    return Ok("aarch64");
    #[allow(unreachable_code)]
    Err(FilesystemUpdateError::UnsupportedPlatform)
}

fn current_target() -> Result<&'static str, FilesystemUpdateError> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Ok("x86_64-pc-windows-msvc");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Ok("x86_64-unknown-linux-gnu");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Ok("aarch64-unknown-linux-gnu");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Ok("x86_64-apple-darwin");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Ok("aarch64-apple-darwin");
    #[allow(unreachable_code)]
    Err(FilesystemUpdateError::UnsupportedPlatform)
}

/// Production filesystem update failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FilesystemUpdateError {
    /// Installation layout or ownership state is invalid.
    #[error("update installation is invalid")]
    InvalidInstall,
    /// The installation root cannot be created until its parent exists.
    #[error("update installation parent is missing")]
    InstallParentMissing,
    /// Active selector and ownership state disagree.
    #[error("update installation state is inconsistent")]
    InconsistentState,
    /// A caller input is not a regular no-follow file.
    #[error("update input is invalid")]
    InvalidInput,
    /// A caller input exceeds its hard byte limit.
    #[error("update input exceeds its byte limit")]
    InputTooLarge,
    /// Detached signature file encoding is invalid.
    #[error("update signature input is invalid")]
    InvalidSignature,
    /// Installed trust policy is malformed or inconsistent.
    #[error("update trust policy is invalid")]
    InvalidPolicy,
    /// The candidate or retained version is not canonical SemVer.
    #[error("update version is invalid")]
    InvalidVersion,
    /// Package archive or exact entry inventory is invalid.
    #[error("update package archive is invalid")]
    InvalidArchive,
    /// Durable transaction state is malformed.
    #[error("update transaction is invalid")]
    InvalidTransaction,
    /// An existing state object violates the private regular-file policy.
    #[error("update state object is insecure")]
    InsecureState,
    /// Another process owns the update lock.
    #[error("another update operation is active")]
    Busy,
    /// The candidate version already exists and is never overwritten.
    #[error("candidate update version is already installed")]
    VersionAlreadyInstalled,
    /// The copied archive differs from the signed artifact.
    #[error("private update artifact copy does not match")]
    ArtifactCopyMismatch,
    /// Registered platform resources require their owning platform uninstaller.
    #[error("registered platform resources remain")]
    PlatformResourcesRemain,
    /// Candidate health did not pass inside the signed deadline.
    #[error("candidate update health check failed")]
    Health(#[source] CandidateHealthError),
    /// Required rollback or cleanup could not restore known-good state.
    #[error("update recovery failed")]
    RecoveryFailed,
    /// Directory publication committed but durability could not be confirmed.
    #[error("update publication durability is unknown")]
    PublicationDurabilityUnknown,
    /// State serialization exceeds its hard bound.
    #[error("update state exceeds its byte limit")]
    StateTooLarge,
    /// The platform has no enabled production update implementation.
    #[error("update platform is unsupported")]
    UnsupportedPlatform,
    /// The trusted local clock is before the Unix epoch.
    #[error("trusted update clock is invalid")]
    Clock,
    /// File-system free space could not be observed.
    #[error("update disk space could not be measured")]
    DiskSpace(#[source] io::Error),
    /// Signed metadata or artifact verification failed.
    #[error("signed update verification failed")]
    Verification(#[source] UpdateError),
    /// Private capability-tree validation or mutation failed.
    #[error("private update filesystem operation failed")]
    PrivateTree(#[source] PlatformError),
    /// ZIP parsing failed.
    #[error("update ZIP parsing failed")]
    Zip(#[source] zip::result::ZipError),
    /// State serialization failed.
    #[error("update state serialization failed")]
    SerializeState(#[source] serde_json::Error),
    /// A filesystem operation failed.
    #[error("update filesystem operation failed")]
    Io(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ed25519_compact::{KeyPair, Seed};
    use zip::{ZipWriter, write::SimpleFileOptions};

    use super::*;

    fn package_bytes(version: &str) -> Vec<u8> {
        package_bytes_with_revision(version, &"a".repeat(40))
    }

    fn package_bytes_with_revision(version: &str, source_revision: &str) -> Vec<u8> {
        package_bytes_with_schema(version, source_revision, TestPackageSchema::V3)
    }

    #[derive(Clone, Copy)]
    enum TestPackageSchema {
        V1,
        V2,
        V3,
    }

    fn package_bytes_with_schema(
        version: &str,
        source_revision: &str,
        schema: TestPackageSchema,
    ) -> Vec<u8> {
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let binaries: &[&str] = if matches!(schema, TestPackageSchema::V3) {
            &EXPECTED_BINARIES
        } else {
            &LEGACY_EXPECTED_BINARIES
        };
        let mut entries = binaries
            .iter()
            .map(|binary| {
                (
                    format!("bin/{binary}{suffix}"),
                    "binary",
                    0o755,
                    format!("{binary}-{version}").into_bytes(),
                )
            })
            .collect::<Vec<_>>();
        entries.push((
            format!("launcher/rootlight-launcher{suffix}"),
            "launcher",
            0o755,
            b"launcher".to_vec(),
        ));
        if !matches!(schema, TestPackageSchema::V1) {
            entries.push((
                format!("launcher/rootlight-mcp-launcher{suffix}"),
                "mcp_launcher",
                0o755,
                b"mcp-launcher".to_vec(),
            ));
        }
        if matches!(schema, TestPackageSchema::V3) {
            entries.extend([
                (
                    WEB_ASSET_MANIFEST.to_owned(),
                    "web_asset",
                    0o644,
                    br#"{"schema_version":1,"assets":[]}"#.to_vec(),
                ),
                (
                    WEB_ASSET_ENTRYPOINT.to_owned(),
                    "web_asset",
                    0o644,
                    b"<!doctype html>".to_vec(),
                ),
            ]);
        }
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        let manifest_entries = entries
            .iter()
            .map(|(path, kind, mode, bytes)| {
                serde_json::json!({
                    "path": path,
                    "kind": kind,
                    "bytes": bytes.len(),
                    "sha256": hex_sha256(Sha256::digest(bytes).into()),
                    "unix_mode": mode
                })
            })
            .collect::<Vec<_>>();
        let mut manifest = serde_json::json!({
            "schema": match schema {
                TestPackageSchema::V1 => PACKAGE_MANIFEST_SCHEMA_V1,
                TestPackageSchema::V2 => PACKAGE_MANIFEST_SCHEMA_V2,
                TestPackageSchema::V3 => PACKAGE_MANIFEST_SCHEMA_V3,
            },
            "target": current_target().expect("test platform is supported"),
            "version": version,
            "source_revision": source_revision,
            "autostart_default": "disabled",
            "autostart_kind": if cfg!(windows) {
                "windows_scheduled_task"
            } else if cfg!(target_os = "macos") {
                "launchd_user_agent"
            } else {
                "systemd_user_unit"
            },
            "autostart_resource": if cfg!(windows) {
                "RootlightDaemon"
            } else if cfg!(target_os = "macos") {
                "com.rootlight.daemon"
            } else {
                "rootlight-daemon.service"
            },
            "user_data_policy": "preserve",
            "ownership_manifest": "state/install-manifest.json",
            "active_version_file": "state/active-version",
            "launcher_binary": format!("launcher/rootlight-launcher{suffix}"),
            "versions_directory": "versions",
            "launcher_directory": "current/bin",
            "update_lock_file": "state/update.lock",
            "update_transaction_file": "state/update-transaction.json",
            "retained_versions": 2,
            "entries": manifest_entries
        });
        if !matches!(schema, TestPackageSchema::V1) {
            manifest
                .as_object_mut()
                .expect("package manifest is an object")
                .insert(
                    "mcp_launcher_binary".to_owned(),
                    serde_json::json!(format!("launcher/rootlight-mcp-launcher{suffix}")),
                );
        }
        let output = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(output);
        for (path, _kind, mode, bytes) in entries {
            writer
                .start_file(
                    path,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Stored)
                        .unix_permissions(mode),
                )
                .expect("package entry starts");
            writer.write_all(&bytes).expect("package entry writes");
        }
        writer
            .start_file(
                PACKAGE_MANIFEST_NAME,
                SimpleFileOptions::default()
                    .compression_method(CompressionMethod::Stored)
                    .unix_permissions(0o644),
            )
            .expect("manifest entry starts");
        writer
            .write_all(&serde_json::to_vec(&manifest).expect("manifest serializes"))
            .expect("manifest writes");
        writer.finish().expect("package finishes").into_inner()
    }

    fn policy(public_key: UpdatePublicKey) -> TrustedUpdatePolicy {
        TrustedUpdatePolicy::new(
            true,
            "rootlight-test-key".to_owned(),
            public_key,
            "stable".to_owned(),
            1,
            1,
            7,
            0,
        )
        .expect("test policy validates")
    }

    fn test_key_pair() -> KeyPair {
        KeyPair::from_seed(Seed::new([7_u8; 32]))
    }

    fn write_signed_update_inputs(
        directory: &Path,
        version: &str,
        artifact: &[u8],
        key_pair: &KeyPair,
    ) -> UpdateInputPaths {
        use data_encoding::BASE64;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("test clock is valid")
            .as_secs();
        let artifact_sha256 = hex_sha256(Sha256::digest(artifact).into());
        let source_revision = "a".repeat(40);
        let target = current_target().expect("test platform is supported");
        let sbom = serde_json::to_vec(&serde_json::json!({
            "bomFormat": "CycloneDX",
            "components": [],
            "dependencies": [],
            "metadata": {
                "component": {
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
                        {"name": "rootlight:source:revision", "value": source_revision},
                        {"name": "rootlight:target:triple", "value": target}
                    ],
                    "type": "application",
                    "version": version
                },
                "properties": [
                    {"name": "cdx:rustc:sbom:target:triple", "value": target},
                    {"name": "rootlight:build:profile", "value": "release"},
                    {"name": "rootlight:source:revision", "value": source_revision}
                ]
            },
            "specVersion": "1.5",
            "version": 1
        }))
        .expect("SBOM serializes");
        let provenance_statement = serde_json::to_vec(&serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{
                "name": "update.zip",
                "digest": {"sha256": artifact_sha256}
            }],
            "predicateType": "https://slsa.dev/provenance/v1",
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
        let provenance = serde_json::to_vec(&serde_json::json!({
            "dsseEnvelope": {
                "payload": BASE64.encode(&provenance_statement),
                "payloadType": "application/vnd.in-toto+json",
                "signatures": [{"keyid": "rootlight-test-key", "sig": "local"}]
            },
            "verificationMaterial": {
                "tlogEntries": [{"logIndex": "local-test"}]
            }
        }))
        .expect("provenance envelope serializes");
        let license_bundle = test_license_bundle();
        let metadata_value = serde_json::json!({
            "schema_version": super::super::UPDATE_METADATA_SCHEMA_VERSION,
            "key_id": "rootlight-test-key",
            "version": version,
            "channel": "stable",
            "platform": current_platform().expect("test platform is supported"),
            "architecture": current_architecture().expect("test architecture is supported"),
            "valid_from_unix_seconds": now.saturating_sub(60),
            "expires_unix_seconds": now.saturating_add(3600),
            "rollout_percentage": 100,
            "artifact": {
                "file_name": "update.zip",
                "sha256": artifact_sha256,
                "size_bytes": artifact.len(),
                "sbom_sha256": hex_sha256(Sha256::digest(&sbom).into()),
                "provenance_sha256": hex_sha256(Sha256::digest(&provenance).into()),
                "license_bundle_sha256": hex_sha256(Sha256::digest(&license_bundle).into()),
                "reproducibility": "provenance_only"
            },
            "compatibility": {
                "minimum_catalog_schema": 1,
                "maximum_catalog_schema": 1,
                "protocol_major": 1,
                "minimum_protocol_minor": 7,
                "maximum_protocol_minor": 7,
                "migration_required_bytes": 0,
                "rollback_supported": true,
                "health_timeout_seconds": 30
            }
        });
        let metadata_contract: super::super::UpdateMetadata =
            serde_json::from_value(metadata_value).expect("metadata contract parses");
        let metadata = super::super::canonical_update_metadata_bytes(&metadata_contract)
            .expect("metadata serializes canonically");
        let metadata_signature = DetachedUpdateSignature(*key_pair.sk.sign(&metadata, None));
        let artifact_message =
            super::super::canonical_artifact_signature_message(&metadata_contract)
                .expect("artifact signature message is canonical");
        let artifact_signature =
            DetachedArtifactSignature(*key_pair.sk.sign(&artifact_message, None));
        let metadata_path = directory.join("update.json");
        let metadata_signature_path = directory.join("update.sig");
        let artifact_signature_path = directory.join("artifact.sig");
        let artifact_path = directory.join("update.zip");
        let sbom_path = directory.join("update.sbom.json");
        let provenance_path = directory.join("update.provenance.json");
        let license_bundle_path = directory.join("update.licenses.zip");
        fs::write(&metadata_path, metadata).expect("metadata writes");
        fs::write(
            &metadata_signature_path,
            encode_hex(metadata_signature.as_bytes()),
        )
        .expect("metadata signature writes");
        fs::write(
            &artifact_signature_path,
            encode_hex(artifact_signature.as_bytes()),
        )
        .expect("artifact signature writes");
        fs::write(&artifact_path, artifact).expect("artifact writes");
        fs::write(&sbom_path, sbom).expect("SBOM writes");
        fs::write(&provenance_path, provenance).expect("provenance writes");
        fs::write(&license_bundle_path, license_bundle).expect("license bundle writes");
        UpdateInputPaths::new(
            metadata_path,
            metadata_signature_path,
            artifact_signature_path,
            artifact_path,
            sbom_path,
            provenance_path,
            license_bundle_path,
        )
    }

    fn test_license_bundle() -> Vec<u8> {
        let output = Cursor::new(Vec::new());
        let mut writer = ZipWriter::new(output);
        for name in [
            "LICENSE",
            "NOTICE",
            "licenses/tree-sitter-cpp-LICENSE",
            "licenses/tree-sitter-java-LICENSE",
            "licenses/tree-sitter-kotlin-LICENSE",
            "licenses/tree-sitter-typescript-LICENSE",
        ] {
            writer
                .start_file(name, SimpleFileOptions::default().unix_permissions(0o644))
                .expect("license entry starts");
            writer
                .write_all(b"test license\n")
                .expect("license entry writes");
        }
        writer
            .finish()
            .expect("license bundle finishes")
            .into_inner()
    }

    struct PassingHealth;

    impl CandidateHealthCheck for PassingHealth {
        fn check(
            &mut self,
            _candidate_version_root: &Path,
            _catalog_state_root: &Path,
            _timeout: Duration,
        ) -> Result<(), CandidateHealthError> {
            Ok(())
        }
    }

    struct FailingHealth;

    impl CandidateHealthCheck for FailingHealth {
        fn check(
            &mut self,
            _candidate_version_root: &Path,
            _catalog_state_root: &Path,
            _timeout: Duration,
        ) -> Result<(), CandidateHealthError> {
            Err(CandidateHealthError)
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn candidate_health_state_uses_a_canonical_temp_root() {
        let probe = candidate_health_tempdir().expect("candidate probe directory is available");

        assert!(probe.path().starts_with("/private/tmp"));
        assert_eq!(
            fs::canonicalize(probe.path()).expect("candidate probe path canonicalizes"),
            probe.path()
        );
    }

    #[test]
    fn candidate_state_clone_is_isolated_and_complete() {
        let source = tempfile::tempdir().expect("source state exists");
        fs::create_dir(source.path().join("first-slice")).expect("nested state directory creates");
        let source_file = source.path().join("first-slice/catalog.sqlite");
        fs::write(&source_file, b"catalog-v1").expect("source catalog writes");
        let destination_root = tempfile::tempdir().expect("destination parent exists");
        let destination = destination_root.path().join("state");

        clone_candidate_state(source.path(), &destination).expect("candidate state clones");
        let cloned_file = destination.join("first-slice/catalog.sqlite");
        assert_eq!(
            fs::read(&cloned_file).expect("cloned catalog reads"),
            b"catalog-v1"
        );
        fs::write(&cloned_file, b"candidate-mutation").expect("candidate clone mutates");
        assert_eq!(
            fs::read(&source_file).expect("source catalog remains readable"),
            b"catalog-v1"
        );
    }

    #[derive(Debug, Clone, Copy)]
    enum RecoveryCheckpoint {
        Staged,
        HealthChecking,
        CommitPrepared,
        ManifestCommitted,
        SelectorCommitted,
        Committed,
        RollbackPrepared,
    }

    fn recovery_checkpoint_fixture(checkpoint: RecoveryCheckpoint) -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let archive = temporary.path().join("baseline.zip");
        let baseline = package_bytes("1.0.0");
        fs::write(&archive, &baseline).expect("baseline package writes");
        let key_pair = test_key_pair();
        install_package_with_policy(
            &root,
            &archive,
            &hex_sha256(Sha256::digest(&baseline).into()),
            &policy(UpdatePublicKey(*key_pair.pk)),
        )
        .expect("baseline package installs");

        let layout = InstallLayout::open(&root).expect("installed layout opens");
        let candidate_version = "2.0.0";
        let candidate_owned_paths = vec![format!(
            "versions/{candidate_version}/bin/rootlight{}",
            std::env::consts::EXE_SUFFIX
        )];
        let transaction = UpdateTransaction {
            schema: UPDATE_TRANSACTION_SCHEMA.to_owned(),
            phase: match checkpoint {
                RecoveryCheckpoint::Staged => UpdateTransactionPhase::Staged,
                RecoveryCheckpoint::HealthChecking => UpdateTransactionPhase::HealthChecking,
                RecoveryCheckpoint::CommitPrepared
                | RecoveryCheckpoint::ManifestCommitted
                | RecoveryCheckpoint::SelectorCommitted => UpdateTransactionPhase::CommitPrepared,
                RecoveryCheckpoint::Committed => UpdateTransactionPhase::Committed,
                RecoveryCheckpoint::RollbackPrepared => UpdateTransactionPhase::RollbackPrepared,
            },
            previous_version: "1.0.0".to_owned(),
            candidate_version: candidate_version.to_owned(),
            target: current_target()
                .expect("test target is supported")
                .to_owned(),
            staging_name: staging_name(candidate_version, &"b".repeat(64))
                .expect("staging name derives"),
            metadata_sha256: "b".repeat(64),
            artifact_sha256: "c".repeat(64),
            candidate_owned_paths: candidate_owned_paths.clone(),
        };

        if matches!(checkpoint, RecoveryCheckpoint::Staged) {
            let _staging = PrivateDirectory::create(
                layout.versions.capability(),
                OsStr::new(&transaction.staging_name),
            )
            .expect("staging directory creates");
        } else {
            let _candidate = PrivateDirectory::create(
                layout.versions.capability(),
                OsStr::new(candidate_version),
            )
            .expect("candidate directory creates");
        }
        layout
            .write_transaction(&transaction)
            .expect("durable transaction writes");

        if matches!(
            checkpoint,
            RecoveryCheckpoint::ManifestCommitted
                | RecoveryCheckpoint::SelectorCommitted
                | RecoveryCheckpoint::Committed
        ) {
            let mut install = layout
                .read_install_manifest()
                .expect("install manifest reads");
            install.owned_paths.extend(candidate_owned_paths);
            install.owned_paths.sort();
            install.owned_paths.dedup();
            install.active_version = candidate_version.to_owned();
            install.last_good_version = Some("1.0.0".to_owned());
            install.schema = INSTALL_MANIFEST_SCHEMA_V2.to_owned();
            layout
                .write_install_manifest(&install)
                .expect("candidate manifest writes");
        }
        if matches!(
            checkpoint,
            RecoveryCheckpoint::SelectorCommitted | RecoveryCheckpoint::Committed
        ) {
            layout
                .write_active_version(candidate_version)
                .expect("candidate selector writes");
        }
        let mut artifact_copy = layout
            .state
            .create_file(OsStr::new(ARTIFACT_COPY_NAME))
            .expect("owned artifact copy creates");
        artifact_copy
            .write_all(b"artifact")
            .expect("owned artifact copy writes");
        artifact_copy
            .sync_all()
            .expect("owned artifact copy synchronizes");
        (temporary, root)
    }

    #[test]
    fn bootstrap_and_owned_uninstall_preserve_user_data() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        fs::create_dir_all(root.join("user")).expect("user directory creates");
        fs::write(root.join("user/sentinel"), b"user").expect("user sentinel writes");
        let unowned = temporary.path().join("unowned");
        fs::write(&unowned, b"unowned").expect("unowned sentinel writes");
        let archive = temporary.path().join("package.zip");
        let bytes = package_bytes("1.0.0");
        fs::write(&archive, &bytes).expect("package writes");
        let digest = hex_sha256(Sha256::digest(&bytes).into());

        let key_pair = test_key_pair();
        let installed = install_package_with_policy(
            &root,
            &archive,
            &digest,
            &policy(UpdatePublicKey(*key_pair.pk)),
        )
        .expect("package installs");
        let status = update_runtime_status(&root).expect("installed status reads");

        assert_eq!(installed.version, "1.0.0");
        assert_eq!(status.active_version, "1.0.0");
        assert_eq!(status.package_version, "1.0.0");
        assert_eq!(status.binary_version, env!("CARGO_PKG_VERSION"));
        assert!(!status.package_matches_binary);
        assert!(!status.recovery_required);
        let semantic_host = platform_executable_name("rootlight-semantic-host");
        let mcp = platform_executable_name("rootlight-mcp");
        assert!(root.join("current/bin").join(&semantic_host).is_file());
        assert_eq!(
            fs::read(root.join("current/bin").join(&mcp)).expect("MCP launcher reads"),
            b"mcp-launcher"
        );
        assert!(
            root.join("versions/1.0.0/bin")
                .join(&semantic_host)
                .is_file()
        );
        #[cfg(windows)]
        {
            let rootlight = root
                .join("current/bin")
                .join(platform_executable_name("rootlight"));
            fs::write(&rootlight, b"linked-launcher").expect("primary launcher link writes");
            assert_eq!(
                fs::read(root.join("current/bin").join(&semantic_host))
                    .expect("secondary launcher link reads"),
                b"linked-launcher"
            );
            assert_eq!(
                fs::read(root.join("current/bin").join(&mcp))
                    .expect("dedicated MCP launcher remains independent"),
                b"mcp-launcher"
            );
        }
        let removed = uninstall_package(&root).expect("package uninstalls");
        assert!(removed.user_data_preserved);
        assert_eq!(
            fs::read(root.join("user/sentinel")).expect("user sentinel remains"),
            b"user"
        );
        assert_eq!(
            fs::read(unowned).expect("unowned sentinel remains"),
            b"unowned"
        );
        assert!(!root.join("current").exists());
        assert!(!root.join("versions").exists());
        assert!(!root.join("state").exists());
    }

    #[test]
    fn bootstrap_accepts_legacy_package_without_dedicated_mcp_launcher() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let archive = temporary.path().join("package.zip");
        let bytes = package_bytes_with_schema("1.0.0", &"a".repeat(40), TestPackageSchema::V1);
        fs::write(&archive, &bytes).expect("package writes");
        let digest = hex_sha256(Sha256::digest(&bytes).into());
        let key_pair = test_key_pair();

        install_package_with_policy(
            &root,
            &archive,
            &digest,
            &policy(UpdatePublicKey(*key_pair.pk)),
        )
        .expect("legacy package installs");

        assert_eq!(
            fs::read(
                root.join("current/bin")
                    .join(platform_executable_name("rootlight-mcp"))
            )
            .expect("legacy MCP launcher reads"),
            b"launcher"
        );
    }

    #[test]
    fn bootstrap_accepts_previous_package_schema_with_dedicated_mcp_launcher() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let archive = temporary.path().join("package.zip");
        let bytes = package_bytes_with_schema("1.0.0", &"a".repeat(40), TestPackageSchema::V2);
        fs::write(&archive, &bytes).expect("package writes");
        let digest = hex_sha256(Sha256::digest(&bytes).into());
        let key_pair = test_key_pair();

        install_package_with_policy(
            &root,
            &archive,
            &digest,
            &policy(UpdatePublicKey(*key_pair.pk)),
        )
        .expect("previous package schema installs");

        assert_eq!(
            fs::read(
                root.join("current/bin")
                    .join(platform_executable_name("rootlight-mcp"))
            )
            .expect("dedicated MCP launcher reads"),
            b"mcp-launcher"
        );
        assert!(
            !root
                .join("versions/1.0.0/bin")
                .join(platform_executable_name("rootlight-web"))
                .exists()
        );
    }

    #[test]
    fn bootstrap_reports_a_missing_install_parent_without_creating_ancestors() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let missing_parent = temporary.path().join("missing");
        let root = missing_parent.join("install");

        assert!(matches!(
            prepare_private_install_root(&root),
            Err(FilesystemUpdateError::InstallParentMissing)
        ));
        assert!(!missing_parent.exists());
    }

    #[test]
    fn bootstrap_accepts_a_sha256_source_revision() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let archive = temporary.path().join("package.zip");
        let bytes = package_bytes_with_revision("1.0.0", &"a".repeat(64));
        fs::write(&archive, &bytes).expect("package writes");
        let digest = hex_sha256(Sha256::digest(&bytes).into());
        let key_pair = test_key_pair();

        let installed = install_package_with_policy(
            &root,
            &archive,
            &digest,
            &policy(UpdatePublicKey(*key_pair.pk)),
        )
        .expect("package with SHA-256 source revision installs");

        assert_eq!(installed.version, "1.0.0");
    }

    #[test]
    fn signed_update_commits_and_retains_the_previous_version() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let key_pair = test_key_pair();
        let trusted_policy = policy(UpdatePublicKey(*key_pair.pk));
        let initial_archive = temporary.path().join("initial.zip");
        let initial_bytes = package_bytes("1.0.0");
        fs::write(&initial_archive, &initial_bytes).expect("initial package writes");
        install_package_with_policy(
            &root,
            &initial_archive,
            &hex_sha256(Sha256::digest(&initial_bytes).into()),
            &trusted_policy,
        )
        .expect("initial package installs");
        let candidate = package_bytes("2.0.0");
        let inputs = write_signed_update_inputs(temporary.path(), "2.0.0", &candidate, &key_pair);

        let outcome = apply_update_package(
            &root,
            &root.join("catalog-state"),
            &inputs,
            &mut PassingHealth,
        )
        .expect("update applies");
        let status = update_runtime_status(&root).expect("updated status reads");

        assert_eq!(outcome.version, "2.0.0");
        assert_eq!(outcome.previous_version, "1.0.0");
        assert_eq!(status.active_version, "2.0.0");
        assert_eq!(status.last_good_version, "1.0.0");
        assert!(!status.recovery_required);
        assert!(root.join("versions/1.0.0").is_dir());
        assert!(root.join("versions/2.0.0").is_dir());
    }

    #[test]
    fn failed_health_rolls_back_and_removes_candidate_state() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        let key_pair = test_key_pair();
        let trusted_policy = policy(UpdatePublicKey(*key_pair.pk));
        let initial_archive = temporary.path().join("initial.zip");
        let initial_bytes = package_bytes("1.0.0");
        fs::write(&initial_archive, &initial_bytes).expect("initial package writes");
        install_package_with_policy(
            &root,
            &initial_archive,
            &hex_sha256(Sha256::digest(&initial_bytes).into()),
            &trusted_policy,
        )
        .expect("initial package installs");
        let candidate = package_bytes("2.0.0");
        let inputs = write_signed_update_inputs(temporary.path(), "2.0.0", &candidate, &key_pair);

        assert!(matches!(
            apply_update_package(
                &root,
                &root.join("catalog-state"),
                &inputs,
                &mut FailingHealth,
            ),
            Err(FilesystemUpdateError::Health(_))
        ));
        let status = update_runtime_status(&root).expect("rolled-back status reads");

        assert_eq!(status.active_version, "1.0.0");
        assert_eq!(status.last_good_version, "1.0.0");
        assert!(!status.recovery_required);
        assert!(!root.join("versions/2.0.0").exists());
        assert!(!root.join("state/update-transaction.json").exists());
        assert!(!root.join("state/update-artifact.zip").exists());
    }

    #[test]
    fn recovery_handles_every_durable_update_checkpoint() {
        for checkpoint in [
            RecoveryCheckpoint::Staged,
            RecoveryCheckpoint::HealthChecking,
            RecoveryCheckpoint::CommitPrepared,
            RecoveryCheckpoint::ManifestCommitted,
            RecoveryCheckpoint::SelectorCommitted,
            RecoveryCheckpoint::Committed,
            RecoveryCheckpoint::RollbackPrepared,
        ] {
            let (_temporary, root) = recovery_checkpoint_fixture(checkpoint);
            let status = recover_update(&root).expect("interrupted update recovers");
            let candidate_committed = matches!(
                checkpoint,
                RecoveryCheckpoint::SelectorCommitted | RecoveryCheckpoint::Committed
            );
            let expected_active = if candidate_committed {
                "2.0.0"
            } else {
                "1.0.0"
            };

            assert_eq!(status.active_version, expected_active, "{checkpoint:?}");
            assert_eq!(status.last_good_version, "1.0.0", "{checkpoint:?}");
            assert!(!status.recovery_required, "{checkpoint:?}");
            assert_eq!(status.transaction_phase, None, "{checkpoint:?}");
            assert_eq!(
                root.join("versions/2.0.0").exists(),
                candidate_committed,
                "{checkpoint:?}"
            );
            assert!(
                !root.join("state").join(UPDATE_TRANSACTION_FILE).exists(),
                "{checkpoint:?}"
            );
            assert!(
                !root.join("state").join(ARTIFACT_COPY_NAME).exists(),
                "{checkpoint:?}"
            );
        }
    }

    #[test]
    fn invalid_bootstrap_archive_cleans_owned_scaffolding() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let root = temporary.path().join("install");
        fs::create_dir_all(root.join("user")).expect("user directory creates");
        fs::write(root.join("user/sentinel"), b"user").expect("user sentinel writes");
        let archive = temporary.path().join("invalid.zip");
        let bytes = b"not a package archive";
        fs::write(&archive, bytes).expect("invalid package writes");
        let key_pair = test_key_pair();

        assert!(matches!(
            install_package_with_policy(
                &root,
                &archive,
                &hex_sha256(Sha256::digest(bytes).into()),
                &policy(UpdatePublicKey(*key_pair.pk)),
            ),
            Err(FilesystemUpdateError::Zip(_))
        ));
        assert_eq!(
            fs::read(root.join("user/sentinel")).expect("user sentinel remains"),
            b"user"
        );
        assert!(!root.join("current").exists());
        assert!(!root.join("versions").exists());
        assert!(!root.join("state").exists());
    }
}
