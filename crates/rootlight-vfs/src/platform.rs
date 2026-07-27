//! Account-private filesystem trees with identity-preserving publication.
//!
//! Linux, macOS, and Windows use retained directory or file handles for every
//! mutable operation and fail closed when the caller's parent is not
//! account-private. macOS additionally removes and verifies inherited extended
//! ACLs through the retained descriptor before publication.

use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, Write},
    marker::PhantomData,
    path::{Component, Path},
};

use cap_std::fs::Dir;

mod os;

/// Maximum platform name units accepted for one private-tree entry.
pub const MAX_PRIVATE_NAME_UNITS: usize = 255;

/// Exact platform object identity observed through a retained handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlatformFileIdentity {
    volume: u64,
    file: u128,
}

impl PlatformFileIdentity {
    /// Returns the reserved platform volume or device identity.
    #[must_use]
    pub const fn volume(self) -> u64 {
        self.volume
    }

    /// Returns the reserved full-width file identity.
    #[must_use]
    pub const fn file(self) -> u128 {
        self.file
    }
}

/// An unpublished account-private directory owned through a retained handle.
///
/// Dropping a value only closes retained Rust owners; it never attempts
/// filesystem cleanup.
///
/// Correctly scoped callers will continue to compile when an enabled
/// implementation replaces the scaffold:
///
/// ```no_run
/// use std::{ffi::OsStr, io::Write as _};
///
/// use cap_std::fs::Dir;
/// use rootlight_vfs::platform::PrivateDirectory;
///
/// # fn stage(parent: &Dir, destination: &Dir) -> Result<(), Box<dyn std::error::Error>> {
/// let directory = PrivateDirectory::create(parent, OsStr::new("staging"))?;
/// {
///     let mut file = directory.create_file(OsStr::new("bundle"))?;
///     file.write_all(b"evidence")?;
///     file.sync_all()?;
/// }
/// let _published =
///     directory.publish_noreplace(destination, OsStr::new("bundle-ready"))?;
/// # Ok(())
/// # }
/// ```
#[must_use = "dropping an unpublished private tree discards its owner without publishing it"]
pub struct PrivateDirectory<'parent> {
    inner: Option<os::Directory>,
    parent: PhantomData<&'parent ()>,
}

impl PrivateDirectory<'static> {
    /// Requires an enabled account-private tree implementation.
    ///
    /// This preflight does not inspect a path, acquire randomness, or perform
    /// a filesystem operation. It lets callers fail closed before they touch
    /// ambient state.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::UnsupportedPlatform`] on targets without an
    /// enabled descriptor-bound implementation.
    pub fn require_supported() -> Result<(), PlatformError> {
        os::require_support()
    }

    /// Verifies that an opened directory enforces the account-private policy.
    ///
    /// This does not mutate permissions. Callers must prepare the owner-only
    /// root before passing its retained capability here.
    ///
    /// # Errors
    ///
    /// Returns a typed platform, privacy-policy, or I/O error.
    pub fn verify_parent(parent: &Dir) -> Result<(), PlatformError> {
        os::verify_parent(parent)
    }

    /// Applies and verifies the account-private policy on an opened directory.
    ///
    /// This is intended for a newly created owner directory retained by the
    /// caller. Existing untrusted directories should be checked with
    /// [`Self::verify_parent`] so validation never silently changes policy.
    ///
    /// # Errors
    ///
    /// Returns a typed platform, privacy-policy, or I/O error.
    pub fn secure_new_parent(parent: &mut Dir) -> Result<(), PlatformError> {
        os::secure_parent(parent)
    }

    /// Creates a new account-private child beneath a verified private parent.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, parent-policy, collision, or I/O error.
    pub fn create(parent: &Dir, name: &OsStr) -> Result<Self, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::create_directory(parent, &name).map(|inner| Self {
            inner: Some(inner),
            parent: PhantomData,
        })
    }

    /// Opens an existing account-private child without following a link.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, parent-policy, identity, or I/O error.
    pub fn open(parent: &Dir, name: &OsStr) -> Result<Self, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::open_directory(parent, &name).map(|inner| Self {
            inner: Some(inner),
            parent: PhantomData,
        })
    }
}

