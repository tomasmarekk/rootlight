//! Per-user daemon paths, private output publication, and checked discovery records.
//!
//! This crate separates owner-only preparation from read-only client validation,
//! derives every endpoint locally, and publishes sensitive artifacts atomically.

#![forbid(unsafe_code)]

use std::{
    fs::{self, File, TryLockError},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use directories::ProjectDirs;
use rootlight_ipc::Endpoint;
use rootlight_protocol::{CURRENT_PROTOCOL_MINOR, MINIMUM_PROTOCOL_MINOR};
use serde::{Deserialize, Serialize};

/// Maximum serialized discovery record accepted from disk.
pub const MAX_DISCOVERY_BYTES: u64 = 4 * 1024;
/// Current discovery-record schema version.
pub const DISCOVERY_SCHEMA_VERSION: u16 = 2;
/// Current private daemon protocol major version.
pub const PROTOCOL_MAJOR: u32 = 1;
/// Current private daemon protocol minor version.
pub const PROTOCOL_MINOR: u32 = CURRENT_PROTOCOL_MINOR;

const ENDPOINT_ID_PREFIX: &str = "daemon-";
const ENDPOINT_ID_HEX_BYTES: usize = 32;
#[cfg(unix)]
const ENDPOINT_ID_SUFFIX: &str = ".sock";
#[cfg(windows)]
const ENDPOINT_ID_SUFFIX: &str = "";

/// Resolved private paths for one user's Rootlight daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimePaths {
    state_dir: PathBuf,
    runtime_dir: PathBuf,
}

impl RuntimePaths {
    /// Resolves operating-system standard state and runtime locations.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::UserDirectoriesUnavailable`] when current-user
    /// application directories cannot be resolved.
    pub fn resolve() -> Result<Self, RuntimeError> {
        let project = ProjectDirs::from("dev", "tomasmarekk", "rootlight")
            .ok_or(RuntimeError::UserDirectoriesUnavailable)?;
        let state_dir = project
            .state_dir()
            .unwrap_or_else(|| project.data_local_dir())
            .to_path_buf();
        let runtime_dir = project
            .runtime_dir()
            .map_or_else(|| state_dir.join("runtime"), Path::to_path_buf);
        Self::new(state_dir, runtime_dir)
    }

    /// Creates an explicit path set for tests and administrative overrides.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDirectory`] for non-absolute, aliased, or
    /// unsupported platform paths.
    pub fn new(state_dir: PathBuf, runtime_dir: PathBuf) -> Result<Self, RuntimeError> {
        validate_directory_path(&state_dir)?;
        validate_directory_path(&runtime_dir)?;
        Ok(Self {
            state_dir,
            runtime_dir,
        })
    }

    /// Creates and verifies owner-only state and runtime directories.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when a directory cannot be created or made private.
    pub fn prepare_owner(&self) -> Result<(), RuntimeError> {
        prepare_private_directory(&self.state_dir, PrivateScope::Account)?;
        prepare_private_directory(&self.runtime_dir, PrivateScope::Session)
    }

    /// Completes a validated partial owner setup created by a concurrent starter.
    ///
    /// Existing directories are validated without mutation; only the absent peer is
    /// created and secured. This keeps ordinary clients read-only once setup completes.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::OwnerSetupIncomplete`] when both directories vanish,
    /// or a security/IO error when an existing artifact fails validation.
    pub fn complete_owner_setup(&self) -> Result<(), RuntimeError> {
        let state_exists = self.state_dir.try_exists().map_err(RuntimeError::Io)?;
        let runtime_exists = self.runtime_dir.try_exists().map_err(RuntimeError::Io)?;
        match (state_exists, runtime_exists) {
            (true, true) => self.validate_client(),
            (true, false) => {
                validate_private_directory(&self.state_dir, PrivateScope::Account)?;
                prepare_private_directory(&self.runtime_dir, PrivateScope::Session)
            }
            (false, true) => {
                validate_private_directory(&self.runtime_dir, PrivateScope::Session)?;
                prepare_private_directory(&self.state_dir, PrivateScope::Account)
            }
            (false, false) => Err(RuntimeError::OwnerSetupIncomplete),
        }
    }

    /// Preserves the initial owner-preparation API for standalone callers.
    ///
    /// # Errors
    ///
    /// Returns a typed directory privacy failure.
    pub fn prepare(&self) -> Result<(), RuntimeError> {
        self.prepare_owner()
    }

    /// Validates existing runtime artifacts without creating or changing them.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InsecureDirectory`] when either directory is absent,
    /// aliased, foreign-owned, or not private to the current user.
    pub fn validate_client(&self) -> Result<(), RuntimeError> {
        validate_private_directory(&self.state_dir, PrivateScope::Account)?;
        validate_private_directory(&self.runtime_dir, PrivateScope::Session)
    }

    /// Reports whether client runtime directories are both absent.
    ///
    /// Partial absence is reported separately after every existing directory passes
    /// the full owner and privacy policy, so callers can distinguish setup races from
    /// hostile filesystem artifacts.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::OwnerSetupIncomplete`] for one validated directory,
    /// or a security/IO error for an existing artifact that fails validation.
    pub fn client_directories_absent(&self) -> Result<bool, RuntimeError> {
        let state_exists = self.state_dir.try_exists().map_err(RuntimeError::Io)?;
        let runtime_exists = self.runtime_dir.try_exists().map_err(RuntimeError::Io)?;
        if !state_exists && !runtime_exists {
            return Ok(true);
        }
        if state_exists {
            validate_private_directory(&self.state_dir, PrivateScope::Account)?;
        }
        if runtime_exists {
            validate_private_directory(&self.runtime_dir, PrivateScope::Session)?;
        }
        if state_exists && runtime_exists {
            Ok(false)
        } else {
            Err(RuntimeError::OwnerSetupIncomplete)
        }
    }

