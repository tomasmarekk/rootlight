//! Proposed account-private filesystem tree types.
//!
//! ADR-026 is not accepted, so the platform boundary remains a zero-mutation
//! scaffold. Every creation, publication, synchronization, and removal
//! operation fails closed on every target.

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

/// Reserved representation for a future exact platform object identity.
///
/// No value is returned while ADR-026 remains proposed.
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

/// An unpublished private-directory owner reserved by ADR-026.
///
/// The proposed implementation is unavailable on every platform. Dropping a
/// value only drops retained Rust owners; it never attempts filesystem cleanup.
///
/// Correctly scoped callers will continue to compile when an accepted
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
    /// Requires an accepted account-private tree implementation.
    ///
    /// This preflight does not inspect a path, acquire randomness, or perform
    /// a filesystem operation. It lets callers fail closed before they touch
    /// ambient state.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::UnsupportedPlatform`] while ADR-026 remains
    /// unaccepted.
    pub fn require_supported() -> Result<(), PlatformError> {
        os::require_support()
    }

    /// Fails closed without inspecting or mutating the supplied parent.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::InvalidName`] for a non-component name.
    /// Every valid name returns [`PlatformError::UnsupportedPlatform`] until
    /// ADR-026 is accepted and implemented.
    pub fn create(parent: &Dir, name: &OsStr) -> Result<Self, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::create_directory(parent, &name).map(|inner| Self {
            inner: Some(inner),
            parent: PhantomData,
        })
    }
}

impl<'parent> PrivateDirectory<'parent> {
    /// Fails closed without creating a child directory.
    ///
    /// # Errors
    ///
    /// Returns a typed name-validation or unsupported-platform error.
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

    /// Fails closed without creating a file.
    ///
    /// # Errors
    ///
    /// Returns a typed name-validation or unsupported-platform error.
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

    /// Returns the reserved identity from the retained scaffold state.
    ///
    /// Safe callers cannot obtain a directory while creation is unsupported.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::directory_identity(self.inner())
    }

    /// Fails closed without performing a filesystem synchronization.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::UnsupportedPlatform`].
    pub fn sync_all(&self) -> Result<(), PlatformError> {
        os::sync_directory(self.inner())
    }

    /// Fails closed without publishing or cleaning up a source tree.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::NotCommitted`] with
    /// [`PlatformError::UnsupportedPlatform`].
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
                self.inner = Some(directory);
                Err(PublishError::NotCommitted { source })
            }
        }
    }

    /// Fails closed without removing a filesystem object.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::UnsupportedPlatform`].
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

/// A private-file owner reserved by ADR-026.
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
    /// Returns the reserved identity from the retained scaffold state.
    ///
    /// Safe callers cannot obtain a file while creation is unsupported.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::file_identity(&self.inner)
    }

    /// Fails closed without performing a filesystem synchronization.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::UnsupportedPlatform`].
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

/// A published-directory owner reserved by ADR-026.
///
/// No value can be produced while publication remains unsupported.
#[must_use = "retain the published directory owner while its exact identity is needed"]
pub struct PublishedPrivateDirectory {
    inner: os::PublishedDirectory,
}

impl PublishedPrivateDirectory {
    /// Returns the reserved identity from the retained scaffold state.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::published_identity(&self.inner)
    }

    /// Fails closed without performing a filesystem synchronization.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::UnsupportedPlatform`].
    pub fn sync_all(&self) -> Result<(), PlatformError> {
        os::sync_published_directory(&self.inner)
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

/// Failures returned by the proposed private-tree boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The supplied object name was not one bounded normal component.
    #[error("private-tree name is not one bounded component")]
    InvalidName,
    /// The supplied staging parent did not enforce the account-private policy.
    #[error("private-tree parent is not account-private")]
    InsecureParent,
    /// Scaffold state was missing or did not satisfy the proposed policy.
    #[error("private-tree object failed account-private verification")]
    SecurityPolicy,
    /// ADR-026 has no accepted platform implementation.
    #[error("private-tree platform boundary is unsupported")]
    UnsupportedPlatform,
    /// A future handle-relative filesystem operation failed.
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

/// Failures reserved for future private-tree publication.
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
    /// The Proposed scaffold never returns this variant.
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

    #[test]
    fn proposed_boundary_fails_closed_without_creating_an_entry() {
        let temporary = tempfile::tempdir().expect("temporary directory is created");
        let parent = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority())
            .expect("temporary parent opens");

        assert!(matches!(
            PrivateDirectory::create(&parent, OsStr::new("staging")),
            Err(PlatformError::UnsupportedPlatform)
        ));
        assert!(!temporary.path().join("staging").exists());
        assert_eq!(
            std::fs::read_dir(temporary.path())
                .expect("temporary directory reads")
                .count(),
            0
        );
    }

    #[test]
    fn support_preflight_fails_without_filesystem_input() {
        assert!(matches!(
            PrivateDirectory::require_supported(),
            Err(PlatformError::UnsupportedPlatform)
        ));
    }
}