impl<'parent> PrivateDirectory<'parent> {
    /// Creates a nested account-private directory.
    ///
    /// # Errors
    ///
    /// Returns a typed name-validation, policy, collision, or I/O error.
    pub fn create_directory<'directory>(
        &'directory self,
        name: &OsStr,
    ) -> Result<PrivateDirectory<'directory>, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::create_child(self.inner(), &name).map(|inner| PrivateDirectory {
            inner: Some(inner),
            parent: PhantomData,
        })
    }

    /// Exclusively creates an account-private regular file.
    ///
    /// # Errors
    ///
    /// Returns a typed name-validation, policy, collision, or I/O error.
    pub fn create_file<'directory>(
        &'directory self,
        name: &OsStr,
    ) -> Result<PrivateFile<'directory>, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::create_file(self.inner(), &name).map(|inner| PrivateFile {
            inner,
            parent: PhantomData,
        })
    }

    /// Reads one existing private regular file without following a link.
    ///
    /// The file identity, owner-only policy, single-link invariant, and stable
    /// length are checked through the retained handle before bytes are returned.
    ///
    /// # Errors
    ///
    /// Returns a typed name, policy, size-bound, or I/O error.
    pub fn read_file_bounded(
        &self,
        name: &OsStr,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::read_file_bounded(self.inner(), &name, maximum_bytes)
    }

    /// Returns the exact identity captured from the retained directory handle.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::directory_identity(self.inner())
    }

    /// Borrows the retained directory capability for nested private operations.
    ///
    /// The returned capability remains tied to this exact validated directory
    /// identity and cannot outlive its owner.
    #[must_use]
    pub fn capability(&self) -> &Dir {
        os::directory_capability(self.inner())
    }

    /// Synchronizes the directory represented by the retained handle.
    ///
    /// # Errors
    ///
    /// Returns a typed platform or I/O error.
    pub fn sync_all(&self) -> Result<(), PlatformError> {
        os::sync_directory(self.inner())
    }

    /// Publishes this directory atomically without replacing a destination.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::NotCommitted`] when no rename occurred, or
    /// [`PublishError::CommittedButDurabilityUnknown`] when the identity moved
    /// but destination-parent synchronization failed.
    pub fn publish_noreplace(
        mut self,
        destination_parent: &Dir,
        destination_name: &OsStr,
    ) -> Result<PublishedPrivateDirectory, PublishError> {
        let destination_name =
            PrivateName::parse(destination_name).map_err(PublishError::not_committed)?;
        let Some(inner) = self.inner.take() else {
            return Err(PublishError::NotCommitted {
                source: PlatformError::SecurityPolicy,
            });
        };
        match os::publish_noreplace(inner, destination_parent, &destination_name) {
            Ok(inner) => Ok(PublishedPrivateDirectory { inner }),
            Err(os::PublishFailure::NotCommitted { directory, source }) => {
                self.inner = Some(*directory);
                Err(PublishError::NotCommitted { source })
            }
            Err(os::PublishFailure::CommittedButDurabilityUnknown { directory, source }) => {
                Err(PublishError::CommittedButDurabilityUnknown {
                    directory: PublishedPrivateDirectory { inner: *directory },
                    source,
                })
            }
        }
    }

    /// Recursively removes the unpublished directory through its retained handle.
    ///
    /// # Errors
    ///
    /// Returns a typed platform or I/O error.
    pub fn remove(mut self) -> Result<(), PlatformError> {
        let Some(inner) = self.inner.take() else {
            return Err(PlatformError::SecurityPolicy);
        };
        os::remove_directory(inner)
    }

    fn inner(&self) -> &os::Directory {
        self.inner
            .as_ref()
            .expect("safe construction always installs scaffold state")
    }
}

impl fmt::Debug for PrivateDirectory<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateDirectory")
            .finish_non_exhaustive()
    }
}

impl Drop for PrivateDirectory<'_> {
    fn drop(&mut self) {
        // Field destruction closes any future retained owner without a path
        // lookup or cleanup mutation.
    }
}

/// An account-private file owner retained until writing completes.
///
/// The explicit destructor keeps the parent borrow live until the file owner is
/// dropped. Publication or removal while a writer remains in scope therefore
/// does not compile:
///
/// ```compile_fail
/// use std::{ffi::OsStr, io::Write as _};
///
/// use cap_std::fs::Dir;
/// use rootlight_vfs::platform::PrivateDirectory;
///
/// # fn invalid(parent: &Dir) -> Result<(), Box<dyn std::error::Error>> {
/// let directory = PrivateDirectory::create(parent, OsStr::new("staging"))?;
/// let mut file = directory.create_file(OsStr::new("bundle"))?;
/// directory.remove()?;
/// file.flush()?;
/// # Ok(())
/// # }
/// ```
#[must_use = "keep the private-file owner alive until writing and synchronization finish"]
pub struct PrivateFile<'parent> {
    inner: os::File,
    parent: PhantomData<&'parent ()>,
}