    /// Returns the durable per-user state directory.
    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Returns the transient per-user runtime directory.
    #[must_use]
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }

    /// Returns the exclusive catalog-writer lock path.
    #[must_use]
    pub fn writer_lock_path(&self) -> PathBuf {
        self.state_dir.join("catalog.writer.lock")
    }

    /// Returns the short-lived daemon startup-arbitration lock path.
    #[must_use]
    pub fn launch_lock_path(&self) -> PathBuf {
        self.runtime_dir.join("daemon.launch.lock")
    }

    /// Acquires short-lived startup authority for this logon session.
    ///
    /// The persistent file is private and no-follow opened. Ownership follows the
    /// file handle, so process termination releases authority without PID probing.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::LaunchBusy`] while another process coordinates startup,
    /// or a typed runtime security or filesystem failure.
    pub fn acquire_launch_lock(&self) -> Result<LaunchLock, RuntimeError> {
        validate_private_directory(&self.runtime_dir, PrivateScope::Session)?;
        LaunchLock::acquire(&self.launch_lock_path())
    }

    /// Returns the durable operation journal path.
    #[must_use]
    pub fn operation_journal_path(&self) -> PathBuf {
        self.state_dir.join("operations.sqlite3")
    }

    /// Returns the checked discovery-record path.
    #[must_use]
    pub fn discovery_path(&self) -> PathBuf {
        self.runtime_dir.join("daemon.json")
    }

    /// Derives the strict endpoint identifier for an instance nonce.
    #[must_use]
    pub fn endpoint_id(&self, nonce: [u8; 16]) -> String {
        format!(
            "{ENDPOINT_ID_PREFIX}{}{ENDPOINT_ID_SUFFIX}",
            encode_nonce(nonce)
        )
    }

    /// Derives a nonce-specific endpoint inside the private runtime namespace.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidEndpoint`] if the endpoint is not representable.
    pub fn endpoint(&self, nonce: [u8; 16]) -> Result<Endpoint, RuntimeError> {
        self.endpoint_from_id(&self.endpoint_id(nonce), nonce)
    }

    /// Applies and verifies the account-private policy on an exclusively opened output file.
    ///
    /// The caller must keep the handle open until content synchronization completes. On
    /// Windows the handle must deny read, write, and delete sharing from creation onward so
    /// inherited directory permissions cannot expose or replace the object before its protected
    /// DACL is installed. On macOS, use [`PrivateOutputFile`] for sensitive new content: mode
    /// hardening alone cannot revoke a descriptor opened through an ACL inherited at creation.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when handle metadata, ownership, permissions, reparse-point,
    /// or Windows DACL validation fails.
    fn secure_private_output_file(file: &mut File) -> Result<(), RuntimeError> {
        let metadata = file.metadata().map_err(RuntimeError::Io)?;
        if !metadata.file_type().is_file() {
            return Err(RuntimeError::InsecureOutputFile);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(RuntimeError::Io)?;
            let metadata = file.metadata().map_err(RuntimeError::Io)?;
            if metadata.uid() != effective_user_id()
                || metadata.nlink() != 1
                || metadata.mode() & 0o077 != 0
            {
                return Err(RuntimeError::InsecureOutputFile);
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
                return Err(RuntimeError::InsecureOutputFile);
            }
            apply_private_windows_dacl_to_file(file, PrivateScope::Account)?;
            verify_private_windows_file_dacl(file, PrivateScope::Account)?;
        }
        Ok(())
    }

    /// Writes a checked discovery record after the daemon endpoint is bound.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for validation, serialization, or atomic publication failures.
    pub fn publish(&self, record: &DiscoveryRecord) -> Result<(), RuntimeError> {
        self.validate_client()?;
        record.validate(self)?;
        let bytes = serde_json::to_vec(record).map_err(RuntimeError::SerializeDiscovery)?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_DISCOVERY_BYTES) {
            return Err(RuntimeError::InvalidDiscovery);
        }
        let mut temporary = tempfile::Builder::new()
            .prefix("daemon-")
            .suffix(".json.new")
            .tempfile_in(&self.runtime_dir)
            .map_err(RuntimeError::Io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(RuntimeError::Io)?;
        }
        temporary.write_all(&bytes).map_err(RuntimeError::Io)?;
        temporary.as_file().sync_all().map_err(RuntimeError::Io)?;
        temporary
            .persist(self.discovery_path())
            .map_err(|error| RuntimeError::Io(error.error))?;
        sync_directory(&self.runtime_dir)
    }

    /// Reads and validates the currently published daemon identity.
    ///
    /// The same no-follow handle supplies metadata and bounded bytes, preventing a
    /// metadata/read replacement race.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] for missing, oversized, malformed, or foreign records.
    pub fn discover(&self) -> Result<DiscoveryRecord, RuntimeError> {
        self.validate_client()?;
        let file = open_discovery_no_follow(&self.discovery_path())?;
        let metadata = file.metadata().map_err(RuntimeError::Io)?;
        validate_discovery_metadata(&metadata)?;
        let maximum = usize::try_from(MAX_DISCOVERY_BYTES)
            .map_err(|_| RuntimeError::InvalidDiscovery)?
            .checked_add(1)
            .ok_or(RuntimeError::InvalidDiscovery)?;
        let mut bytes = Vec::with_capacity(maximum);
        file.take(
            MAX_DISCOVERY_BYTES
                .checked_add(1)
                .ok_or(RuntimeError::InvalidDiscovery)?,
        )
        .read_to_end(&mut bytes)
        .map_err(RuntimeError::Io)?;
        if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_DISCOVERY_BYTES) {
            return Err(RuntimeError::InvalidDiscovery);
        }
        let record: DiscoveryRecord =
            serde_json::from_slice(&bytes).map_err(RuntimeError::DeserializeDiscovery)?;
        record.validate(self)?;
        Ok(record)
    }

    /// Removes the record only when it still names the supplied daemon instance.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when a matching record cannot be removed.
    pub fn remove_discovery_if_matches(&self, nonce: [u8; 16]) -> Result<(), RuntimeError> {
        let Ok(record) = self.discover() else {
            return Ok(());
        };
        if record.instance_nonce() != nonce {
            return Ok(());
        }
        fs::remove_file(self.discovery_path()).map_err(RuntimeError::Io)?;
        sync_directory(&self.runtime_dir)
    }

    /// Removes one exact stale same-user Unix socket derived from a nonce.
    ///
    /// Regular files, links, foreign-owned sockets, and malformed names are never removed.
    ///
    /// # Errors
    ///
    /// Returns a typed validation or filesystem error.
    pub fn remove_stale_endpoint(&self, nonce: [u8; 16]) -> Result<bool, RuntimeError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

            self.validate_client()?;
            let path = self.runtime_dir.join(self.endpoint_id(nonce));
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
                Err(source) => return Err(RuntimeError::Io(source)),
            };
            if !metadata.file_type().is_socket()
                || metadata.uid() != effective_user_id()
                || metadata.mode() & 0o077 != 0
            {
                return Err(RuntimeError::InsecureEndpointArtifact);
            }
            fs::remove_file(path).map_err(RuntimeError::Io)?;
            sync_directory(&self.runtime_dir)?;
            Ok(true)
        }
        #[cfg(windows)]
        {
            let _ = nonce;
            Ok(false)
        }
    }

    fn endpoint_from_id(
        &self,
        endpoint_id: &str,
        nonce: [u8; 16],
    ) -> Result<Endpoint, RuntimeError> {
        if !valid_endpoint_id(endpoint_id, nonce) {
            return Err(RuntimeError::InvalidDiscovery);
        }
        #[cfg(unix)]
        let path = self.runtime_dir.join(endpoint_id);
        #[cfg(windows)]
        let path = PathBuf::from(format!(r"\\.\pipe\rootlight-{endpoint_id}"));
        Endpoint::new(path).map_err(RuntimeError::InvalidEndpoint)
    }
}

/// An owner-private output publication handle.
///
/// On macOS, content is staged behind a sanitized descriptor-anchored directory boundary and
/// published with an atomic no-replace rename. On other platforms, the final path is created
/// exclusively and is visible immediately; a write or commit failure can leave a partial
/// owner-private file at that path.
///
/// The outer macOS directory can briefly inherit an ACL, but it contains no archive bytes; content
/// is created only inside an owner-private directory made after descriptor-bound ACL removal.
///
/// macOS publication fails closed when `/bin/chmod -N /dev/fd/0`, two-link empty-directory
/// semantics, symlink-free parent traversal, no-replace rename, or directory synchronization are
/// unavailable. Cleanup reopens every staging directory name without following links and verifies
/// that its device and inode still match the held descriptor before removal. Call [`Self::abort`]
/// to observe macOS cleanup errors; dropping an unpublished handle performs bounded best-effort
/// cleanup, so abnormal termination, concurrent same-user mutation, or persistent filesystem
/// errors can leave an owner-private staging tree.
#[derive(Debug)]
pub struct PrivateOutputFile {
    file: File,
    state: PrivateOutputState,
}

#[derive(Debug)]
enum PrivateOutputState {
    #[cfg(not(target_os = "macos"))]
    Direct { parent: PathBuf },
    #[cfg(target_os = "macos")]
    Staged(MacosPrivateOutputState),
}

#[cfg(target_os = "macos")]
#[derive(Debug)]
struct MacosPrivateOutputState {
    parent: File,
    destination_name: std::ffi::OsString,
    outer_name: std::ffi::OsString,
    outer: Option<File>,
    outer_present: bool,
    inner_name: Option<std::ffi::OsString>,
    inner: Option<File>,
    inner_present: bool,
    staged_name: &'static str,
    staged_present: bool,
    published: bool,
    #[cfg(test)]
    cleanup_failure: Option<MacosCleanupStep>,
}

#[cfg(all(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacosCleanupStep {
    RemoveStagedFile,
    RemoveInnerDirectory,
    RemoveOuterDirectory,
    SyncParent,
}

#[cfg(target_os = "macos")]
const MACOS_CLEANUP_ATTEMPTS: usize = 3;

