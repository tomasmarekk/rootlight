//! Account-private filesystem trees backed by stable operating-system handles.
//!
//! The public surface never exposes raw handles or paths. Platform-specific
//! code owns creation, identity, publication, and cleanup as one boundary.

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

/// Exact identity of an object on one mounted filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlatformFileIdentity {
    volume: u64,
    file: u128,
}

impl PlatformFileIdentity {
    /// Returns the platform volume or device identity.
    #[must_use]
    pub const fn volume(self) -> u64 {
        self.volume
    }

    /// Returns the complete platform file identity.
    #[must_use]
    pub const fn file(self) -> u128 {
        self.file
    }

    #[cfg(all(unix, not(target_vendor = "apple")))]
    const fn new(volume: u64, file: u128) -> Self {
        Self { volume, file }
    }
}

/// A private directory tree that has not yet been published.
///
/// Dropping this value attempts handle-bound recursive cleanup. Call
/// [`PrivateDirectory::remove`] when cleanup errors must be observed.
pub struct PrivateDirectory<'parent> {
    inner: Option<os::Directory>,
    // A live child prevents its parent from being published or removed.
    parent: PhantomData<&'parent ()>,
}

impl PrivateDirectory<'static> {
    /// Creates an empty account-private directory beneath an opened parent.
    ///
    /// On Unix, the supplied parent must itself be owned by the effective user
    /// and deny group and other access. This protects the source entry for
    /// handle-relative publication APIs that still name that entry.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::InvalidName`] for a non-component name,
    /// [`PlatformError::InsecureParent`] when the parent cannot protect the
    /// staging entry, [`PlatformError::UnsupportedPlatform`] when the platform
    /// boundary is not implemented, or [`PlatformError::Io`] for filesystem
    /// failures.
    pub fn create(parent: &Dir, name: &OsStr) -> Result<Self, PlatformError> {
        let name = PrivateName::parse(name)?;
        os::create_directory(parent, &name).map(|inner| Self {
            inner: Some(inner),
            parent: PhantomData,
        })
    }
}

impl<'parent> PrivateDirectory<'parent> {
    /// Creates an empty private child directory.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, policy, unsupported-platform, or filesystem
    /// error without exposing an unverified child handle.
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

    /// Creates a new private regular file without following links.
    ///
    /// The returned file is policy-verified before callers can write bytes.
    ///
    /// # Errors
    ///
    /// Returns a typed validation, policy, unsupported-platform, or filesystem
    /// error without exposing an unverified file handle.
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

    /// Returns the exact identity read from the retained directory handle.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::directory_identity(self.inner())
    }

    /// Flushes directory metadata through the retained handle.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Io`] when the platform cannot flush the handle.
    pub fn sync_all(&self) -> Result<(), PlatformError> {
        os::sync_directory(self.inner())
    }

    /// Atomically publishes this tree without replacing an existing entry.
    ///
    /// The destination is addressed relative to the supplied opened parent.
    /// A pre-commit failure removes the still-private source tree. A successful
    /// rename followed by a destination-directory flush failure is represented
    /// separately because the tree is already visible.
    ///
    /// # Errors
    ///
    /// Returns [`PublishError::NotCommitted`] when the rename did not commit, or
    /// [`PublishError::CommittedButDurabilityUnknown`] when the rename committed
    /// but the destination directory could not be flushed.
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
            #[cfg(all(unix, not(target_vendor = "apple")))]
            Err(os::PublishFailure::Committed { directory, source }) => {
                Err(PublishError::CommittedButDurabilityUnknown {
                    directory: PublishedPrivateDirectory { inner: directory },
                    source,
                })
            }
        }
    }

    /// Recursively removes this exact private tree.
    ///
    /// # Errors
    ///
    /// Returns a platform error if handle-bound cleanup cannot complete. The
    /// implementation leaves an orphan rather than deleting a mismatched entry.
    pub fn remove(mut self) -> Result<(), PlatformError> {
        let Some(inner) = self.inner.take() else {
            return Err(PlatformError::SecurityPolicy);
        };
        os::remove_directory(inner)
    }

    fn inner(&self) -> &os::Directory {
        self.inner
            .as_ref()
            .expect("private directory always owns its platform handle")
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
        if let Some(inner) = self.inner.take() {
            let _ = os::remove_directory(inner);
        }
    }
}

/// A private regular file created inside a [`PrivateDirectory`].
pub struct PrivateFile<'parent> {
    inner: os::File,
    // Publication cannot consume the containing tree while this writer exists.
    parent: PhantomData<&'parent ()>,
}

impl PrivateFile<'_> {
    /// Returns the exact identity read from the retained file handle.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::file_identity(&self.inner)
    }

    /// Flushes file data and metadata through the retained handle.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Io`] when the platform cannot flush the handle.
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

/// A successfully published private directory retained by its exact handle.
pub struct PublishedPrivateDirectory {
    inner: os::PublishedDirectory,
}

impl PublishedPrivateDirectory {
    /// Returns the exact identity of the published directory.
    #[must_use]
    pub fn identity(&self) -> PlatformFileIdentity {
        os::published_identity(&self.inner)
    }

    /// Flushes the published directory through its retained handle.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::Io`] when the platform cannot flush the handle.
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

/// Failures returned by private-tree creation and handle operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The supplied object name was not one bounded normal component.
    #[error("private-tree name is not one bounded component")]
    InvalidName,
    /// The supplied staging parent did not enforce the account-private policy.
    #[error("private-tree parent is not account-private")]
    InsecureParent,
    /// The opened object did not satisfy the account-private policy.
    #[error("private-tree object failed account-private verification")]
    SecurityPolicy,
    /// This target has no approved implementation for the required guarantees.
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

    #[cfg(all(unix, not(target_vendor = "apple")))]
    fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}

/// Failures returned while publishing a private tree.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PublishError {
    /// Publication did not commit and the private source was scheduled for cleanup.
    #[error("private tree was not published")]
    NotCommitted {
        /// Validation, policy, or operating-system failure before commit.
        #[source]
        source: PlatformError,
    },
    /// Publication committed, but destination-directory durability is unknown.
    #[error("private tree was published but destination durability is unknown")]
    CommittedButDurabilityUnknown {
        /// Handle for the already-published exact directory.
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

    #[cfg(all(unix, not(target_vendor = "apple")))]
    fn as_os_str(&self) -> &OsStr {
        &self.0
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

    #[cfg(any(windows, target_vendor = "apple"))]
    #[test]
    fn unapproved_native_boundaries_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary directory is created");
        let parent = Dir::open_ambient_dir(temporary.path(), cap_std::ambient_authority())
            .expect("temporary parent opens");

        assert!(matches!(
            PrivateDirectory::create(&parent, OsStr::new("staging")),
            Err(PlatformError::UnsupportedPlatform)
        ));
    }
}