impl PrivateFile<'_> {
    /// Returns the exact identity captured from the retained file handle.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::file_identity(&self.inner)
    }

    /// Synchronizes file content and metadata through the retained handle.
    ///
    /// # Errors
    ///
    /// Returns a typed platform or I/O error.
    pub fn sync_all(&self) -> Result<(), PlatformError> {
        os::sync_file(&self.inner)
    }
}

impl Write for PrivateFile<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        os::write_file(&mut self.inner, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        os::flush_file(&mut self.inner)
    }
}

impl Drop for PrivateFile<'_> {
    fn drop(&mut self) {
        // The explicit destructor makes dropck retain the parent borrow through
        // destruction; the field owner itself closes any future handle.
    }
}

/// A standalone account-private file and its retained parent capability.
///
/// Creation is handle-relative beneath a verified private parent. The file is
/// created without following links, hardened through its retained descriptor,
/// and synchronized together with the parent on commit.
#[must_use = "commit or retain the standalone private file until writing finishes"]
pub struct PrivateStandaloneFile {
    inner: os::File,
    parent: Dir,
}

impl PrivateStandaloneFile {
    /// Exclusively creates one private regular file beneath `parent`.
    ///
    /// # Errors
    ///
    /// Returns a typed name, parent-policy, collision, or I/O error.
    pub fn create(parent: &Dir, name: &OsStr) -> Result<Self, PlatformError> {
        let name = PrivateName::parse(name)?;
        let retained_parent = parent.try_clone().map_err(|source| PlatformError::Io {
            operation: "clone_parent",
            source,
        })?;
        let inner = os::create_standalone_file(parent, &name)?;
        Ok(Self {
            inner,
            parent: retained_parent,
        })
    }

    /// Returns the exact identity captured from the retained file handle.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::file_identity(&self.inner)
    }

    /// Flushes content, synchronizes file metadata, and synchronizes the
    /// retained parent entry.
    ///
    /// # Errors
    ///
    /// Returns a typed file or parent synchronization error.
    pub fn commit(mut self) -> Result<(), PlatformError> {
        os::flush_file(&mut self.inner).map_err(|source| PlatformError::Io {
            operation: "flush_file",
            source,
        })?;
        os::sync_file(&self.inner)?;
        os::sync_parent(&self.parent)
    }
}

impl Write for PrivateStandaloneFile {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        os::write_file(&mut self.inner, buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        os::flush_file(&mut self.inner)
    }
}

impl fmt::Debug for PrivateStandaloneFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateStandaloneFile")
            .finish_non_exhaustive()
    }
}

/// A published private directory retained by exact platform identity.
#[must_use = "retain the published directory owner while its exact identity is needed"]
pub struct PublishedPrivateDirectory {
    inner: os::PublishedDirectory,
}

impl PublishedPrivateDirectory {
    /// Returns the exact identity captured before publication.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::published_identity(&self.inner)
    }

    /// Synchronizes the published directory through its retained handle.
    ///
    /// # Errors
    ///
    /// Returns a typed platform or I/O error.
    pub fn sync_all(&self) -> Result<(), PlatformError> {
        os::sync_published_directory(&self.inner)
    }

    /// Recursively removes the published directory through retained handles.
    ///
    /// The exact published identity is removed without reopening a caller-
    /// supplied path, and the retained destination parent is synchronized.
    ///
    /// # Errors
    ///
    /// Returns a typed platform or I/O error.
    pub fn remove(self) -> Result<(), PlatformError> {
        os::remove_published_directory(self.inner)
    }
}

impl fmt::Debug for PrivateFile<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateFile")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for PublishedPrivateDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublishedPrivateDirectory")
            .finish_non_exhaustive()
    }
}

/// Failures returned by the private-tree boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The supplied object name was not one bounded normal component.
    #[error("private-tree name is not one bounded component")]
    InvalidName,
    /// The supplied staging parent did not enforce the account-private policy.
    #[error("private-tree parent is not account-private")]
    InsecureParent,
    /// Retained state was missing or did not satisfy the active policy.
    #[error("private-tree object failed account-private verification")]
    SecurityPolicy,
    /// A bounded private-tree operation exceeded its configured resource limit.
    #[error("private-tree resource limit was exceeded")]
    ResourceLimit,
    /// The native platform boundary has no enabled implementation.
    #[error("private-tree platform boundary is unsupported")]
    UnsupportedPlatform,
    /// A handle-relative filesystem operation failed.
    #[error("private-tree operation {operation} failed")]
    Io {
        /// Stable source-free operation label.
        operation: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: io::Error,
    },
}