impl PrivateOutputFile {
    /// Creates a new private output publication handle for `path`.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::Io`] when the destination cannot be created exclusively, or
    /// [`RuntimeError::PrivateOutputSecurityPolicy`] when macOS cannot establish the staging
    /// boundary required to prevent inherited ACLs from exposing content.
    pub fn create(path: &Path) -> Result<Self, RuntimeError> {
        #[cfg(target_os = "macos")]
        {
            Self::create_macos(path)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Self::create_direct(path)
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn create_direct(path: &Path) -> Result<Self, RuntimeError> {
        let parent = output_parent(path)?.to_path_buf();
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;

            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows::Win32::Storage::FileSystem::{
                FILE_GENERIC_READ, FILE_GENERIC_WRITE, WRITE_DAC,
            };

            options
                .access_mode((FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC).0)
                .share_mode(0);
        }
        let mut file = options.open(path).map_err(RuntimeError::Io)?;
        RuntimePaths::secure_private_output_file(&mut file)?;
        Ok(Self {
            file,
            state: PrivateOutputState::Direct { parent },
        })
    }

    #[cfg(target_os = "macos")]
    fn create_macos(path: &Path) -> Result<Self, RuntimeError> {
        #[cfg(test)]
        {
            Self::create_macos_impl(path, None)
        }
        #[cfg(not(test))]
        {
            Self::create_macos_impl(path)
        }
    }

    #[cfg(target_os = "macos")]
    fn create_macos_impl(
        path: &Path,
        #[cfg(test)] injected_setup_cleanup_failure: Option<MacosCleanupStep>,
    ) -> Result<Self, RuntimeError> {
        use rustix::fs::{Mode, OFlags};

        const STAGED_NAME: &str = "archive";

        let destination_name = macos_output_name(path)?;
        let parent = open_macos_output_parent(path)?;
        let (outer_name, outer) =
            create_open_private_output_directory(&parent, ".rootlight-support-")?;
        let mut state = MacosPrivateOutputState {
            parent,
            destination_name,
            outer_name,
            outer: Some(outer),
            outer_present: true,
            inner_name: None,
            inner: None,
            inner_present: false,
            staged_name: STAGED_NAME,
            staged_present: false,
            published: false,
            #[cfg(test)]
            cleanup_failure: None,
        };

        let setup = (|| {
            let outer = state
                .outer
                .as_ref()
                .ok_or_else(private_output_security_policy)?;
            remove_macos_acl(outer)?;
            rustix::fs::fchmod(outer, Mode::RWXU).map_err(runtime_io)?;
            verify_private_staging_directory(outer)?;

            let (inner_name, inner) = create_open_private_output_directory(outer, "private-")?;
            state.inner_name = Some(inner_name);
            state.inner = Some(inner);
            state.inner_present = true;

            let inner = state
                .inner
                .as_ref()
                .ok_or_else(private_output_security_policy)?;
            verify_private_staging_directory(inner)?;
            let descriptor = rustix::fs::openat(
                inner,
                STAGED_NAME,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(runtime_io)?;
            state.staged_present = true;
            let mut file = File::from(descriptor);
            RuntimePaths::secure_private_output_file(&mut file)?;

            #[cfg(test)]
            if let Some(step) = injected_setup_cleanup_failure {
                state.cleanup_failure = Some(step);
                return Err(private_output_security_io(io::Error::other(
                    "injected macOS partial setup failure",
                )));
            }

            Ok(file)
        })();

        match setup {
            Ok(file) => Ok(Self {
                file,
                state: PrivateOutputState::Staged(state),
            }),
            Err(error) => Err(cleanup_after_macos_setup_failure(&mut state, error)),
        }
    }

    /// Synchronizes and atomically publishes the completed output.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError`] when file synchronization, exclusive publication, or required
    /// directory synchronization fails. A macOS destination collision is reported as
    /// [`RuntimeError::Io`] with [`io::ErrorKind::AlreadyExists`]. Once the macOS no-replace
    /// rename succeeds, a later durability-sync or staging-cleanup error can be returned while the
    /// destination is already visible. On other platforms the destination is visible from
    /// [`Self::create`], and commit only flushes and synchronizes that existing file.
    pub fn commit(mut self) -> Result<(), RuntimeError> {
        let result = (|| {
            self.file.flush().map_err(RuntimeError::Io)?;
            self.file.sync_all().map_err(RuntimeError::Io)?;
            match &mut self.state {
                #[cfg(not(target_os = "macos"))]
                PrivateOutputState::Direct { parent } => sync_directory(parent),
                #[cfg(target_os = "macos")]
                PrivateOutputState::Staged(state) => {
                    let inner = state
                        .inner
                        .as_ref()
                        .ok_or_else(private_output_security_policy)?;
                    if state.outer.is_none() {
                        return Err(private_output_security_policy());
                    }
                    inner.sync_all().map_err(RuntimeError::Io)?;
                    rustix::fs::renameat_with(
                        inner,
                        state.staged_name,
                        &state.parent,
                        &state.destination_name,
                        rustix::fs::RenameFlags::NOREPLACE,
                    )
                    .map_err(|source| RuntimeError::Io(source.into()))?;
                    state.staged_present = false;
                    state.published = true;
                    inner.sync_all().map_err(RuntimeError::Io)?;
                    state.parent.sync_all().map_err(RuntimeError::Io)?;
                    cleanup_macos_private_output(state, false)
                }
            }
        })();

        #[cfg(target_os = "macos")]
        {
            match result {
                Ok(()) => Ok(()),
                Err(error) => {
                    let PrivateOutputState::Staged(state) = &mut self.state;
                    cleanup_macos_private_output(state, !state.published)?;
                    Err(error)
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            result
        }
    }

    /// Abandons an unpublished output and reports cleanup failures.
    ///
    /// On macOS this removes the staged file and both staging directories, then synchronizes the
    /// parent directory. On other platforms publication happens during [`Self::create`], so abort
    /// cannot retract the already-visible final inode and returns successfully without removing it.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::PrivateOutputCleanup`] when macOS cannot remove or durably record
    /// removal of the staging tree. The subsequent destructor retries cleanup a bounded number of
    /// times, but the original failure is still reported.
    pub fn abort(self) -> Result<(), RuntimeError> {
        #[cfg(target_os = "macos")]
        {
            let mut output = self;
            let PrivateOutputState::Staged(state) = &mut output.state;
            cleanup_macos_private_output(state, !state.published)
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(())
        }
    }
}

impl Write for PrivateOutputFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.file.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for PrivateOutputFile {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        {
            let PrivateOutputState::Staged(state) = &mut self.state;
            // Explicit abort reports the first failure; Drop only covers transient failures and
            // callers that cannot report an error during unwinding.
            for _ in 0..MACOS_CLEANUP_ATTEMPTS {
                if cleanup_macos_private_output(state, !state.published).is_ok() {
                    break;
                }
            }
        }
    }
}

fn output_parent(path: &Path) -> Result<&Path, RuntimeError> {
    if path.file_name().is_none() {
        return Err(private_output_security_policy());
    }
    Ok(path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new(".")))
}

fn private_output_security_policy() -> RuntimeError {
    RuntimeError::PrivateOutputSecurityPolicy(None)
}

#[cfg(target_os = "macos")]
fn private_output_security_io(source: io::Error) -> RuntimeError {
    RuntimeError::PrivateOutputSecurityPolicy(Some(source))
}

#[cfg(target_os = "macos")]
fn macos_output_name(path: &Path) -> Result<std::ffi::OsString, RuntimeError> {
    use std::os::unix::ffi::OsStrExt as _;

    let final_component = path
        .as_os_str()
        .as_bytes()
        .rsplit(|byte| *byte == b'/')
        .next()
        .unwrap_or_default();
    if final_component.is_empty() || final_component == b"." || final_component == b".." {
        return Err(private_output_security_policy());
    }
    path.file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(private_output_security_policy)
}

#[cfg(target_os = "macos")]
fn runtime_io(source: rustix::io::Errno) -> RuntimeError {
    RuntimeError::Io(source.into())
}

#[cfg(target_os = "macos")]
fn open_macos_output_parent(path: &Path) -> Result<File, RuntimeError> {
    use std::path::Component;

    use rustix::fs::{Mode, OFlags};

    let parent = output_parent(path)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let initial = if parent.is_absolute() { "/" } else { "." };
    let mut directory = rustix::fs::open(initial, flags, Mode::empty())
        .map(File::from)
        .map_err(|source| private_output_security_io(source.into()))?;

    for component in parent.components() {
        let name = match component {
            Component::Prefix(_) => return Err(private_output_security_policy()),
            Component::RootDir | Component::CurDir => continue,
            Component::ParentDir => std::ffi::OsStr::new(".."),
            Component::Normal(name) => name,
        };
        directory = rustix::fs::openat(&directory, name, flags, Mode::empty())
            .map(File::from)
            .map_err(|source| private_output_security_io(source.into()))?;
    }
    verify_output_parent_directory(&directory)?;
    Ok(directory)
}

#[cfg(target_os = "macos")]
fn create_open_private_output_directory(
    parent: &File,
    prefix: &str,
) -> Result<(std::ffi::OsString, File), RuntimeError> {
    use rustix::fs::{AtFlags, Mode};

    const ATTEMPTS: usize = 16;
    const RANDOM_BYTES: usize = 16;

    for _ in 0..ATTEMPTS {
        let mut random = [0_u8; RANDOM_BYTES];
        getrandom::fill(&mut random).map_err(|_| private_output_security_policy())?;
        let mut name = String::with_capacity(prefix.len() + RANDOM_BYTES * 2);
        name.push_str(prefix);
        for byte in random {
            use std::fmt::Write as _;
            write!(&mut name, "{byte:02x}")
                .unwrap_or_else(|_| unreachable!("formatting into String cannot fail"));
        }
        match rustix::fs::mkdirat(parent, name.as_str(), Mode::RWXU) {
            Ok(()) => {
                // mkdirat applies the process umask, which can remove the owner permissions
                // required to reopen the new directory for descriptor-bound hardening.
                if let Err(source) = rustix::fs::chmodat(
                    parent,
                    name.as_str(),
                    Mode::RWXU,
                    AtFlags::SYMLINK_NOFOLLOW,
                ) {
                    let primary = runtime_io(source);
                    return Err(cleanup_created_directory_after_setup_failure(
                        parent,
                        name.as_ref(),
                        primary,
                    ));
                }
                return match open_private_output_directory_io(parent, name.as_ref()) {
                    Ok(directory) => Ok((name.into(), directory)),
                    Err(primary) => Err(cleanup_created_directory_after_setup_failure(
                        parent,
                        name.as_ref(),
                        RuntimeError::Io(primary),
                    )),
                };
            }
            Err(source) if source == rustix::io::Errno::EXIST => {}
            Err(source) => return Err(RuntimeError::Io(source.into())),
        }
    }
    Err(private_output_security_policy())
}

#[cfg(target_os = "macos")]
fn open_private_output_directory_io(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags};

    rustix::fs::openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(Into::into)
}

#[cfg(target_os = "macos")]
fn verify_output_parent_directory(directory: &File) -> Result<(), RuntimeError> {
    use rustix::fs::FileType;

    let metadata =
        rustix::fs::fstat(directory).map_err(|source| private_output_security_io(source.into()))?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory {
        return Err(private_output_security_policy());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_private_staging_directory(directory: &File) -> Result<(), RuntimeError> {
    use rustix::fs::FileType;

    let metadata = rustix::fs::fstat(directory).map_err(runtime_io)?;
    if FileType::from_raw_mode(metadata.st_mode) != FileType::Directory
        || metadata.st_uid != effective_user_id()
        || metadata.st_mode & 0o077 != 0
        || metadata.st_nlink != 2
    {
        return Err(private_output_security_policy());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_macos_acl(directory: &File) -> Result<(), RuntimeError> {
    remove_macos_acl_with_program(directory, Path::new("/bin/chmod"))
}

#[cfg(target_os = "macos")]
fn remove_macos_acl_with_program(directory: &File, program: &Path) -> Result<(), RuntimeError> {
    use std::process::{Command, Stdio};

    let descriptor = directory.try_clone().map_err(private_output_security_io)?;
    let status = Command::new(program)
        .args(["-N", "/dev/fd/0"])
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::from(descriptor))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(private_output_security_io)?;
    if status.success() {
        Ok(())
    } else {
        Err(private_output_security_io(io::Error::other(format!(
            "macOS ACL removal command exited with {status}"
        ))))
    }
}

#[cfg(target_os = "macos")]
fn private_output_cleanup_io(source: io::Error) -> RuntimeError {
    RuntimeError::PrivateOutputCleanup(source)
}

#[cfg(target_os = "macos")]
fn cleanup_created_directory_after_setup_failure(
    parent: &File,
    name: &std::ffi::OsStr,
    primary: RuntimeError,
) -> RuntimeError {
    use rustix::fs::AtFlags;

    let first_cleanup = match rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR) {
        Ok(()) => return primary,
        Err(source) => private_output_cleanup_io(source.into()),
    };
    for _ in 1..MACOS_CLEANUP_ATTEMPTS {
        if rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).is_ok() {
            break;
        }
    }
    first_cleanup
}

#[cfg(target_os = "macos")]
fn cleanup_after_macos_setup_failure(
    state: &mut MacosPrivateOutputState,
    primary: RuntimeError,
) -> RuntimeError {
    let first_cleanup = match cleanup_macos_private_output(state, true) {
        Ok(()) => return primary,
        Err(error) => error,
    };
    // A transient cleanup error must remain observable even when a bounded retry removes the
    // residue successfully.
    for _ in 1..MACOS_CLEANUP_ATTEMPTS {
        if cleanup_macos_private_output(state, true).is_ok() {
            break;
        }
    }
    first_cleanup
}

#[cfg(target_os = "macos")]
fn remove_linked_directory(
    parent: &File,
    name: &std::ffi::OsStr,
    expected: &File,
) -> Result<(), RuntimeError> {
    use rustix::fs::AtFlags;

    let observed =
        open_private_output_directory_io(parent, name).map_err(private_output_cleanup_io)?;
    let expected_metadata =
        rustix::fs::fstat(expected).map_err(|source| private_output_cleanup_io(source.into()))?;
    let observed_metadata =
        rustix::fs::fstat(&observed).map_err(|source| private_output_cleanup_io(source.into()))?;
    if expected_metadata.st_dev != observed_metadata.st_dev
        || expected_metadata.st_ino != observed_metadata.st_ino
    {
        return Err(private_output_cleanup_io(io::Error::other(
            "macOS staging directory identity changed",
        )));
    }
    rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR)
        .map_err(|source| private_output_cleanup_io(source.into()))
}

#[cfg(all(test, target_os = "macos"))]
fn inject_cleanup_failure(
    state: &mut MacosPrivateOutputState,
    step: MacosCleanupStep,
) -> Result<(), RuntimeError> {
    if state.cleanup_failure == Some(step) {
        state.cleanup_failure = None;
        return Err(private_output_cleanup_io(io::Error::other(format!(
            "injected macOS cleanup failure at {step:?}"
        ))));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn cleanup_macos_private_output(
    state: &mut MacosPrivateOutputState,
    remove_staged_file: bool,
) -> Result<(), RuntimeError> {
    use rustix::fs::AtFlags;

    if remove_staged_file && state.staged_present {
        #[cfg(test)]
        inject_cleanup_failure(state, MacosCleanupStep::RemoveStagedFile)?;
        let inner = state
            .inner
            .as_ref()
            .ok_or_else(private_output_security_policy)?;
        rustix::fs::unlinkat(inner, state.staged_name, AtFlags::empty())
            .map_err(|source| private_output_cleanup_io(source.into()))?;
        state.staged_present = false;
    }

    if state.inner_present {
        #[cfg(test)]
        inject_cleanup_failure(state, MacosCleanupStep::RemoveInnerDirectory)?;
        let outer = state
            .outer
            .as_ref()
            .ok_or_else(private_output_security_policy)?;
        let inner_name = state
            .inner_name
            .as_deref()
            .ok_or_else(private_output_security_policy)?;
        let inner = state
            .inner
            .as_ref()
            .ok_or_else(private_output_security_policy)?;
        remove_linked_directory(outer, inner_name, inner)?;
        state.inner_present = false;
        drop(state.inner.take());
    }

    if state.outer_present {
        #[cfg(test)]
        inject_cleanup_failure(state, MacosCleanupStep::RemoveOuterDirectory)?;
        let outer = state
            .outer
            .as_ref()
            .ok_or_else(private_output_security_policy)?;
        remove_linked_directory(&state.parent, &state.outer_name, outer)?;
        state.outer_present = false;
        drop(state.outer.take());
    }

    #[cfg(test)]
    inject_cleanup_failure(state, MacosCleanupStep::SyncParent)?;
    state.parent.sync_all().map_err(private_output_cleanup_io)
}

/// Exclusive short-lived authority for coordinating one daemon launch.
#[derive(Debug)]
pub struct LaunchLock {
    file: File,
}

impl LaunchLock {
    fn acquire(path: &Path) -> Result<Self, RuntimeError> {
        let file = open_private_lock_file(path, PrivateScope::Session)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file }),
            Err(TryLockError::WouldBlock) => Err(RuntimeError::LaunchBusy),
            Err(TryLockError::Error(source)) => Err(RuntimeError::Io(source)),
        }
    }
}