impl PlatformError {
    /// Returns whether this error reports an existing destination or child.
    #[must_use]
    pub fn is_already_exists(&self) -> bool {
        matches!(
            self,
            Self::Io { source, .. } if source.kind() == io::ErrorKind::AlreadyExists
        )
    }
}

/// Failures from atomic private-tree publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublishError {
    /// Publication did not commit.
    #[error("private tree was not published")]
    NotCommitted {
        /// Validation, policy, or unsupported-platform cause.
        #[source]
        source: PlatformError,
    },
    /// Publication committed, but destination-directory durability is unknown.
    ///
    #[error("private tree was published but destination durability is unknown")]
    CommittedButDurabilityUnknown {
        /// Owner for the already-published exact directory.
        directory: PublishedPrivateDirectory,
        /// Destination-directory flush failure.
        #[source]
        source: io::Error,
    },
}

impl PublishError {
    fn not_committed(source: PlatformError) -> Self {
        Self::NotCommitted { source }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct PrivateName(OsString);

impl PrivateName {
    fn parse(name: &OsStr) -> Result<Self, PlatformError> {
        let path = Path::new(name);
        let mut components = path.components();
        let Some(Component::Normal(component)) = components.next() else {
            return Err(PlatformError::InvalidName);
        };
        if components.next().is_some()
            || component != name
            || has_name_separator_or_nul(name)
            || platform_name_units(name) > MAX_PRIVATE_NAME_UNITS
        {
            return Err(PlatformError::InvalidName);
        }
        Ok(Self(name.to_os_string()))
    }
}

impl fmt::Debug for PrivateName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PrivateName(<redacted>)")
    }
}

#[cfg(unix)]
fn has_name_separator_or_nul(name: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    name.as_bytes()
        .iter()
        .any(|byte| matches!(byte, b'/' | b'\\' | b'\0'))
}

#[cfg(windows)]
fn has_name_separator_or_nul(name: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;

    name.encode_wide()
        .any(|unit| unit == u16::from(b'/') || unit == u16::from(b'\\') || unit == 0)
}

#[cfg(not(any(unix, windows)))]
fn has_name_separator_or_nul(name: &OsStr) -> bool {
    name.to_string_lossy()
        .chars()
        .any(|character| matches!(character, '/' | '\\' | '\0'))
}

#[cfg(unix)]
fn platform_name_units(name: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    name.as_bytes().len()
}

#[cfg(windows)]
fn platform_name_units(name: &OsStr) -> usize {
    use std::os::windows::ffi::OsStrExt as _;

    name.encode_wide().count()
}

#[cfg(not(any(unix, windows)))]
fn platform_name_units(name: &OsStr) -> usize {
    name.to_string_lossy().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(unix, windows))]
    fn private_parent() -> (tempfile::TempDir, Dir) {
        let temporary = tempfile::tempdir().expect("temporary directory is created");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;

            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o700))
                .expect("temporary parent permissions are private");
        }
        #[cfg(windows)]
        os::protect_test_parent_path(temporary.path()).expect("temporary parent DACL is private");
        let parent = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority())
            .expect("temporary parent opens");
        (temporary, parent)
    }

    #[test]
    fn private_names_are_bounded_single_components() {
        for invalid in ["", ".", "..", "child/name", "child\\name", "child\0name"] {
            assert!(
                PrivateName::parse(OsStr::new(invalid)).is_err(),
                "{invalid}"
            );
        }
        assert!(PrivateName::parse(OsStr::new("result")).is_ok());
        assert!(PrivateName::parse(OsStr::new(&"a".repeat(256))).is_err());
    }

    #[test]
    fn private_handle_debug_output_is_redacted() {
        assert_eq!(
            format!("{:?}", PrivateName::parse(OsStr::new("secret")).unwrap()),
            "PrivateName(<redacted>)"
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn private_tree_writes_syncs_and_publishes_without_replacement() {
        let (temporary, parent) = private_parent();
        let directory =
            PrivateDirectory::create(&parent, OsStr::new("staging")).expect("staging is private");
        let identity = directory.identity();
        {
            let mut file = directory
                .create_file(OsStr::new("evidence.json"))
                .expect("private file is created");
            file.write_all(b"{\"ready\":true}\n")
                .expect("private file writes");
            file.sync_all().expect("private file synchronizes");
        }
        directory
            .sync_all()
            .expect("staging directory synchronizes");
        let published = directory
            .publish_noreplace(&parent, OsStr::new("published"))
            .expect("private tree publishes");

        assert_eq!(published.identity(), identity);
        published
            .sync_all()
            .expect("published directory synchronizes");
        let reopened = PrivateDirectory::open(&parent, OsStr::new("published"))
            .expect("published directory reopens without following links");
        assert_eq!(reopened.identity(), identity);
        drop(reopened);
        assert!(!temporary.path().join("staging").exists());
        assert_eq!(
            std::fs::read(temporary.path().join("published/evidence.json"))
                .expect("published evidence reads"),
            b"{\"ready\":true}\n"
        );
        published
            .remove()
            .expect("published directory removes through retained handles");
        assert!(!temporary.path().join("published").exists());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn publication_collision_preserves_both_private_trees() {
        let (temporary, parent) = private_parent();
        let source =
            PrivateDirectory::create(&parent, OsStr::new("source")).expect("source is private");
        let destination = PrivateDirectory::create(&parent, OsStr::new("destination"))
            .expect("target is private");

        assert!(matches!(
            source.publish_noreplace(&parent, OsStr::new("destination")),
            Err(PublishError::NotCommitted {
                source: PlatformError::Io { source, .. }
            }) if source.kind() == io::ErrorKind::AlreadyExists
        ));
        assert!(temporary.path().join("source").is_dir());
        assert!(temporary.path().join("destination").is_dir());
        destination.remove().expect("target cleanup succeeds");
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn unpublished_tree_removal_is_recursive_and_identity_bound() {
        let (temporary, parent) = private_parent();
        let directory =
            PrivateDirectory::create(&parent, OsStr::new("staging")).expect("staging is private");
        {
            let child = directory
                .create_directory(OsStr::new("nested"))
                .expect("nested directory is private");
            {
                let mut file = child
                    .create_file(OsStr::new("payload"))
                    .expect("nested file is private");
                file.write_all(b"payload").expect("payload writes");
                file.sync_all().expect("payload synchronizes");
            }
        }
        directory.remove().expect("tree removal succeeds");
        assert!(!temporary.path().join("staging").exists());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn private_file_reads_are_identity_checked_and_bounded() {
        let (_temporary, parent) = private_parent();
        let directory =
            PrivateDirectory::create(&parent, OsStr::new("staging")).expect("staging is private");
        {
            let mut file = directory
                .create_file(OsStr::new("payload"))
                .expect("private file is created");
            file.write_all(b"payload").expect("payload writes");
            file.sync_all().expect("payload synchronizes");
        }

        assert_eq!(
            directory
                .read_file_bounded(OsStr::new("payload"), 7)
                .expect("bounded private file reads"),
            b"payload"
        );
        assert!(matches!(
            directory.read_file_bounded(OsStr::new("payload"), 6),
            Err(PlatformError::ResourceLimit)
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_extended_acl_is_rejected_on_reopen() {
        let (temporary, parent) = private_parent();
        let directory =
            PrivateDirectory::create(&parent, OsStr::new("staging")).expect("staging is private");
        {
            let mut file = directory
                .create_file(OsStr::new("payload"))
                .expect("private file is created");
            file.write_all(b"payload").expect("payload writes");
            file.sync_all().expect("payload synchronizes");
        }

        let status = std::process::Command::new("/bin/chmod")
            .arg("+a")
            .arg("everyone allow read")
            .arg(temporary.path().join("staging/payload"))
            .status()
            .expect("macOS chmod executes");
        assert!(status.success(), "macOS test ACL is installed");
        assert!(matches!(
            directory.read_file_bounded(OsStr::new("payload"), 7),
            Err(PlatformError::SecurityPolicy)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn insecure_parent_is_rejected_before_child_creation() {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().expect("temporary directory is created");
        std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755))
            .expect("temporary parent permissions change");
        let parent = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority())
            .expect("temporary parent opens");

        assert!(matches!(
            PrivateDirectory::create(&parent, OsStr::new("staging")),
            Err(PlatformError::InsecureParent)
        ));
        assert!(!temporary.path().join("staging").exists());
    }

    #[cfg(not(any(unix, windows)))]
    #[test]
    fn unsupported_boundary_fails_before_creating_an_entry() {
        let temporary = tempfile::tempdir().expect("temporary directory is created");
        let parent = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority())
            .expect("temporary parent opens");

        assert!(matches!(
            PrivateDirectory::create(&parent, OsStr::new("staging")),
            Err(PlatformError::UnsupportedPlatform)
        ));
        assert!(!temporary.path().join("staging").exists());
        assert!(matches!(
            PrivateDirectory::require_supported(),
            Err(PlatformError::UnsupportedPlatform)
        ));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn support_preflight_succeeds_without_filesystem_input() {
        PrivateDirectory::require_supported().expect("platform boundary is enabled");
    }
}