impl Drop for LaunchLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

/// Strict, bounded record used by clients to authenticate one daemon instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRecord {
    schema_version: u16,
    pid: u32,
    endpoint_id: String,
    instance_nonce: String,
    protocol_major: u32,
    protocol_minor: u32,
}

impl DiscoveryRecord {
    /// Creates a discovery record for a bound endpoint and instance nonce.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidDiscovery`] for a zero process identifier or
    /// an endpoint outside the derived Rootlight namespace.
    pub fn new(
        paths: &RuntimePaths,
        pid: u32,
        endpoint: &Endpoint,
        nonce: [u8; 16],
    ) -> Result<Self, RuntimeError> {
        let endpoint_id = paths.endpoint_id(nonce);
        if paths.endpoint_from_id(&endpoint_id, nonce)? != *endpoint {
            return Err(RuntimeError::InvalidDiscovery);
        }
        let record = Self {
            schema_version: DISCOVERY_SCHEMA_VERSION,
            pid,
            endpoint_id,
            instance_nonce: encode_nonce(nonce),
            protocol_major: PROTOCOL_MAJOR,
            protocol_minor: PROTOCOL_MINOR,
        };
        record.validate(paths)?;
        Ok(record)
    }

    /// Returns the diagnostic process identifier. It does not prove liveness.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the validated daemon endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`RuntimeError::InvalidEndpoint`] if the retained identifier is invalid.
    pub fn endpoint(&self, paths: &RuntimePaths) -> Result<Endpoint, RuntimeError> {
        paths.endpoint_from_id(&self.endpoint_id, self.instance_nonce())
    }

    /// Returns the strict endpoint identifier.
    #[must_use]
    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    /// Returns the decoded instance nonce.
    ///
    /// # Panics
    ///
    /// Panics only if private validated state is mutated without checked construction.
    #[must_use]
    pub fn instance_nonce(&self) -> [u8; 16] {
        let Some(nonce) = decode_nonce(&self.instance_nonce) else {
            unreachable!("validated discovery nonce remains hexadecimal");
        };
        nonce
    }

    fn validate(&self, paths: &RuntimePaths) -> Result<(), RuntimeError> {
        if self.schema_version != DISCOVERY_SCHEMA_VERSION
            || self.pid == 0
            || self.protocol_major != PROTOCOL_MAJOR
            || self.protocol_minor < MINIMUM_PROTOCOL_MINOR
        {
            return Err(RuntimeError::InvalidDiscovery);
        }
        let nonce = decode_nonce(&self.instance_nonce).ok_or(RuntimeError::InvalidDiscovery)?;
        paths.endpoint_from_id(&self.endpoint_id, nonce)?;
        Ok(())
    }
}

fn validate_directory_path(path: &Path) -> Result<(), RuntimeError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(RuntimeError::InvalidDirectory);
    }
    if path.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )
    }) {
        return Err(RuntimeError::InvalidDirectory);
    }
    #[cfg(windows)]
    {
        use std::path::Prefix;
        match path.components().next() {
            Some(std::path::Component::Prefix(prefix))
                if matches!(
                    prefix.kind(),
                    Prefix::UNC(_, _)
                        | Prefix::VerbatimUNC(_, _)
                        | Prefix::DeviceNS(_)
                        | Prefix::Verbatim(_)
                ) =>
            {
                return Err(RuntimeError::InvalidDirectory);
            }
            _ => {}
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrivateScope {
    Account,
    Session,
}

fn prepare_private_directory(path: &Path, scope: PrivateScope) -> Result<(), RuntimeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(RuntimeError::InsecureDirectory);
        }
        Ok(_) => {}
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(RuntimeError::Io)?;
        }
        Err(source) => return Err(RuntimeError::Io(source)),
    }
    #[cfg(unix)]
    fs::set_permissions(path, unix_private_directory_permissions()).map_err(RuntimeError::Io)?;
    #[cfg(windows)]
    {
        if windows_path_has_reparse_component(path)? {
            return Err(RuntimeError::InsecureDirectory);
        }
        apply_private_windows_dacl(path, scope)?;
    }
    validate_private_directory(path, scope)
}

fn validate_private_directory(path: &Path, scope: PrivateScope) -> Result<(), RuntimeError> {
    let metadata = fs::symlink_metadata(path).map_err(RuntimeError::Io)?;
    if !metadata.file_type().is_dir() {
        return Err(RuntimeError::InsecureDirectory);
    }
    #[cfg(unix)]
    {
        let _ = scope;
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != effective_user_id() || metadata.mode() & 0o077 != 0 {
            return Err(RuntimeError::InsecureDirectory);
        }
    }
    #[cfg(windows)]
    {
        if metadata.file_type().is_symlink() || windows_path_has_reparse_component(path)? {
            return Err(RuntimeError::InsecureDirectory);
        }
        verify_private_windows_dacl(path, scope)?;
    }
    Ok(())
}

fn open_discovery_no_follow(path: &Path) -> Result<File, RuntimeError> {
    #[cfg(unix)]
    {
        use nix::{
            fcntl::{OFlag, open},
            sys::stat::Mode,
        };

        let descriptor = open(
            path,
            OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::empty(),
        )
        .map_err(|source| RuntimeError::Io(io::Error::from_raw_os_error(source as i32)))?;
        Ok(File::from(descriptor))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        options.open(path).map_err(RuntimeError::Io)
    }
}

fn open_private_lock_file(path: &Path, scope: PrivateScope) -> Result<File, RuntimeError> {
    #[cfg(unix)]
    let file = {
        use nix::{
            fcntl::{OFlag, open},
            sys::stat::Mode,
        };

        let descriptor = open(
            path,
            OFlag::O_RDWR | OFlag::O_CREAT | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
            Mode::from_bits_truncate(0o600),
        )
        .map_err(|source| RuntimeError::Io(io::Error::from_raw_os_error(source as i32)))?;
        File::from(descriptor)
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let mut options = fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        let file = options.open(path).map_err(RuntimeError::Io)?;
        let metadata = file.metadata().map_err(RuntimeError::Io)?;
        use std::os::windows::fs::MetadataExt as _;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if !metadata.file_type().is_file()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
        {
            return Err(RuntimeError::InsecureLockFile);
        }
        apply_private_windows_dacl(path, scope)?;
        verify_private_windows_dacl(path, scope)?;
        file
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = scope;
        let metadata = file.metadata().map_err(RuntimeError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.uid() != effective_user_id()
            || metadata.mode() & 0o077 != 0
        {
            return Err(RuntimeError::InsecureLockFile);
        }
    }
    Ok(file)
}

fn validate_discovery_metadata(metadata: &fs::Metadata) -> Result<(), RuntimeError> {
    if !metadata.file_type().is_file() || metadata.len() > MAX_DISCOVERY_BYTES {
        return Err(RuntimeError::InvalidDiscovery);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != effective_user_id() || metadata.mode() & 0o077 != 0 {
            return Err(RuntimeError::InvalidDiscovery);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(RuntimeError::InvalidDiscovery);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn private_windows_descriptor(
    scope: PrivateScope,
) -> Result<windows_permissions::LocalBox<windows_permissions::SecurityDescriptor>, RuntimeError> {
    use windows_permissions::{LocalBox, SecurityDescriptor};

    let expected_sids = windows_scope_sids(scope)?;
    let mut sddl = String::from("D:P");
    for sid in expected_sids {
        use std::fmt::Write as _;
        write!(&mut sddl, "(A;;FA;;;{sid})").map_err(|_| RuntimeError::WindowsSecurityPolicy)?;
    }
    let descriptor: LocalBox<SecurityDescriptor> = sddl
        .parse()
        .map_err(|_| RuntimeError::WindowsSecurityPolicy)?;
    Ok(descriptor)
}

#[cfg(windows)]
fn apply_private_windows_dacl_to_file(
    file: &mut File,
    scope: PrivateScope,
) -> Result<(), RuntimeError> {
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetSecurityInfo,
    };

    let descriptor = private_windows_descriptor(scope)?;
    let dacl = descriptor
        .dacl()
        .ok_or(RuntimeError::WindowsSecurityPolicy)?;
    SetSecurityInfo(
        file,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(RuntimeError::Io)
}

#[cfg(windows)]
fn verify_private_windows_file_dacl(file: &File, scope: PrivateScope) -> Result<(), RuntimeError> {
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::GetSecurityInfo,
    };

    let expected_sids = windows_scope_sids(scope)?;
    let descriptor = GetSecurityInfo(
        file,
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
    )
    .map_err(RuntimeError::Io)?;
    verify_windows_descriptor(&descriptor, &expected_sids)
}

#[cfg(windows)]
fn apply_private_windows_dacl(path: &Path, scope: PrivateScope) -> Result<(), RuntimeError> {
    use windows_permissions::{
        LocalBox, SecurityDescriptor,
        constants::{SeObjectType, SecurityInformation},
        wrappers::SetNamedSecurityInfo,
    };

    let descriptor: LocalBox<SecurityDescriptor> = private_windows_descriptor(scope)?;
    let dacl = descriptor
        .dacl()
        .ok_or(RuntimeError::WindowsSecurityPolicy)?;
    SetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        None,
        None,
        Some(dacl),
        None,
    )
    .map_err(RuntimeError::Io)
}

#[cfg(windows)]
fn verify_private_windows_dacl(path: &Path, scope: PrivateScope) -> Result<(), RuntimeError> {
    use windows_permissions::{
        constants::{SeObjectType, SecurityInformation},
        wrappers::GetNamedSecurityInfo,
    };

    let expected_sids = windows_scope_sids(scope)?;
    let descriptor = GetNamedSecurityInfo(
        path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
    )
    .map_err(RuntimeError::Io)?;
    verify_windows_descriptor(&descriptor, &expected_sids)
}

#[cfg(windows)]
fn verify_windows_descriptor(
    descriptor: &windows_permissions::SecurityDescriptor,
    expected_sids: &[String],
) -> Result<(), RuntimeError> {
    use windows_permissions::{
        constants::SecurityInformation,
        wrappers::ConvertSecurityDescriptorToStringSecurityDescriptor,
    };

    let dacl = descriptor
        .dacl()
        .ok_or(RuntimeError::WindowsSecurityPolicy)?;
    let sddl =
        ConvertSecurityDescriptorToStringSecurityDescriptor(descriptor, SecurityInformation::Dacl)
            .map_err(RuntimeError::Io)?;
    if !sddl.to_string_lossy().starts_with("D:P") {
        return Err(RuntimeError::WindowsSecurityPolicy);
    }
    let expected_count =
        u32::try_from(expected_sids.len()).map_err(|_| RuntimeError::WindowsSecurityPolicy)?;
    if dacl.len() != expected_count {
        return Err(RuntimeError::WindowsSecurityPolicy);
    }
    let mut observed_sids = Vec::with_capacity(expected_sids.len());
    for index in 0..dacl.len() {
        let ace = dacl
            .get_ace(index)
            .ok_or(RuntimeError::WindowsSecurityPolicy)?;
        if ace.ace_type() != windows_permissions::constants::AceType::ACCESS_ALLOWED_ACE_TYPE
            || ace.mask() != windows_permissions::constants::AccessRights::FileAllAccess
            || !ace.flags().is_empty()
        {
            return Err(RuntimeError::WindowsSecurityPolicy);
        }
        let sid = ace
            .sid()
            .ok_or(RuntimeError::WindowsSecurityPolicy)?
            .to_string();
        if observed_sids.contains(&sid) || !expected_sids.contains(&sid) {
            return Err(RuntimeError::WindowsSecurityPolicy);
        }
        observed_sids.push(sid);
    }
    if !expected_sids.iter().all(|sid| observed_sids.contains(sid)) {
        return Err(RuntimeError::WindowsSecurityPolicy);
    }
    Ok(())
}

#[cfg(windows)]
fn windows_scope_sids(scope: PrivateScope) -> Result<Vec<String>, RuntimeError> {
    use nt_token::OwnedToken;
    use windows::Win32::Security::TOKEN_QUERY;

    let token = OwnedToken::from_current_process(TOKEN_QUERY)
        .map_err(|_| RuntimeError::WindowsSecurityPolicy)?;
    match scope {
        PrivateScope::Account => Ok(vec![
            token
                .user()
                .and_then(|sid| sid.to_string())
                .map_err(|_| RuntimeError::WindowsSecurityPolicy)?,
        ]),
        PrivateScope::Session => {
            let sids = token
                .logon_sid()
                .map_err(|_| RuntimeError::WindowsSecurityPolicy)?
                .into_iter()
                .map(|group| {
                    group
                        .sid()
                        .to_string()
                        .map_err(|_| RuntimeError::WindowsSecurityPolicy)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if sids.is_empty() {
                return Err(RuntimeError::WindowsSecurityPolicy);
            }
            Ok(sids)
        }
    }
}

#[cfg(windows)]
fn windows_path_has_reparse_component(path: &Path) -> Result<bool, RuntimeError> {
    use std::os::windows::fs::MetadataExt as _;
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        let metadata = fs::symlink_metadata(&current).map_err(RuntimeError::Io)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn valid_endpoint_id(value: &str, nonce: [u8; 16]) -> bool {
    let expected = format!(
        "{ENDPOINT_ID_PREFIX}{}{ENDPOINT_ID_SUFFIX}",
        encode_nonce(nonce)
    );
    value.len() == ENDPOINT_ID_PREFIX.len() + ENDPOINT_ID_HEX_BYTES + ENDPOINT_ID_SUFFIX.len()
        && value == expected
}

#[cfg(unix)]
fn unix_private_directory_permissions() -> fs::Permissions {
    use std::os::unix::fs::PermissionsExt as _;
    fs::Permissions::from_mode(0o700)
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    nix::unistd::geteuid().as_raw()
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), RuntimeError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(RuntimeError::Io)
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<(), RuntimeError> {
    Ok(())
}

fn encode_nonce(nonce: [u8; 16]) -> String {
    let mut encoded = String::with_capacity(32);
    for byte in nonce {
        use std::fmt::Write as _;
        let _ = write!(&mut encoded, "{byte:02x}");
    }
    encoded
}

fn decode_nonce(value: &str) -> Option<[u8; 16]> {
    if value.len() != 32
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return None;
    }
    let mut nonce = [0_u8; 16];
    for (index, output) in nonce.iter_mut().enumerate() {
        let start = index.checked_mul(2)?;
        *output = u8::from_str_radix(value.get(start..start + 2)?, 16).ok()?;
    }
    Some(nonce)
}

/// Per-user path and discovery failures.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// Operating-system current-user directories were unavailable.
    #[error("current-user application directories are unavailable")]
    UserDirectoriesUnavailable,
    /// A configured directory was not an absolute normalized path.
    #[error("daemon directory is invalid")]
    InvalidDirectory,
    /// Owner startup has created only one of the two validated private directories.
    #[error("daemon owner directory setup is incomplete")]
    OwnerSetupIncomplete,
    /// A directory did not satisfy the private-user filesystem policy.
    #[error("daemon directory permissions are insecure")]
    InsecureDirectory,
    /// A discovery record failed schema, bounds, or namespace validation.
    #[error("daemon discovery record is invalid")]
    InvalidDiscovery,
    /// A stale endpoint artifact was not a removable same-user private socket.
    #[error("daemon endpoint artifact is insecure")]
    InsecureEndpointArtifact,
    /// A persistent startup lock artifact violated the private-file policy.
    #[error("daemon startup lock artifact is insecure")]
    InsecureLockFile,
    /// A user-selected protected output violated the private-file policy.
    #[error("protected output file is insecure")]
    InsecureOutputFile,
    /// The platform could not establish the private output publication boundary.
    ///
    /// The optional source retains internal operating-system context when the boundary operation
    /// itself failed.
    #[error("protected output security policy failed")]
    PrivateOutputSecurityPolicy(#[source] Option<io::Error>),
    /// A private output staging tree could not be removed or its removal synchronized.
    #[error("protected output cleanup failed")]
    PrivateOutputCleanup(#[source] io::Error),
    /// Another process currently owns startup authority.
    #[error("daemon startup is already in progress")]
    LaunchBusy,
    /// Windows token, reparse-point, or ACL verification failed.
    #[error("daemon Windows security policy failed")]
    WindowsSecurityPolicy,
    /// The endpoint failed platform transport validation.
    #[error("daemon endpoint is invalid")]
    InvalidEndpoint(#[source] rootlight_ipc::IpcError),
    /// Discovery serialization failed.
    #[error("daemon discovery serialization failed")]
    SerializeDiscovery(#[source] serde_json::Error),
    /// Discovery deserialization failed.
    #[error("daemon discovery deserialization failed")]
    DeserializeDiscovery(#[source] serde_json::Error),
    /// A runtime filesystem operation failed.
    #[error("daemon runtime filesystem operation failed")]
    Io(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    fn private_tempdir() -> tempfile::TempDir {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(temporary.path(), fs::Permissions::from_mode(0o700))
                .expect("temporary directory becomes private");
        }
        temporary
    }

    fn private_output_path(temporary: &tempfile::TempDir, name: &str) -> PathBuf {
        #[cfg(target_os = "macos")]
        let parent = fs::canonicalize(temporary.path()).expect("temporary path canonicalizes");
        #[cfg(not(target_os = "macos"))]
        let parent = temporary.path().to_path_buf();
        parent.join(name)
    }

    fn paths() -> (tempfile::TempDir, RuntimePaths) {
        let temporary = private_tempdir();
        let state = temporary.path().join("state");
        let runtime = temporary.path().join("runtime");
        let paths = RuntimePaths::new(state, runtime).expect("explicit paths are valid");
        paths.prepare_owner().expect("private directories prepare");
        (temporary, paths)
    }

    #[test]
    fn private_output_policy_validates_the_open_regular_file() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create_new(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            use windows::Win32::Storage::FileSystem::{
                FILE_GENERIC_READ, FILE_GENERIC_WRITE, WRITE_DAC,
            };
            options
                .access_mode((FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC).0)
                .share_mode(0);
        }
        let mut file = options.open(&path).expect("output file creates");

        RuntimePaths::secure_private_output_file(&mut file).expect("output policy applies");

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let metadata = file.metadata().expect("output metadata reads");
            assert_eq!(metadata.mode() & 0o077, 0);
            assert_eq!(metadata.nlink(), 1);
        }
        #[cfg(windows)]
        verify_private_windows_file_dacl(&file, PrivateScope::Account)
            .expect("output account DACL verifies through its handle");
    }

    #[cfg(unix)]
    #[test]
    fn private_output_policy_rejects_hard_linked_files() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");
        let alias = temporary.path().join("support-alias.zip");
        let mut options = fs::OpenOptions::new();
        use std::os::unix::fs::OpenOptionsExt as _;
        options.read(true).write(true).create_new(true).mode(0o600);
        let mut file = options.open(&path).expect("output file creates");
        fs::hard_link(&path, &alias).expect("hard link creates");

        assert!(matches!(
            RuntimePaths::secure_private_output_file(&mut file),
            Err(RuntimeError::InsecureOutputFile)
        ));
    }

    #[test]
    fn private_output_transaction_refuses_overwrite() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");
        fs::write(&path, b"existing").expect("existing output writes");

        let result = match PrivateOutputFile::create(&path) {
            Ok(output) => output.commit(),
            Err(error) => Err(error),
        };
        let error = result.expect_err("overwrite is refused");

        assert!(matches!(
            error,
            RuntimeError::Io(source) if source.kind() == io::ErrorKind::AlreadyExists
        ));
        assert_eq!(fs::read(path).expect("existing output reads"), b"existing");
        #[cfg(target_os = "macos")]
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            1
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_transaction_rejects_directory_path_syntax() {
        let temporary = private_tempdir();
        let base = fs::canonicalize(temporary.path()).expect("temporary path canonicalizes");
        for (requested, normalized) in [("trailing/", "trailing"), ("dot/.", "dot")] {
            let path = base.join(requested);

            assert!(matches!(
                PrivateOutputFile::create(&path),
                Err(RuntimeError::PrivateOutputSecurityPolicy(_))
            ));
            assert!(!base.join(normalized).exists());
        }
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            0
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_rejects_symlinked_parent_components() {
        use std::os::unix::fs::symlink;

        let temporary = private_tempdir();
        let base = fs::canonicalize(temporary.path()).expect("temporary path canonicalizes");
        let final_target = base.join("final-target");
        let intermediate_target = base.join("intermediate-target");
        fs::create_dir(&final_target).expect("final target creates");
        fs::create_dir(&intermediate_target).expect("intermediate target creates");
        fs::create_dir(intermediate_target.join("nested")).expect("nested target creates");

        let final_link = base.join("final-link");
        let intermediate_link = base.join("intermediate-link");
        symlink(&final_target, &final_link).expect("final parent link creates");
        symlink(&intermediate_target, &intermediate_link)
            .expect("intermediate parent link creates");

        for path in [
            final_link.join("support.zip"),
            intermediate_link.join("nested").join("support.zip"),
        ] {
            assert!(matches!(
                PrivateOutputFile::create(&path),
                Err(RuntimeError::PrivateOutputSecurityPolicy(_))
            ));
            assert!(!path.exists());
        }
        assert_eq!(
            fs::read_dir(&final_target)
                .expect("final target reads")
                .count(),
            0
        );
        assert_eq!(
            fs::read_dir(intermediate_target.join("nested"))
                .expect("nested target reads")
                .count(),
            0
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_transaction_tolerates_restrictive_umask() {
        const CHILD_MARKER: &str = "ROOTLIGHT_TEST_RESTRICTIVE_OUTPUT_UMASK";
        const TEST_NAME: &str = "tests::private_output_transaction_tolerates_restrictive_umask";

        if std::env::var_os(CHILD_MARKER).is_some() {
            let temporary = private_tempdir();
            nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o777));
            let path = private_output_path(&temporary, "support.zip");
            let mut output = PrivateOutputFile::create(&path).expect("private output creates");
            output.write_all(b"bundle").expect("private output writes");
            output.commit().expect("private output publishes");

            assert_eq!(fs::read(&path).expect("published output reads"), b"bundle");
            assert_eq!(
                fs::read_dir(temporary.path())
                    .expect("temporary directory reads")
                    .count(),
                1
            );
            return;
        }

        let child = std::process::Command::new(
            std::env::current_exe().expect("current test executable resolves"),
        )
        .args(["--exact", TEST_NAME, "--nocapture"])
        .env(CHILD_MARKER, "1")
        .output()
        .expect("restrictive-umask child runs");
        assert!(
            child.status.success(),
            "restrictive-umask child failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unpublished_private_output_is_cleaned_up() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");

        let mut output = PrivateOutputFile::create(&path).expect("private output creates");
        output.write_all(b"partial").expect("partial output writes");
        drop(output);

        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            0
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_abort_preserves_replaced_outer_directory() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");
        let mut output = PrivateOutputFile::create(&path).expect("private output creates");
        output.write_all(b"partial").expect("private output writes");
        let PrivateOutputState::Staged(state) = &output.state;
        let parent = path.parent().expect("output parent exists");
        let outer_path = parent.join(&state.outer_name);
        let displaced_path = parent.join("displaced-staging");
        fs::rename(&outer_path, &displaced_path).expect("staging directory displaces");
        fs::create_dir(&outer_path).expect("replacement directory creates");
        fs::set_permissions(&outer_path, fs::Permissions::from_mode(0o700))
            .expect("replacement directory becomes private");
        let replacement = fs::metadata(&outer_path).expect("replacement metadata reads");

        let error = output
            .abort()
            .expect_err("replacement identity stops cleanup");

        assert!(matches!(error, RuntimeError::PrivateOutputCleanup(_)));
        let observed = fs::metadata(&outer_path).expect("replacement remains");
        assert_eq!(
            (observed.dev(), observed.ino()),
            (replacement.dev(), replacement.ino())
        );
        assert!(displaced_path.is_dir());
        assert!(!path.exists());
        let source = std::error::Error::source(&error).expect("cleanup error retains a source");
        assert_eq!(
            source.to_string(),
            "macOS staging directory identity changed"
        );
        assert!(
            !source
                .to_string()
                .contains(temporary.path().to_string_lossy().as_ref())
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_partial_setup_cleanup_failure_is_reported() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");

        let error =
            PrivateOutputFile::create_macos_impl(&path, Some(MacosCleanupStep::RemoveStagedFile))
                .expect_err("injected setup cleanup failure reports");

        assert!(matches!(error, RuntimeError::PrivateOutputCleanup(_)));
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            0
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_abort_reports_cleanup_failure_and_drop_retries() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");
        let mut output = PrivateOutputFile::create(&path).expect("private output creates");
        output.write_all(b"partial").expect("private output writes");
        let PrivateOutputState::Staged(state) = &mut output.state;
        state.cleanup_failure = Some(MacosCleanupStep::RemoveStagedFile);

        let error = output
            .abort()
            .expect_err("injected cleanup failure reports");

        assert!(matches!(error, RuntimeError::PrivateOutputCleanup(_)));
        assert!(!path.exists());
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            0
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn private_output_commit_reports_cleanup_failure_after_publication() {
        let temporary = private_tempdir();
        let path = private_output_path(&temporary, "support.zip");
        let mut output = PrivateOutputFile::create(&path).expect("private output creates");
        output.write_all(b"bundle").expect("private output writes");
        let PrivateOutputState::Staged(state) = &mut output.state;
        state.cleanup_failure = Some(MacosCleanupStep::RemoveInnerDirectory);

        let error = output
            .commit()
            .expect_err("injected post-publication cleanup failure reports");

        assert!(matches!(error, RuntimeError::PrivateOutputCleanup(_)));
        assert_eq!(fs::read(&path).expect("published output reads"), b"bundle");
        assert_eq!(
            fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            1
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_acl_spawn_failure_is_a_security_policy_error_with_source() {
        let temporary = private_tempdir();
        let directory = File::open(temporary.path()).expect("temporary directory opens");
        let missing_program = temporary.path().join("missing-chmod");

        let error = remove_macos_acl_with_program(&directory, &missing_program)
            .expect_err("missing chmod executable fails");

        assert!(std::error::Error::source(&error).is_some());
        match error {
            RuntimeError::PrivateOutputSecurityPolicy(Some(source)) => {
                assert_eq!(source.kind(), io::ErrorKind::NotFound);
            }
            other => panic!("unexpected ACL boundary error: {other:?}"),
        }
    }

    #[test]
    fn discovery_round_trip_is_checked_namespaced_and_read_only() {
        let (_temporary, paths) = paths();
        let nonce = [7; 16];
        let endpoint = paths.endpoint(nonce).expect("endpoint derives");
        let record = DiscoveryRecord::new(&paths, std::process::id(), &endpoint, nonce)
            .expect("record validates");

        paths.publish(&record).expect("record publishes");

        let discovered = paths.discover().expect("record discovers");
        assert_eq!(discovered, record);
        assert_eq!(
            discovered.endpoint(&paths).expect("endpoint derives"),
            endpoint
        );
        assert_eq!(record.endpoint_id(), paths.endpoint_id(nonce));
    }

    #[test]
    fn client_validation_never_creates_directories() {
        let temporary = private_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("paths are valid");

        assert!(paths.client_directories_absent().expect("absence checks"));
        assert!(matches!(paths.validate_client(), Err(RuntimeError::Io(_))));
        assert!(!paths.state_dir().exists());
        assert!(!paths.runtime_dir().exists());
    }

    #[test]
    fn client_directory_absence_classifies_valid_partial_setup() {
        let temporary = private_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("paths are valid");
        prepare_private_directory(paths.state_dir(), PrivateScope::Account)
            .expect("partial state is private");

        assert!(matches!(
            paths.client_directories_absent(),
            Err(RuntimeError::OwnerSetupIncomplete)
        ));
        assert!(!paths.runtime_dir().exists());
    }

    #[test]
    fn owner_setup_completes_valid_partial_state() {
        let temporary = private_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("paths are valid");
        prepare_private_directory(paths.state_dir(), PrivateScope::Account)
            .expect("partial state is private");

        paths
            .complete_owner_setup()
            .expect("missing runtime directory is secured");

        paths.validate_client().expect("completed setup validates");
    }

    #[test]
    fn client_directory_absence_rejects_insecure_partial_setup() {
        let temporary = private_tempdir();
        let paths = RuntimePaths::new(
            temporary.path().join("state"),
            temporary.path().join("runtime"),
        )
        .expect("paths are valid");
        fs::write(paths.state_dir(), b"not a directory").expect("hostile artifact writes");

        assert!(matches!(
            paths.client_directories_absent(),
            Err(RuntimeError::InsecureDirectory)
        ));
        assert!(!paths.runtime_dir().exists());
    }

    #[test]
    fn launch_lock_is_exclusive_and_reuses_private_artifact() {
        let (_temporary, paths) = paths();
        let first = paths.acquire_launch_lock().expect("first lock acquires");
        assert!(matches!(
            paths.acquire_launch_lock(),
            Err(RuntimeError::LaunchBusy)
        ));
        drop(first);

        let second = paths.acquire_launch_lock().expect("persistent lock reuses");
        drop(second);
        assert!(paths.launch_lock_path().is_file());
    }

    #[cfg(unix)]
    #[test]
    fn launch_lock_refuses_links_and_public_artifacts() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let (_temporary, paths) = paths();
        let target = paths.runtime_dir().join("target.lock");
        fs::write(&target, b"").expect("target writes");
        symlink(&target, paths.launch_lock_path()).expect("link creates");
        assert!(matches!(
            paths.acquire_launch_lock(),
            Err(RuntimeError::Io(_))
        ));
        fs::remove_file(paths.launch_lock_path()).expect("link removes");
        fs::write(paths.launch_lock_path(), b"").expect("lock fixture writes");
        fs::set_permissions(paths.launch_lock_path(), fs::Permissions::from_mode(0o644))
            .expect("fixture becomes public");
        assert!(matches!(
            paths.acquire_launch_lock(),
            Err(RuntimeError::InsecureLockFile)
        ));
    }

    #[test]
    fn discovery_rejects_unknown_fields_endpoint_substitution_and_links() {
        let (_temporary, paths) = paths();
        fs::write(
            paths.discovery_path(),
            r#"{"schema_version":2,"pid":1,"endpoint_id":"invalid","instance_nonce":"00000000000000000000000000000000","protocol_major":1,"protocol_minor":1,"extra":true}"#,
        )
        .expect("fixture writes");
        assert!(matches!(
            paths.discover(),
            Err(RuntimeError::DeserializeDiscovery(_))
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let target = paths.runtime_dir().join("target.json");
            fs::write(&target, b"{}").expect("target writes");
            let _ = fs::remove_file(paths.discovery_path());
            symlink(target, paths.discovery_path()).expect("link creates");
            assert!(matches!(paths.discover(), Err(RuntimeError::Io(_))));
        }
    }

    #[test]
    fn discovery_allows_negotiation_with_future_minor_and_rejects_obsolete_minor() {
        let (_temporary, paths) = paths();
        let nonce = [5; 16];
        let endpoint = paths.endpoint(nonce).expect("endpoint derives");
        let mut future = DiscoveryRecord::new(&paths, std::process::id(), &endpoint, nonce)
            .expect("record validates");
        future.protocol_minor = PROTOCOL_MINOR.saturating_add(1);
        paths.publish(&future).expect("future minor publishes");
        assert_eq!(
            paths.discover().expect("future minor is negotiable"),
            future
        );

        future.protocol_minor = MINIMUM_PROTOCOL_MINOR.saturating_sub(1);
        assert!(matches!(
            paths.publish(&future),
            Err(RuntimeError::InvalidDiscovery)
        ));
    }

    #[test]
    fn cleanup_never_removes_a_replaced_record() {
        let (_temporary, paths) = paths();
        let first_nonce = [1; 16];
        let second_nonce = [2; 16];
        let second_endpoint = paths.endpoint(second_nonce).expect("endpoint derives");
        let second =
            DiscoveryRecord::new(&paths, std::process::id(), &second_endpoint, second_nonce)
                .expect("record validates");
        paths.publish(&second).expect("record publishes");

        paths
            .remove_discovery_if_matches(first_nonce)
            .expect("cleanup succeeds");

        assert_eq!(paths.discover().expect("replacement remains"), second);
    }

    #[cfg(unix)]
    #[test]
    fn stale_cleanup_refuses_regular_files_and_links() {
        use std::os::unix::fs::symlink;

        let (_temporary, paths) = paths();
        let nonce = [4; 16];
        let endpoint_path = paths.runtime_dir().join(paths.endpoint_id(nonce));
        fs::write(&endpoint_path, b"not a socket").expect("regular file writes");
        assert!(matches!(
            paths.remove_stale_endpoint(nonce),
            Err(RuntimeError::InsecureEndpointArtifact)
        ));
        fs::remove_file(&endpoint_path).expect("regular file removes");
        symlink(paths.runtime_dir(), &endpoint_path).expect("link creates");
        assert!(matches!(
            paths.remove_stale_endpoint(nonce),
            Err(RuntimeError::InsecureEndpointArtifact)
        ));
    }
}
