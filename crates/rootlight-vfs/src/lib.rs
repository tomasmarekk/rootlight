//! Capability-confined local directory browsing, repository access, and
//! immutable source snapshots.
//!
//! Local paths are presentation data rather than authorization. Retained
//! directory handles govern browsing, repository paths are untrusted, and every
//! source read verifies file stability.

#![deny(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    fmt,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::Instant,
};

#[cfg(any(unix, windows))]
use cap_fs_ext::OsMetadataExt as _;
use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, File, Metadata, OpenOptions},
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    ContentHash, FileId, FileIdentity, RepositoryId, content_hash as hash_content, derive_file,
};
use rootlight_ir::{
    FilePathLocator, FilePathLocatorEncoding, MAX_FILE_PATH_LOCATOR_COMPONENTS, SourceRef,
};

pub mod platform;

/// Hard ceiling for one VFS source capture, independent of caller configuration.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
const SNAPSHOT_READ_CHUNK_BYTES: usize = 64 * 1024;
/// Maximum number of relative path components accepted by the VFS.
pub const MAX_PATH_COMPONENTS: usize = 256;
/// Maximum platform path bytes accepted by the VFS.
pub const MAX_PATH_BYTES: usize = 32 * 1024;
/// Hard ceiling for entries examined while capturing one browse snapshot.
pub const MAX_BROWSE_DIRECTORY_ENTRIES: usize = 4_096;
/// Maximum number of browse entries returned by one page.
pub const MAX_BROWSE_PAGE_SIZE: usize = 256;
/// Maximum platform bytes accepted for one browse child name.
pub const MAX_BROWSE_CHILD_NAME_BYTES: usize = 4 * 1_024;

/// A validated repository-relative path with platform-stable identity bytes.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RelativePath {
    components: Vec<OsString>,
    display: String,
    identity: Vec<u8>,
}

impl RelativePath {
    /// Parses a non-empty path containing only normal relative components.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::InvalidRelativePath`] for absolute, parent, prefix,
    /// empty, oversized, or separator-aliased paths.
    pub fn parse(path: &Path) -> Result<Self, VfsError> {
        if path.as_os_str().is_empty()
            || path.is_absolute()
            || path
                .as_os_str()
                .to_str()
                .is_some_and(|path| path.contains('\\'))
        {
            return Err(VfsError::InvalidRelativePath);
        }

        let path_bytes = platform_path_byte_len(path.as_os_str()).ok_or(VfsError::PathTooLong {
            maximum: MAX_PATH_BYTES,
        })?;
        if path_bytes > MAX_PATH_BYTES {
            return Err(VfsError::PathTooLong {
                maximum: MAX_PATH_BYTES,
            });
        }

        let mut components = Vec::new();
        let mut display_parts = Vec::new();
        let mut identity = Vec::new();
        for component in path.components() {
            let Component::Normal(component) = component else {
                return Err(VfsError::InvalidRelativePath);
            };
            if component.is_empty() || contains_separator_alias(component) {
                return Err(VfsError::InvalidRelativePath);
            }
            if components.len() >= MAX_PATH_COMPONENTS {
                return Err(VfsError::TooManyPathComponents {
                    maximum: MAX_PATH_COMPONENTS,
                });
            }
            let (display, identity_bytes) = canonical_component(component);
            append_identity_component(&mut identity, &identity_bytes)?;
            display_parts.push(display);
            components.push(component.to_os_string());
        }
        if components.is_empty() {
            return Err(VfsError::InvalidRelativePath);
        }

        Ok(Self {
            components,
            display: display_parts.join("/"),
            identity,
        })
    }

    /// Reconstructs a validated relative path from a lossless IR locator.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] when the locator belongs to another platform,
    /// contains a non-canonical component, or exceeds VFS path bounds.
    pub fn from_locator(locator: &FilePathLocator) -> Result<Self, VfsError> {
        if locator.encoding() != platform_locator_encoding()
            || locator.components().len() > MAX_PATH_COMPONENTS
            || locator.components().len() > MAX_FILE_PATH_LOCATOR_COMPONENTS
        {
            return Err(VfsError::InvalidRelativePath);
        }

        let component_count = locator.components().len();
        let mut components = Vec::new();
        components
            .try_reserve_exact(component_count)
            .map_err(|_| VfsError::MemoryUnavailable)?;
        let mut display_parts = Vec::new();
        display_parts
            .try_reserve_exact(component_count)
            .map_err(|_| VfsError::MemoryUnavailable)?;
        let mut identity = Vec::new();
        let mut path_bytes = 0usize;
        for (index, encoded) in locator.components().iter().enumerate() {
            let raw = decode_lower_hex(encoded)?;
            let component = platform_os_string(raw)?;
            if !is_single_normal_component(&component) {
                return Err(VfsError::InvalidRelativePath);
            }
            let component_bytes =
                platform_path_byte_len(&component).ok_or(VfsError::PathTooLong {
                    maximum: MAX_PATH_BYTES,
                })?;
            if index > 0 {
                path_bytes = path_bytes
                    .checked_add(platform_separator_byte_len())
                    .ok_or(VfsError::PathTooLong {
                        maximum: MAX_PATH_BYTES,
                    })?;
            }
            path_bytes = path_bytes
                .checked_add(component_bytes)
                .ok_or(VfsError::PathTooLong {
                    maximum: MAX_PATH_BYTES,
                })?;
            if path_bytes > MAX_PATH_BYTES {
                return Err(VfsError::PathTooLong {
                    maximum: MAX_PATH_BYTES,
                });
            }
            let (display, identity_bytes) = canonical_component(&component);
            append_identity_component(&mut identity, &identity_bytes)?;
            display_parts.push(display);
            components.push(component);
        }

        Ok(Self {
            components,
            display: display_parts.join("/"),
            identity,
        })
    }

    /// Encodes this path as a lossless producer-neutral IR locator.
    ///
    /// # Panics
    ///
    /// Panics only if this crate's validated path invariants diverge from the
    /// normalized IR locator contract.
    #[must_use]
    pub fn to_locator(&self) -> FilePathLocator {
        let components = self
            .components
            .iter()
            .map(|component| encode_lower_hex(&platform_path_bytes(component)))
            .collect();
        FilePathLocator::new(platform_locator_encoding(), components)
            .expect("validated relative paths produce canonical path locators")
    }

    /// Returns the canonical forward-slash presentation path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.display
    }

    /// Returns the lossless platform identity bytes used for stable file IDs.
    #[must_use]
    pub fn identity_bytes(&self) -> &[u8] {
        &self.identity
    }

    /// Returns the leaf name.
    #[must_use]
    pub fn file_name(&self) -> &OsStr {
        self.components
            .last()
            .map(OsString::as_os_str)
            .unwrap_or_else(|| OsStr::new(""))
    }

    /// Appends one raw platform name and revalidates the complete path.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] when the child would violate path bounds.
    pub fn join_name(&self, name: &OsStr) -> Result<Self, VfsError> {
        if self.components.len() >= MAX_PATH_COMPONENTS || !is_single_normal_component(name) {
            return Err(VfsError::InvalidRelativePath);
        }
        let name_bytes = platform_path_byte_len(name).ok_or(VfsError::PathTooLong {
            maximum: MAX_PATH_BYTES,
        })?;
        if name_bytes > MAX_PATH_BYTES {
            return Err(VfsError::PathTooLong {
                maximum: MAX_PATH_BYTES,
            });
        }
        let mut components = self.components.clone();
        let mut identity = self.identity.clone();
        let (display_name, identity_bytes) = canonical_component(name);
        append_identity_component(&mut identity, &identity_bytes)?;
        if identity.len() > MAX_PATH_BYTES {
            return Err(VfsError::PathTooLong {
                maximum: MAX_PATH_BYTES,
            });
        }
        components.push(name.to_os_string());
        Ok(Self {
            components,
            display: format!("{}/{display_name}", self.display),
            identity,
        })
    }

    fn parent_components(&self) -> &[OsString] {
        &self.components[..self.components.len() - 1]
    }
}

impl fmt::Debug for RelativePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RelativePath")
            .field("component_count", &self.components.len())
            .field("identity_byte_length", &self.identity.len())
            .finish_non_exhaustive()
    }
}

/// Immutable bytes captured from one stable regular file.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    file: FileId,
    path: RelativePath,
    content: Vec<u8>,
    content_hash: ContentHash,
    metadata: SnapshotMetadata,
}

impl SourceSnapshot {
    /// Reconstructs a snapshot from identity-verified persisted source bytes.
    ///
    /// The repository and canonical relative path must derive the expected file
    /// identity, and the bytes must hash to the expected content identity. The
    /// reconstructed metadata intentionally omits live filesystem change
    /// tokens, so it can never authorize metadata-only hash reuse.
    ///
    /// # Errors
    ///
    /// Returns a typed size or persisted-identity mismatch.
    pub fn from_persisted(
        repository: RepositoryId,
        path: RelativePath,
        expected_file: FileId,
        expected_content_hash: ContentHash,
        content: Vec<u8>,
    ) -> Result<Self, VfsError> {
        let length = u64::try_from(content.len()).unwrap_or(u64::MAX);
        if length > MAX_SNAPSHOT_BYTES {
            return Err(VfsError::FileTooLarge {
                maximum: MAX_SNAPSHOT_BYTES,
            });
        }
        let file = derive_file(FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        if file != expected_file {
            return Err(VfsError::PersistedFileIdentityMismatch);
        }
        let content_hash = hash_content(&content);
        if content_hash != expected_content_hash {
            return Err(VfsError::PersistedContentHashMismatch);
        }
        Ok(Self {
            file,
            path,
            content,
            content_hash,
            metadata: SnapshotMetadata {
                length,
                modified_ns: None,
                change_token: None,
                volume: None,
                file_index: None,
            },
        })
    }

    /// Returns the stable repository-scoped file identity.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the canonical repository-relative path.
    #[must_use]
    pub const fn path(&self) -> &RelativePath {
        &self.path
    }

    /// Returns the captured source bytes.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// Returns the authoritative hash of the captured bytes.
    #[must_use]
    pub const fn content_hash(&self) -> ContentHash {
        self.content_hash
    }

    /// Returns source-free metadata used for reconciliation decisions.
    #[must_use]
    pub const fn metadata(&self) -> SnapshotMetadata {
        self.metadata
    }
}

impl fmt::Debug for SourceSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceSnapshot")
            .field("file", &self.file)
            .field("byte_length", &self.content.len())
            .field("content_hash", &self.content_hash)
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

/// Source-free metadata retained with a source snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotMetadata {
    /// File length observed on the opened handle.
    pub length: u64,
    /// Last modification time in nanoseconds since the Unix epoch, when available.
    pub modified_ns: Option<u128>,
    /// Additional platform change-detection token, when safely available.
    ///
    /// Unix uses the status-change timestamp. Platforms without an equivalent
    /// handle-derived change token leave this absent and force content hashing.
    pub change_token: Option<u128>,
    /// Platform volume/device identity, when exposed safely.
    pub volume: Option<u64>,
    /// Platform file identity, when exposed safely.
    pub file_index: Option<u64>,
}

impl SnapshotMetadata {
    /// Reports whether every field required for metadata-only hash reuse exists.
    ///
    /// This checks shape, not provenance. Only metadata emitted by an opened
    /// [`RepositoryRoot`] handle can be used as a trust attestation.
    #[must_use]
    pub const fn supports_hash_reuse(self) -> bool {
        self.modified_ns.is_some()
            && self.change_token.is_some()
            && self.volume.is_some()
            && self.file_index.is_some()
    }
}

/// A repository-independent capability for one securely opened directory.
///
/// The retained handle, not [`Self::local_path`], authorizes browsing. Child
/// navigation always opens one validated name relative to this handle without
/// following symbolic links or Windows reparse points.
pub struct BrowseDirectory {
    local_path: PathBuf,
    directory: Dir,
}

impl BrowseDirectory {
    /// Opens an absolute directory path without following linked components.
    ///
    /// The supplied path is retained only for local presentation and later
    /// repository submission. Callers must not treat it as authorization for
    /// filesystem access.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] when the path is invalid or oversized,
    /// cancellation wins, or a stable directory capability cannot be opened.
    pub fn open(path: &Path, cancellation: &Cancellation) -> Result<Self, BrowseError> {
        validate_browse_root_path(path)?;
        browse_check(cancellation)?;
        let local_path =
            std::path::absolute(path).map_err(|source| BrowseError::OpenRoot { source })?;
        browse_check(cancellation)?;
        let directory = open_browse_absolute_directory(&local_path, cancellation)?;
        browse_check(cancellation)?;
        Ok(Self {
            local_path,
            directory,
        })
    }

    /// Opens exactly one child directory relative to the retained capability.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError`] when the child name is invalid or oversized,
    /// cancellation wins, or the child is linked, replaced, unreadable, or not
    /// a directory.
    pub fn open_child(
        &self,
        name: &OsStr,
        cancellation: &Cancellation,
    ) -> Result<Self, BrowseError> {
        validate_browse_child_name(name)?;
        let local_path = self.local_path.join(name);
        validate_browse_path_length(&local_path)?;
        let directory = browse_controlled(cancellation, || {
            self.directory
                .open_dir_nofollow(name)
                .map_err(|source| BrowseError::OpenChild { source })
        })?;
        let metadata = browse_controlled(cancellation, || {
            directory
                .dir_metadata()
                .map_err(|source| BrowseError::OpenChild { source })
        })?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(BrowseError::ChildNotDirectory);
        }
        Ok(Self {
            local_path,
            directory,
        })
    }

    /// Returns the local path for presentation or repository submission.
    ///
    /// This value is not an authorization capability. A later repository
    /// submission must reopen and revalidate it through [`RepositoryRoot`].
    #[must_use]
    pub fn local_path(&self) -> &Path {
        &self.local_path
    }

    /// Captures an immutable directories-only snapshot under the hard ceiling.
    ///
    /// Every directory entry is counted toward the ceiling, including files
    /// and links that are omitted from the returned snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError::EntryLimitExceeded`] instead of returning a
    /// misleading partial global order. Cancellation and deadline expiry are
    /// checked around filesystem operations and enumeration steps.
    pub fn snapshot(
        &self,
        cancellation: &Cancellation,
    ) -> Result<BrowseDirectorySnapshot, BrowseError> {
        self.snapshot_with_limit(MAX_BROWSE_DIRECTORY_ENTRIES, cancellation)
    }

    /// Captures an immutable directories-only snapshot under a narrower limit.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError::InvalidEntryLimit`] for zero or values above the
    /// hard ceiling, [`BrowseError::EntryLimitExceeded`] when enumeration
    /// crosses the accepted limit, and typed cancellation or filesystem
    /// failures for interrupted or unreadable directories.
    pub fn snapshot_with_limit(
        &self,
        maximum_entries: usize,
        cancellation: &Cancellation,
    ) -> Result<BrowseDirectorySnapshot, BrowseError> {
        if !(1..=MAX_BROWSE_DIRECTORY_ENTRIES).contains(&maximum_entries) {
            return Err(BrowseError::InvalidEntryLimit {
                maximum: MAX_BROWSE_DIRECTORY_ENTRIES,
            });
        }

        let directory = browse_controlled(cancellation, || {
            self.directory
                .try_clone()
                .map_err(|source| BrowseError::ReadDirectory { source })
        })?;
        let read_directory = browse_controlled(cancellation, || {
            directory
                .entries()
                .map_err(|source| BrowseError::ReadDirectory { source })
        })?;
        let mut examined_entries = 0usize;
        let mut entries = Vec::new();
        for result in read_directory {
            let entry = browse_controlled(cancellation, || {
                result.map_err(|source| BrowseError::ReadDirectory { source })
            })?;
            let name = entry.file_name();
            if name == OsStr::new(".") || name == OsStr::new("..") {
                continue;
            }
            examined_entries =
                examined_entries
                    .checked_add(1)
                    .ok_or(BrowseError::EntryLimitExceeded {
                        maximum: maximum_entries,
                    })?;
            if examined_entries > maximum_entries {
                return Err(BrowseError::EntryLimitExceeded {
                    maximum: maximum_entries,
                });
            }
            validate_browse_child_name(&name)?;
            let file_type = browse_controlled(cancellation, || {
                entry
                    .file_type()
                    .map_err(|source| BrowseError::ReadDirectory { source })
            })?;
            let metadata = browse_controlled(cancellation, || {
                entry
                    .metadata()
                    .map_err(|source| BrowseError::ReadDirectory { source })
            })?;
            if file_type.is_symlink()
                || !file_type.is_dir()
                || !metadata.is_dir()
                || is_reparse_point(&metadata)
            {
                continue;
            }
            browse_controlled(cancellation, || {
                entries
                    .try_reserve(1)
                    .map_err(|_| BrowseError::MemoryUnavailable)
            })?;
            let (display_name, sort_key) = canonical_component(&name);
            entries.push(BrowseDirectoryEntry {
                name,
                display_name,
                sort_key,
            });
        }
        browse_check(cancellation)?;
        entries.sort_by(|left, right| left.sort_key.cmp(&right.sort_key));
        browse_check(cancellation)?;
        Ok(BrowseDirectorySnapshot { entries })
    }
}

impl fmt::Debug for BrowseDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowseDirectory")
            .finish_non_exhaustive()
    }
}

/// One validated directory name retained by a browse snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct BrowseDirectoryEntry {
    name: OsString,
    display_name: String,
    sort_key: Vec<u8>,
}

impl BrowseDirectoryEntry {
    /// Returns the exact platform child name for handle-relative navigation.
    #[must_use]
    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// Returns a stable UTF-8 presentation of the child name.
    ///
    /// Platform names that are not valid UTF-8 use the VFS raw-byte label
    /// encoding and must still be navigated through [`Self::name`].
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

impl fmt::Debug for BrowseDirectoryEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowseDirectoryEntry")
            .finish_non_exhaustive()
    }
}

/// An immutable, globally sorted directories-only browse snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct BrowseDirectorySnapshot {
    entries: Vec<BrowseDirectoryEntry>,
}

impl BrowseDirectorySnapshot {
    /// Returns the number of directories retained by this snapshot.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether this snapshot contains no directories.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns one bounded page without re-enumerating the filesystem.
    #[must_use]
    pub fn page(
        &self,
        offset: BrowsePageOffset,
        page_size: BrowsePageSize,
    ) -> BrowseDirectoryPage<'_> {
        let start = offset.get().min(self.entries.len());
        let end = start
            .checked_add(page_size.get())
            .unwrap_or(self.entries.len())
            .min(self.entries.len());
        let entries = self.entries.get(start..end).unwrap_or(&[]);
        let next_offset = (end < self.entries.len()).then_some(BrowsePageOffset(end));
        BrowseDirectoryPage {
            entries,
            next_offset,
        }
    }
}

impl fmt::Debug for BrowseDirectorySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowseDirectorySnapshot")
            .field("entry_count", &self.entries.len())
            .finish()
    }
}

/// A checked maximum number of browse entries returned by one page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowsePageSize(usize);

impl BrowsePageSize {
    /// Validates a nonzero browse page size against the hard ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError::InvalidPageSize`] for zero or values above
    /// [`MAX_BROWSE_PAGE_SIZE`].
    pub const fn new(value: usize) -> Result<Self, BrowseError> {
        if value == 0 || value > MAX_BROWSE_PAGE_SIZE {
            return Err(BrowseError::InvalidPageSize {
                maximum: MAX_BROWSE_PAGE_SIZE,
            });
        }
        Ok(Self(value))
    }

    /// Returns the validated page size.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// A checked offset into one immutable browse snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BrowsePageOffset(usize);

impl BrowsePageOffset {
    /// Validates a browse offset against the snapshot hard ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`BrowseError::InvalidPageOffset`] for values above
    /// [`MAX_BROWSE_DIRECTORY_ENTRIES`].
    pub const fn new(value: usize) -> Result<Self, BrowseError> {
        if value > MAX_BROWSE_DIRECTORY_ENTRIES {
            return Err(BrowseError::InvalidPageOffset {
                maximum: MAX_BROWSE_DIRECTORY_ENTRIES,
            });
        }
        Ok(Self(value))
    }

    /// Returns the validated snapshot offset.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// One borrowed page from an immutable browse snapshot.
#[derive(Clone, Copy)]
pub struct BrowseDirectoryPage<'snapshot> {
    entries: &'snapshot [BrowseDirectoryEntry],
    next_offset: Option<BrowsePageOffset>,
}

impl BrowseDirectoryPage<'_> {
    /// Returns the sorted directory entries in this page.
    #[must_use]
    pub const fn entries(&self) -> &[BrowseDirectoryEntry] {
        self.entries
    }

    /// Returns the next validated offset when more entries remain.
    #[must_use]
    pub const fn next_offset(&self) -> Option<BrowsePageOffset> {
        self.next_offset
    }
}

impl fmt::Debug for BrowseDirectoryPage<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrowseDirectoryPage")
            .field("entry_count", &self.entries.len())
            .field("next_offset", &self.next_offset)
            .finish()
    }
}

/// A capability handle confining all repository content access.
pub struct RepositoryRoot {
    repository: RepositoryId,
    canonical_path: PathBuf,
    directory: Dir,
}

impl fmt::Debug for RepositoryRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RepositoryRoot")
            .field("repository", &self.repository)
            .finish_non_exhaustive()
    }
}

impl RepositoryRoot {
    /// Opens a repository root and rejects roots reached through symbolic links or
    /// Windows reparse points.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] when the root cannot be opened as a stable directory.
    pub fn open(repository: RepositoryId, path: &Path) -> Result<Self, VfsError> {
        let canonical_path =
            std::path::absolute(path).map_err(|source| VfsError::OpenRoot { source })?;
        let directory = open_absolute_directory(&canonical_path)?;
        Ok(Self {
            repository,
            canonical_path,
            directory,
        })
    }

    /// Returns the stable repository identity associated with this root.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the canonical path for local diagnostics only.
    ///
    /// Public errors and serialized evidence must not include this host path.
    #[must_use]
    pub fn local_path(&self) -> &Path {
        &self.canonical_path
    }

    /// Returns the stable file identity for a validated relative path.
    #[must_use]
    pub fn file_id(&self, path: &RelativePath) -> FileId {
        derive_file(FileIdentity {
            repository: self.repository,
            path_identity: path.identity_bytes(),
        })
        .id()
    }

    /// Enumerates one directory without following a directory-entry link.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError`] for invalid paths, link/reparse entries, or IO errors.
    pub fn read_directory(
        &self,
        directory: Option<&RelativePath>,
    ) -> Result<Vec<DirectoryEntry>, VfsError> {
        let opened = match directory {
            Some(path) => self.open_directory(path)?,
            None => self
                .directory
                .try_clone()
                .map_err(|source| VfsError::ReadDirectory { source })?,
        };
        let mut entries = Vec::new();
        for result in opened
            .entries()
            .map_err(|source| VfsError::ReadDirectory { source })?
        {
            let entry = result.map_err(|source| VfsError::ReadDirectory { source })?;
            let name = entry.file_name();
            if name == OsStr::new(".") || name == OsStr::new("..") {
                continue;
            }
            let file_type = entry
                .file_type()
                .map_err(|source| VfsError::ReadDirectory { source })?;
            let metadata = entry
                .metadata()
                .map_err(|source| VfsError::ReadDirectory { source })?;
            let kind = if file_type.is_symlink() || is_reparse_point(&metadata) {
                EntryKind::Link
            } else if file_type.is_dir() {
                EntryKind::Directory
            } else if file_type.is_file() {
                EntryKind::File
            } else {
                EntryKind::Special
            };
            let source_metadata = if kind == EntryKind::File {
                let mut options = OpenOptions::new();
                options.read(true).follow(FollowSymlinks::No);
                entry
                    .open_with(&options)
                    .and_then(|file| file.metadata())
                    .ok()
                    .filter(|metadata| metadata.is_file() && !is_reparse_point(metadata))
                    .map_or_else(
                        || directory_entry_metadata(&metadata),
                        |metadata| snapshot_metadata(&metadata),
                    )
            } else {
                directory_entry_metadata(&metadata)
            };
            entries.push(DirectoryEntry {
                name,
                kind,
                length: source_metadata.length,
                metadata: source_metadata,
            });
        }
        entries.sort_by(|left, right| {
            platform_path_bytes(&left.name).cmp(&platform_path_bytes(&right.name))
        });
        Ok(entries)
    }

    /// Captures one stable regular file without following links.
    ///
    /// The file is read twice from separately opened handles and accepted only
    /// when identity, metadata, and actual-byte hashes agree. This detects normal
    /// in-place rewrites and atomic replacements without claiming kernel snapshots.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::UnstableFile`] when repeated captures disagree and a
    /// bounded or typed error for invalid, linked, special, or oversized inputs.
    pub fn snapshot(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
    ) -> Result<SourceSnapshot, VfsError> {
        self.snapshot_with_check(path, maximum_bytes, || Ok(()))
    }

    /// Captures one stable regular file under an inherited cancellation token.
    ///
    /// Any monotonic deadline carried by the token is checked before and after
    /// each fallible handle operation, allocation checkpoint, and read chunk.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Cancelled`] when the token's cancellation or
    /// deadline wins, plus the ordinary bounded snapshot errors.
    pub fn snapshot_with_cancellation(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
        cancellation: &Cancellation,
    ) -> Result<SourceSnapshot, VfsError> {
        self.snapshot_with_check(path, maximum_bytes, || {
            cancellation
                .check()
                .map_err(|cancelled| VfsError::Cancelled(cancelled.reason()))
        })
    }

    /// Captures one stable regular file with cooperative cancellation.
    ///
    /// The absolute monotonic deadline is checked before and after every
    /// fallible handle operation, allocation checkpoint, and read chunk in
    /// both captures. An in-flight operating-system operation cannot be
    /// preempted. A stop observed immediately after an operation takes
    /// precedence over that operation's result.
    ///
    /// # Errors
    ///
    /// Returns [`VfsError::Cancelled`] when cancellation or the supplied
    /// deadline wins, plus the ordinary bounded snapshot errors.
    pub fn snapshot_cancellable(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
        cancellation: &Cancellation,
        deadline: Instant,
    ) -> Result<SourceSnapshot, VfsError> {
        self.snapshot_with_check(path, maximum_bytes, || {
            cancellation
                .check()
                .map_err(|cancelled| VfsError::Cancelled(cancelled.reason()))?;
            if Instant::now() >= deadline {
                return Err(VfsError::Cancelled(CancellationReason::DeadlineExceeded));
            }
            Ok(())
        })
    }

    fn snapshot_with_check(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
        mut check: impl FnMut() -> Result<(), VfsError>,
    ) -> Result<SourceSnapshot, VfsError> {
        let maximum_bytes = maximum_bytes.min(MAX_SNAPSHOT_BYTES);
        if maximum_bytes == 0 {
            return Err(VfsError::InvalidByteLimit);
        }
        check()?;
        let Capture {
            content: _,
            hash: first_hash,
            metadata: first_metadata,
        } = self.capture(path, maximum_bytes, &mut check)?;
        check()?;
        let second = self.capture(path, maximum_bytes, &mut check)?;
        check()?;
        let result = self.finish_snapshot(path, first_hash, first_metadata, second);
        check()?;
        result
    }

    fn finish_snapshot(
        &self,
        path: &RelativePath,
        first_hash: ContentHash,
        first_metadata: SnapshotMetadata,
        second: Capture,
    ) -> Result<SourceSnapshot, VfsError> {
        if first_metadata != second.metadata || first_hash != second.hash {
            return Err(VfsError::UnstableFile);
        }
        Ok(SourceSnapshot {
            file: self.file_id(path),
            path: path.clone(),
            content: second.content,
            content_hash: second.hash,
            metadata: second.metadata,
        })
    }

    /// Resolves a generation-bound source reference against a supplied path.
    ///
    /// # Errors
    ///
    /// Rejects repository, file, content-hash, or byte-span mismatches and all
    /// ordinary snapshot failures.
    pub fn read_source(
        &self,
        source: &SourceRef,
        path: &RelativePath,
        maximum_bytes: u64,
    ) -> Result<Vec<u8>, VfsError> {
        if source.repository() != self.repository || source.span().file() != self.file_id(path) {
            return Err(VfsError::SourceReferenceMismatch);
        }
        let snapshot = self.snapshot(path, maximum_bytes)?;
        if snapshot.content_hash() != source.content_hash() {
            return Err(VfsError::StaleContentHash);
        }
        let start =
            usize::try_from(source.span().start_byte()).map_err(|_| VfsError::InvalidSourceSpan)?;
        let end =
            usize::try_from(source.span().end_byte()).map_err(|_| VfsError::InvalidSourceSpan)?;
        snapshot
            .content()
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or(VfsError::InvalidSourceSpan)
    }

    fn open_directory(&self, path: &RelativePath) -> Result<Dir, VfsError> {
        let mut directory = self
            .directory
            .try_clone()
            .map_err(|source| VfsError::OpenDirectory { source })?;
        for component in &path.components {
            directory = directory
                .open_dir_nofollow(component)
                .map_err(|source| VfsError::OpenDirectory { source })?;
            let metadata = directory
                .dir_metadata()
                .map_err(|source| VfsError::OpenDirectory { source })?;
            if !metadata.is_dir() || is_reparse_point(&metadata) {
                return Err(VfsError::LinkedPath);
            }
        }
        Ok(directory)
    }

    fn open_parent(
        &self,
        path: &RelativePath,
        check: &mut impl FnMut() -> Result<(), VfsError>,
    ) -> Result<Dir, VfsError> {
        let mut directory = controlled(check, || {
            self.directory
                .try_clone()
                .map_err(|source| VfsError::OpenDirectory { source })
        })?;
        for component in path.parent_components() {
            directory = controlled(check, || {
                directory
                    .open_dir_nofollow(component)
                    .map_err(|source| VfsError::OpenDirectory { source })
            })?;
            let metadata = controlled(check, || {
                directory
                    .dir_metadata()
                    .map_err(|source| VfsError::OpenDirectory { source })
            })?;
            if !metadata.is_dir() || is_reparse_point(&metadata) {
                return Err(VfsError::LinkedPath);
            }
        }
        Ok(directory)
    }

    fn capture(
        &self,
        path: &RelativePath,
        maximum_bytes: u64,
        check: &mut impl FnMut() -> Result<(), VfsError>,
    ) -> Result<Capture, VfsError> {
        let parent = self.open_parent(path, check)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = controlled(check, || {
            parent
                .open_with(path.file_name(), &options)
                .map_err(|source| VfsError::OpenFile { source })
        })?;
        let before = controlled(check, || checked_metadata(&file, maximum_bytes))?;
        let capacity = usize::try_from(before.length).map_err(|_| VfsError::FileTooLarge {
            maximum: maximum_bytes,
        })?;
        let mut content = Vec::new();
        controlled(check, || {
            content
                .try_reserve_exact(capacity)
                .map_err(|_| VfsError::MemoryUnavailable)
        })?;
        let mut hasher = blake3::Hasher::new();
        let read_ceiling = maximum_bytes.saturating_add(1);
        let mut buffer = [0u8; SNAPSHOT_READ_CHUNK_BYTES];
        loop {
            check()?;
            let consumed = u64::try_from(content.len()).unwrap_or(u64::MAX);
            let remaining = read_ceiling.saturating_sub(consumed);
            if remaining == 0 {
                break;
            }
            let admitted = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = controlled(check, || {
                file.read(&mut buffer[..admitted])
                    .map_err(|source| VfsError::ReadFile { source })
            })?;
            if read == 0 {
                break;
            }
            controlled(check, || {
                content
                    .try_reserve(read)
                    .map_err(|_| VfsError::MemoryUnavailable)
            })?;
            content.extend_from_slice(&buffer[..read]);
            hasher.update(&buffer[..read]);
            check()?;
        }
        if u64::try_from(content.len()).unwrap_or(u64::MAX) > maximum_bytes {
            return Err(VfsError::FileTooLarge {
                maximum: maximum_bytes,
            });
        }
        let after = controlled(check, || checked_metadata(&file, maximum_bytes))?;
        if before != after || after.length != u64::try_from(content.len()).unwrap_or(u64::MAX) {
            return Err(VfsError::UnstableFile);
        }
        Ok(Capture {
            hash: ContentHash::from_bytes(*hasher.finalize().as_bytes()),
            content,
            metadata: after,
        })
    }
}

/// One source-free directory entry returned by the VFS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Entry name relative to the enumerated directory.
    pub name: OsString,
    /// Entry type without following links.
    pub kind: EntryKind,
    /// Observed byte length for regular files.
    pub length: u64,
    /// Source-free metadata used by authoritative incremental reconciliation.
    pub metadata: SnapshotMetadata,
}

/// Closed entry classification at the VFS boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A regular source candidate.
    File,
    /// A directory that may be traversed under configured bounds.
    Directory,
    /// A symbolic link, junction, mount point, or other reparse point.
    Link,
    /// A non-regular filesystem object.
    Special,
}

#[derive(Debug)]
struct Capture {
    content: Vec<u8>,
    hash: ContentHash,
    metadata: SnapshotMetadata,
}

fn controlled<T>(
    check: &mut impl FnMut() -> Result<(), VfsError>,
    operation: impl FnOnce() -> Result<T, VfsError>,
) -> Result<T, VfsError> {
    check()?;
    let result = operation();
    check()?;
    result
}

fn checked_metadata(file: &File, maximum_bytes: u64) -> Result<SnapshotMetadata, VfsError> {
    let metadata = file
        .metadata()
        .map_err(|source| VfsError::ReadFile { source })?;
    if is_reparse_point(&metadata) {
        return Err(VfsError::LinkedPath);
    }
    if !metadata.is_file() {
        return Err(VfsError::NotRegularFile);
    }
    if metadata.len() > maximum_bytes {
        return Err(VfsError::FileTooLarge {
            maximum: maximum_bytes,
        });
    }
    Ok(snapshot_metadata(&metadata))
}

fn snapshot_metadata(metadata: &Metadata) -> SnapshotMetadata {
    let modified_ns = metadata_modified_ns(metadata);
    SnapshotMetadata {
        length: metadata.len(),
        modified_ns,
        change_token: metadata_change_token(metadata),
        volume: Some(cap_fs_ext::MetadataExt::dev(metadata)),
        file_index: Some(cap_fs_ext::MetadataExt::ino(metadata)),
    }
}

fn directory_entry_metadata(metadata: &Metadata) -> SnapshotMetadata {
    SnapshotMetadata {
        length: metadata.len(),
        modified_ns: metadata_modified_ns(metadata),
        change_token: metadata_change_token(metadata),
        volume: None,
        file_index: None,
    }
}

fn metadata_modified_ns(metadata: &Metadata) -> Option<u128> {
    metadata.modified().ok().and_then(|modified| {
        modified
            .into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_nanos())
    })
}

#[cfg(unix)]
fn metadata_change_token(metadata: &Metadata) -> Option<u128> {
    let seconds = u128::try_from(metadata.ctime()).ok()?;
    let nanoseconds = u128::try_from(metadata.ctime_nsec()).ok()?;
    seconds.checked_mul(1_000_000_000)?.checked_add(nanoseconds)
}

#[cfg(windows)]
fn metadata_change_token(_metadata: &Metadata) -> Option<u128> {
    // CreationTime is not NTFS ChangeTime, so it cannot attest that an
    // unchanged mtime and size imply unchanged bytes.
    None
}

#[cfg(not(any(unix, windows)))]
fn metadata_change_token(_metadata: &Metadata) -> Option<u128> {
    None
}

#[cfg(windows)]
fn is_reparse_point(metadata: &Metadata) -> bool {
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &Metadata) -> bool {
    false
}

fn validate_browse_root_path(path: &Path) -> Result<(), BrowseError> {
    if !path.is_absolute() || path_contains_nul(path.as_os_str()) {
        return Err(BrowseError::InvalidRootPath);
    }
    validate_browse_path_length(path)?;
    let mut normal_components = 0usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => {
                normal_components = normal_components
                    .checked_add(1)
                    .ok_or(BrowseError::InvalidRootPath)?;
                if normal_components > MAX_PATH_COMPONENTS {
                    return Err(BrowseError::InvalidRootPath);
                }
            }
            Component::Prefix(_) | Component::RootDir => {}
            Component::CurDir | Component::ParentDir => {
                return Err(BrowseError::InvalidRootPath);
            }
        }
    }
    Ok(())
}

fn validate_browse_path_length(path: &Path) -> Result<(), BrowseError> {
    let length = platform_path_byte_len(path.as_os_str()).ok_or(BrowseError::RootPathTooLong {
        maximum: MAX_PATH_BYTES,
    })?;
    if length > MAX_PATH_BYTES {
        return Err(BrowseError::RootPathTooLong {
            maximum: MAX_PATH_BYTES,
        });
    }
    Ok(())
}

fn validate_browse_child_name(name: &OsStr) -> Result<(), BrowseError> {
    let length = platform_path_byte_len(name).ok_or(BrowseError::InvalidChildName {
        maximum: MAX_BROWSE_CHILD_NAME_BYTES,
    })?;
    if !is_single_normal_component(name)
        || path_contains_nul(name)
        || length > MAX_BROWSE_CHILD_NAME_BYTES
    {
        return Err(BrowseError::InvalidChildName {
            maximum: MAX_BROWSE_CHILD_NAME_BYTES,
        });
    }
    Ok(())
}

#[cfg(unix)]
fn path_contains_nul(value: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().contains(&0)
}

#[cfg(windows)]
fn path_contains_nul(value: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;

    value.encode_wide().any(|unit| unit == 0)
}

fn browse_check(cancellation: &Cancellation) -> Result<(), BrowseError> {
    cancellation
        .check()
        .map_err(|cancelled| BrowseError::Cancelled(cancelled.reason()))
}

fn browse_controlled<T>(
    cancellation: &Cancellation,
    operation: impl FnOnce() -> Result<T, BrowseError>,
) -> Result<T, BrowseError> {
    browse_check(cancellation)?;
    let result = operation();
    browse_check(cancellation)?;
    result
}

fn open_browse_absolute_directory(
    path: &Path,
    cancellation: &Cancellation,
) -> Result<Dir, BrowseError> {
    let mut anchor = PathBuf::new();
    let mut relative = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir if relative.is_empty() => {
                anchor.push(component.as_os_str());
            }
            Component::Normal(component) => {
                relative
                    .try_reserve(1)
                    .map_err(|_| BrowseError::MemoryUnavailable)?;
                relative.push(component.to_os_string());
            }
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(BrowseError::InvalidRootPath);
            }
        }
    }
    if anchor.as_os_str().is_empty() {
        return Err(BrowseError::InvalidRootPath);
    }

    let mut directory = browse_controlled(cancellation, || {
        Dir::open_ambient_dir(anchor, ambient_authority())
            .map_err(|source| BrowseError::OpenRoot { source })
    })?;
    for component in relative {
        directory = browse_controlled(cancellation, || {
            directory
                .open_dir_nofollow(component)
                .map_err(|source| BrowseError::OpenRoot { source })
        })?;
        let metadata = browse_controlled(cancellation, || {
            directory
                .dir_metadata()
                .map_err(|source| BrowseError::OpenRoot { source })
        })?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(BrowseError::RootNotDirectory);
        }
    }
    Ok(directory)
}

fn open_absolute_directory(path: &Path) -> Result<Dir, VfsError> {
    let mut anchor = PathBuf::new();
    let mut relative = Vec::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir if relative.is_empty() => {
                anchor.push(component.as_os_str());
            }
            Component::Normal(component) => relative.push(component.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(VfsError::InvalidRootPath);
            }
        }
    }
    if anchor.as_os_str().is_empty() {
        return Err(VfsError::InvalidRootPath);
    }

    let mut directory = Dir::open_ambient_dir(anchor, ambient_authority())
        .map_err(|source| VfsError::OpenRoot { source })?;
    for component in relative {
        directory = directory
            .open_dir_nofollow(component)
            .map_err(|source| VfsError::OpenRoot { source })?;
        let metadata = directory
            .dir_metadata()
            .map_err(|source| VfsError::OpenRoot { source })?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(VfsError::RootNotDirectory);
        }
    }
    Ok(directory)
}

fn canonical_component(component: &OsStr) -> (String, Vec<u8>) {
    match component.to_str() {
        Some(text) => {
            let mut identity = Vec::with_capacity(text.len().saturating_add(1));
            identity.push(0);
            identity.extend_from_slice(text.as_bytes());
            (text.to_owned(), identity)
        }
        None => {
            let raw = platform_path_bytes(component);
            let mut display = String::from("@raw-");
            for byte in &raw {
                use std::fmt::Write as _;
                let _ = write!(display, "{byte:02x}");
            }
            let mut identity = Vec::with_capacity(raw.len().saturating_add(1));
            identity.push(1);
            identity.extend_from_slice(&raw);
            (display, identity)
        }
    }
}

fn append_identity_component(identity: &mut Vec<u8>, bytes: &[u8]) -> Result<(), VfsError> {
    let length = u32::try_from(bytes.len()).map_err(|_| VfsError::InvalidRelativePath)?;
    identity.extend_from_slice(&length.to_be_bytes());
    identity.extend_from_slice(bytes);
    Ok(())
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn decode_lower_hex(encoded: &str) -> Result<Vec<u8>, VfsError> {
    let mut decoded = Vec::new();
    decoded
        .try_reserve_exact(encoded.len() / 2)
        .map_err(|_| VfsError::MemoryUnavailable)?;
    for pair in encoded.as_bytes().chunks_exact(2) {
        let [high, low] = pair else {
            return Err(VfsError::InvalidRelativePath);
        };
        let high = decode_lower_hex_nibble(*high).ok_or(VfsError::InvalidRelativePath)?;
        let low = decode_lower_hex_nibble(*low).ok_or(VfsError::InvalidRelativePath)?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

const fn decode_lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn is_single_normal_component(name: &OsStr) -> bool {
    if name.is_empty() || contains_separator_alias(name) {
        return false;
    }
    let mut components = Path::new(name).components();
    matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(component)), None) if component == name
    )
}

#[cfg(unix)]
fn contains_separator_alias(component: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    component.as_bytes().contains(&b'\\')
}

#[cfg(windows)]
fn contains_separator_alias(component: &OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;

    component.encode_wide().any(|unit| unit == u16::from(b'\\'))
}

#[cfg(unix)]
fn platform_path_byte_len(value: &OsStr) -> Option<usize> {
    use std::os::unix::ffi::OsStrExt as _;

    Some(value.as_bytes().len())
}

#[cfg(unix)]
const fn platform_separator_byte_len() -> usize {
    1
}

#[cfg(unix)]
const fn platform_locator_encoding() -> FilePathLocatorEncoding {
    FilePathLocatorEncoding::UnixBytesV1
}

#[cfg(unix)]
fn platform_os_string(raw: Vec<u8>) -> Result<OsString, VfsError> {
    use std::os::unix::ffi::OsStringExt as _;

    Ok(OsString::from_vec(raw))
}

#[cfg(windows)]
fn platform_path_byte_len(value: &OsStr) -> Option<usize> {
    use std::os::windows::ffi::OsStrExt as _;

    value.encode_wide().try_fold(0usize, |length, _| {
        length.checked_add(std::mem::size_of::<u16>())
    })
}

#[cfg(windows)]
const fn platform_separator_byte_len() -> usize {
    std::mem::size_of::<u16>()
}

#[cfg(windows)]
const fn platform_locator_encoding() -> FilePathLocatorEncoding {
    FilePathLocatorEncoding::WindowsWideV1
}

#[cfg(windows)]
fn platform_os_string(raw: Vec<u8>) -> Result<OsString, VfsError> {
    use std::os::windows::ffi::OsStringExt as _;

    if !raw.len().is_multiple_of(2) {
        return Err(VfsError::InvalidRelativePath);
    }
    let mut wide = Vec::new();
    wide.try_reserve_exact(raw.len() / 2)
        .map_err(|_| VfsError::MemoryUnavailable)?;
    for pair in raw.chunks_exact(2) {
        let [low, high] = pair else {
            return Err(VfsError::InvalidRelativePath);
        };
        wide.push(u16::from_le_bytes([*low, *high]));
    }
    Ok(OsString::from_wide(&wide))
}

#[cfg(unix)]
fn platform_path_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn platform_path_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;

    value
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

/// Typed failures returned by repository-independent directory browsing.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BrowseError {
    /// The supplied browse root was not an absolute component-safe path.
    #[error("invalid browse root path")]
    InvalidRootPath,
    /// The supplied or derived local path exceeded the hard byte ceiling.
    #[error("browse path exceeds {maximum} bytes")]
    RootPathTooLong {
        /// Maximum permitted platform path bytes.
        maximum: usize,
    },
    /// The browse root could not be opened without following links.
    #[error("failed to open browse root")]
    OpenRoot {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// The opened browse root was not an ordinary directory.
    #[error("browse root is not a regular directory")]
    RootNotDirectory,
    /// The supplied child name was not one bounded normal component.
    #[error("browse child name is not one component within {maximum} bytes")]
    InvalidChildName {
        /// Maximum permitted platform bytes for one child name.
        maximum: usize,
    },
    /// A child could not be opened without following links.
    #[error("failed to open browse child")]
    OpenChild {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// The opened child was linked, replaced, or not an ordinary directory.
    #[error("browse child is not a stable regular directory")]
    ChildNotDirectory,
    /// A browse directory could not be enumerated.
    #[error("failed to enumerate browse directory")]
    ReadDirectory {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// The requested entry ceiling was zero or above the hard maximum.
    #[error("browse entry limit must be between 1 and {maximum}")]
    InvalidEntryLimit {
        /// Maximum permitted entries examined in one snapshot.
        maximum: usize,
    },
    /// Enumeration crossed the accepted entry ceiling.
    #[error("browse directory exceeds {maximum} entries")]
    EntryLimitExceeded {
        /// Maximum entries permitted for this snapshot capture.
        maximum: usize,
    },
    /// The requested page size was zero or above the hard maximum.
    #[error("browse page size must be between 1 and {maximum}")]
    InvalidPageSize {
        /// Maximum permitted entries in one page.
        maximum: usize,
    },
    /// The requested page offset was above the snapshot hard ceiling.
    #[error("browse page offset exceeds {maximum}")]
    InvalidPageOffset {
        /// Maximum permitted snapshot offset.
        maximum: usize,
    },
    /// A bounded browse operation could not reserve admitted memory.
    #[error("browse snapshot memory is unavailable")]
    MemoryUnavailable,
    /// Cooperative cancellation or a monotonic deadline stopped browsing.
    #[error("directory browsing was cancelled: {0:?}")]
    Cancelled(CancellationReason),
}

/// Typed failures returned by the capability-confined VFS.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    /// The supplied repository-relative path was not safe or canonical.
    #[error("invalid repository-relative path")]
    InvalidRelativePath,
    /// The supplied path exceeded the hard byte ceiling.
    #[error("repository-relative path exceeds {maximum} bytes")]
    PathTooLong {
        /// Maximum permitted platform path bytes.
        maximum: usize,
    },
    /// The supplied path exceeded the hard component ceiling.
    #[error("repository-relative path exceeds {maximum} components")]
    TooManyPathComponents {
        /// Maximum permitted relative path components.
        maximum: usize,
    },
    /// The supplied repository root path was not absolute and component-safe.
    #[error("invalid repository root path")]
    InvalidRootPath,
    /// The repository root could not be opened.
    #[error("failed to open repository root")]
    OpenRoot {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// The opened root was not an ordinary directory.
    #[error("repository root is not a regular directory")]
    RootNotDirectory,
    /// A directory component could not be opened without following links.
    #[error("failed to open repository directory")]
    OpenDirectory {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// A directory could not be enumerated.
    #[error("failed to enumerate repository directory")]
    ReadDirectory {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// A source file could not be opened without following links.
    #[error("failed to open repository file")]
    OpenFile {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// A source file could not be read completely.
    #[error("failed to read repository file")]
    ReadFile {
        /// Underlying capability filesystem error.
        #[source]
        source: io::Error,
    },
    /// The selected path contains a link, junction, or reparse point.
    #[error("repository path contains a link or reparse point")]
    LinkedPath,
    /// The selected entry is not a regular file.
    #[error("repository entry is not a regular file")]
    NotRegularFile,
    /// The source file exceeds the configured byte ceiling.
    #[error("repository file exceeds {maximum} bytes")]
    FileTooLarge {
        /// Maximum permitted source bytes.
        maximum: u64,
    },
    /// A zero-byte capture ceiling was supplied.
    #[error("source byte limit must be positive")]
    InvalidByteLimit,
    /// Repeated bounded captures observed different file state.
    #[error("repository file changed during snapshot capture")]
    UnstableFile,
    /// A bounded source capture could not reserve its admitted memory.
    #[error("repository snapshot memory is unavailable")]
    MemoryUnavailable,
    /// Cooperative cancellation or a monotonic deadline stopped the capture.
    #[error("repository snapshot was cancelled: {0:?}")]
    Cancelled(CancellationReason),
    /// The source reference does not identify this repository or file.
    #[error("source reference does not match repository path")]
    SourceReferenceMismatch,
    /// The source reference names an older content hash.
    #[error("source reference content hash is stale")]
    StaleContentHash,
    /// The source reference span lies outside the captured bytes.
    #[error("source reference span is outside captured content")]
    InvalidSourceSpan,
    /// Persisted path and repository inputs did not derive the expected file.
    #[error("persisted source file identity does not match its canonical path")]
    PersistedFileIdentityMismatch,
    /// Persisted bytes did not match their recorded content hash.
    #[error("persisted source content hash does not match its recorded identity")]
    PersistedContentHashMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_ir::SourceSpan;
    use std::{cell::Cell, fs, time::Duration};
    use tempfile::tempdir_in;

    fn local_tempdir() -> tempfile::TempDir {
        let current = std::env::current_dir().expect("current directory is available");
        tempdir_in(current).expect("local temporary directory is available")
    }

    fn fixture() -> (tempfile::TempDir, RepositoryRoot) {
        let temporary = local_tempdir();
        let repository = derive_repository(b"vfs-test").id();
        let root = RepositoryRoot::open(repository, temporary.path())
            .expect("temporary directory is a valid repository root");
        (temporary, root)
    }

    fn capture(root: &RepositoryRoot, path: &RelativePath) -> Capture {
        let mut check = || Ok(());
        root.capture(path, MAX_SNAPSHOT_BYTES, &mut check)
            .expect("fixture capture succeeds")
    }

    fn browse_deadline() -> Cancellation {
        Cancellation::with_deadline(
            Instant::now()
                .checked_add(Duration::from_secs(30))
                .expect("test deadline is representable"),
        )
    }

    fn browse_fixture() -> (tempfile::TempDir, BrowseDirectory) {
        let temporary = local_tempdir();
        let directory = BrowseDirectory::open(temporary.path(), &browse_deadline())
            .expect("temporary directory is a valid browse root");
        (temporary, directory)
    }

    #[test]
    fn browse_snapshots_are_immutable_sorted_and_stably_paged() {
        let (temporary, directory) = browse_fixture();
        for name in ["zeta", "alpha", "middle"] {
            fs::create_dir(temporary.path().join(name)).expect("fixture directory is created");
        }
        fs::write(temporary.path().join("ignored.rs"), b"source").expect("fixture file is created");

        let snapshot = directory
            .snapshot(&browse_deadline())
            .expect("browse snapshot succeeds");
        fs::create_dir(temporary.path().join("after-snapshot"))
            .expect("post-snapshot directory is created");
        let page_size = BrowsePageSize::new(2).expect("fixture page size is valid");
        let first = snapshot.page(
            BrowsePageOffset::new(0).expect("initial offset is valid"),
            page_size,
        );
        let first_names = first
            .entries()
            .iter()
            .map(BrowseDirectoryEntry::display_name)
            .collect::<Vec<_>>();

        assert_eq!(snapshot.len(), 3);
        assert_eq!(first_names, ["alpha", "middle"]);
        assert_eq!(
            first.next_offset(),
            Some(BrowsePageOffset::new(2).expect("continuation offset is valid"))
        );
        let second = snapshot.page(
            first.next_offset().expect("first page has a continuation"),
            page_size,
        );
        assert_eq!(
            second
                .entries()
                .iter()
                .map(BrowseDirectoryEntry::display_name)
                .collect::<Vec<_>>(),
            ["zeta"]
        );
        assert!(second.next_offset().is_none());
    }

    #[test]
    fn browse_page_bounds_fail_closed() {
        assert!(matches!(
            BrowsePageSize::new(0),
            Err(BrowseError::InvalidPageSize {
                maximum: MAX_BROWSE_PAGE_SIZE
            })
        ));
        assert!(matches!(
            BrowsePageSize::new(MAX_BROWSE_PAGE_SIZE + 1),
            Err(BrowseError::InvalidPageSize {
                maximum: MAX_BROWSE_PAGE_SIZE
            })
        ));
        assert!(matches!(
            BrowsePageOffset::new(MAX_BROWSE_DIRECTORY_ENTRIES + 1),
            Err(BrowseError::InvalidPageOffset {
                maximum: MAX_BROWSE_DIRECTORY_ENTRIES
            })
        ));
    }

    #[test]
    fn browse_snapshots_reject_overflow_instead_of_returning_a_partial_order() {
        let (temporary, directory) = browse_fixture();
        for name in ["one", "two", "three"] {
            fs::create_dir(temporary.path().join(name)).expect("fixture directory is created");
        }

        assert!(matches!(
            directory.snapshot_with_limit(2, &browse_deadline()),
            Err(BrowseError::EntryLimitExceeded { maximum: 2 })
        ));
        for invalid in [0, MAX_BROWSE_DIRECTORY_ENTRIES + 1] {
            assert!(matches!(
                directory.snapshot_with_limit(invalid, &browse_deadline()),
                Err(BrowseError::InvalidEntryLimit {
                    maximum: MAX_BROWSE_DIRECTORY_ENTRIES
                })
            ));
        }
    }

    #[test]
    fn browse_snapshots_filter_non_directories() {
        let (temporary, directory) = browse_fixture();
        fs::write(temporary.path().join("file"), b"content").expect("fixture file is created");

        assert!(
            directory
                .snapshot(&browse_deadline())
                .expect("browse snapshot succeeds")
                .is_empty()
        );
        assert!(
            directory
                .open_child(OsStr::new("file"), &browse_deadline())
                .is_err()
        );
    }

    #[test]
    fn browse_child_names_accept_exactly_one_bounded_component() {
        let (temporary, directory) = browse_fixture();
        fs::create_dir(temporary.path().join("child")).expect("fixture directory is created");
        let child = directory
            .open_child(OsStr::new("child"), &browse_deadline())
            .expect("ordinary child opens");

        assert_eq!(child.local_path(), temporary.path().join("child"));
        for name in ["", ".", "..", "/", "nested/child", "nested\\child", "\0"] {
            assert!(
                matches!(
                    directory.open_child(OsStr::new(name), &browse_deadline()),
                    Err(BrowseError::InvalidChildName {
                        maximum: MAX_BROWSE_CHILD_NAME_BYTES
                    })
                ),
                "{name:?}"
            );
        }
    }

    #[test]
    fn browse_roots_require_absolute_component_safe_paths() {
        assert!(matches!(
            BrowseDirectory::open(Path::new("."), &browse_deadline()),
            Err(BrowseError::InvalidRootPath)
        ));
        assert!(matches!(
            BrowseDirectory::open(Path::new("../parent"), &browse_deadline()),
            Err(BrowseError::InvalidRootPath)
        ));
    }

    #[test]
    fn browse_operations_observe_cancellation_and_deadlines() {
        let (temporary, directory) = browse_fixture();
        let cancelled = Cancellation::new();
        assert!(cancelled.cancel(CancellationReason::ClientRequest));

        assert!(matches!(
            directory.snapshot(&cancelled),
            Err(BrowseError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert!(matches!(
            BrowseDirectory::open(temporary.path(), &cancelled),
            Err(BrowseError::Cancelled(CancellationReason::ClientRequest))
        ));

        let now = Instant::now();
        let expired =
            Cancellation::with_deadline(now.checked_sub(Duration::from_nanos(1)).unwrap_or(now));
        assert!(matches!(
            directory.snapshot(&expired),
            Err(BrowseError::Cancelled(CancellationReason::DeadlineExceeded))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn browse_roots_and_children_reject_symbolic_links() {
        use std::os::unix::fs::symlink;

        let base = local_tempdir();
        let target = local_tempdir();
        let linked_root = base.path().join("linked");
        symlink(target.path(), &linked_root).expect("directory symlink is created");
        let cancellation = browse_deadline();

        assert!(BrowseDirectory::open(&linked_root, &cancellation).is_err());
        let base_directory =
            BrowseDirectory::open(base.path(), &cancellation).expect("ordinary browse root opens");
        assert!(
            base_directory
                .snapshot(&cancellation)
                .expect("browse snapshot succeeds")
                .is_empty()
        );
        assert!(
            base_directory
                .open_child(OsStr::new("linked"), &cancellation)
                .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn browse_roots_and_children_reject_windows_reparse_points() {
        use std::os::windows::fs::symlink_dir;

        const ERROR_PRIVILEGE_NOT_HELD: i32 = 1_314;

        let base = local_tempdir();
        let target = local_tempdir();
        let linked_root = base.path().join("linked");
        match symlink_dir(target.path(), &linked_root) {
            Ok(()) => {}
            Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => return,
            Err(error) => panic!("directory reparse point is created: {error}"),
        }
        let cancellation = browse_deadline();

        assert!(BrowseDirectory::open(&linked_root, &cancellation).is_err());
        let base_directory =
            BrowseDirectory::open(base.path(), &cancellation).expect("ordinary browse root opens");
        assert!(
            base_directory
                .snapshot(&cancellation)
                .expect("browse snapshot succeeds")
                .is_empty()
        );
        assert!(
            base_directory
                .open_child(OsStr::new("linked"), &cancellation)
                .is_err()
        );
    }

    #[test]
    fn relative_paths_reject_escape_and_alias_forms() {
        for path in ["", ".", "..", "a/../b", "/absolute", "a\\b"] {
            assert!(RelativePath::parse(Path::new(path)).is_err(), "{path}");
        }
        assert_eq!(
            RelativePath::parse(Path::new("src/lib.rs"))
                .expect("ordinary path is accepted")
                .as_str(),
            "src/lib.rs"
        );
    }

    #[test]
    fn persisted_snapshots_reverify_file_and_content_identity() {
        let repository = derive_repository(b"persisted-snapshot").id();
        let path = RelativePath::parse(Path::new("src/lib.rs")).expect("fixture path is valid");
        let file = derive_file(FileIdentity {
            repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        let content = b"pub fn restored() {}\n".to_vec();
        let expected_hash = hash_content(&content);

        let restored =
            SourceSnapshot::from_persisted(repository, path.clone(), file, expected_hash, content)
                .expect("matching persisted identity restores");
        assert_eq!(restored.file(), file);
        assert_eq!(restored.content_hash(), expected_hash);
        assert!(!restored.metadata().supports_hash_reuse());

        let other_repository = derive_repository(b"other-repository").id();
        assert!(matches!(
            SourceSnapshot::from_persisted(
                other_repository,
                path.clone(),
                file,
                expected_hash,
                Vec::new(),
            ),
            Err(VfsError::PersistedFileIdentityMismatch)
        ));
        assert!(matches!(
            SourceSnapshot::from_persisted(
                repository,
                path,
                file,
                expected_hash,
                b"tampered".to_vec(),
            ),
            Err(VfsError::PersistedContentHashMismatch)
        ));
    }

    #[test]
    fn joined_names_accept_exactly_one_normal_component() {
        let parent = RelativePath::parse(Path::new("src")).expect("fixture parent is valid");
        for name in ["", ".", "..", "/", "nested/file.rs", "nested\\file.rs"] {
            assert!(parent.join_name(OsStr::new(name)).is_err(), "{name}");
        }
        #[cfg(windows)]
        for name in ["C:", "C:/absolute", "//server/share"] {
            assert!(parent.join_name(OsStr::new(name)).is_err(), "{name}");
        }

        let joined = parent
            .join_name(OsStr::new("lib.rs"))
            .expect("one normal component is accepted");
        let parsed =
            RelativePath::parse(Path::new("src/lib.rs")).expect("equivalent path is valid");
        assert_eq!(joined, parsed);

        let (_, root) = fixture();
        assert_eq!(root.file_id(&joined), root.file_id(&parsed));
    }

    #[test]
    fn lossless_locators_preserve_canonical_path_identity() {
        let path = RelativePath::parse(Path::new("src/lib.rs")).expect("fixture path is canonical");
        let reconstructed =
            RelativePath::from_locator(&path.to_locator()).expect("locator reconstructs");

        assert_eq!(reconstructed, path);
        let (_, root) = fixture();
        assert_eq!(root.file_id(&reconstructed), root.file_id(&path));
    }

    #[cfg(unix)]
    #[test]
    fn unix_locators_distinguish_invalid_bytes_from_literal_raw_labels() {
        use std::os::unix::ffi::OsStrExt as _;

        let raw = RelativePath::parse(Path::new(OsStr::from_bytes(b"\xff")))
            .expect("raw Unix component is valid");
        let literal = RelativePath::parse(Path::new("@raw-ff")).expect("literal path is valid");
        assert_eq!(raw.as_str(), literal.as_str());
        assert_ne!(raw.identity_bytes(), literal.identity_bytes());
        assert_eq!(
            RelativePath::from_locator(&raw.to_locator()).expect("raw locator reconstructs"),
            raw
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_locators_distinguish_unpaired_wide_names_from_literal_raw_labels() {
        use std::os::windows::ffi::OsStringExt as _;

        let raw_name = OsString::from_wide(&[0xd800]);
        let raw =
            RelativePath::parse(Path::new(&raw_name)).expect("unpaired wide component is valid");
        let literal = RelativePath::parse(Path::new("@raw-00d8")).expect("literal path is valid");
        assert_eq!(raw.as_str(), literal.as_str());
        assert_ne!(raw.identity_bytes(), literal.identity_bytes());
        assert_eq!(
            RelativePath::from_locator(&raw.to_locator()).expect("wide locator reconstructs"),
            raw
        );
    }

    #[test]
    fn relative_paths_reject_oversized_input_before_canonical_allocation() {
        let oversized = "a".repeat(MAX_PATH_BYTES + 1);

        assert!(matches!(
            RelativePath::parse(Path::new(&oversized)),
            Err(VfsError::PathTooLong {
                maximum: MAX_PATH_BYTES
            })
        ));
    }

    #[test]
    fn debug_output_redacts_repository_paths_and_source() {
        let (temporary, root) = fixture();
        let source = b"do not log repository source";
        fs::write(temporary.path().join("sample.rs"), source).expect("fixture write succeeds");
        let path = RelativePath::parse(Path::new("sample.rs")).expect("fixture path is valid");
        let snapshot = root.snapshot(&path, 1_024).expect("snapshot succeeds");

        let rendered = format!("{root:?} {path:?} {snapshot:?}");
        assert!(!rendered.contains("sample.rs"));
        assert!(!rendered.contains("do not log"));
        assert!(!rendered.contains(&temporary.path().to_string_lossy().into_owned()));
    }

    #[test]
    fn snapshots_hash_actual_bytes_and_detect_same_size_rewrites() {
        let (temporary, root) = fixture();
        fs::write(temporary.path().join("sample.rs"), b"alpha").expect("fixture write succeeds");
        let path = RelativePath::parse(Path::new("sample.rs")).expect("fixture path is valid");
        let first = root.snapshot(&path, 1024).expect("first capture succeeds");
        fs::write(temporary.path().join("sample.rs"), b"bravo").expect("rewrite succeeds");
        let second = root.snapshot(&path, 1024).expect("second capture succeeds");

        assert_ne!(first.content_hash(), second.content_hash());
        assert_eq!(first.metadata().length, second.metadata().length);
    }

    #[test]
    fn repeated_captures_reject_between_capture_in_place_rewrites() {
        let (temporary, root) = fixture();
        let target = temporary.path().join("sample.rs");
        fs::write(&target, b"alpha").expect("fixture write succeeds");
        let path = RelativePath::parse(Path::new("sample.rs")).expect("fixture path is valid");
        let Capture {
            content: _,
            hash: first_hash,
            metadata: first_metadata,
        } = capture(&root, &path);

        fs::write(&target, b"bravo").expect("same-size rewrite succeeds");
        let second = capture(&root, &path);

        assert!(matches!(
            root.finish_snapshot(&path, first_hash, first_metadata, second),
            Err(VfsError::UnstableFile)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn repeated_captures_reject_between_capture_atomic_replacements() {
        let (temporary, root) = fixture();
        let target = temporary.path().join("sample.rs");
        let replacement = temporary.path().join("replacement.rs");
        fs::write(&target, b"alpha").expect("fixture write succeeds");
        let path = RelativePath::parse(Path::new("sample.rs")).expect("fixture path is valid");
        let Capture {
            content: _,
            hash: first_hash,
            metadata: first_metadata,
        } = capture(&root, &path);

        fs::write(&replacement, b"alpha").expect("replacement fixture is written");
        fs::rename(replacement, target).expect("atomic replacement succeeds");
        let second = capture(&root, &path);

        assert!(matches!(
            root.finish_snapshot(&path, first_hash, first_metadata, second),
            Err(VfsError::UnstableFile)
        ));
    }

    #[test]
    fn snapshots_enforce_the_hard_source_file_ceiling() {
        let (temporary, root) = fixture();
        let fixture_path = temporary.path().join("oversized.rs");
        let fixture_file = fs::File::create(&fixture_path).expect("fixture file is created");
        fixture_file
            .set_len(MAX_SNAPSHOT_BYTES + 1)
            .expect("fixture file length is set");
        let path = RelativePath::parse(Path::new("oversized.rs")).expect("fixture path is valid");

        assert!(matches!(
            root.snapshot(&path, u64::MAX),
            Err(VfsError::FileTooLarge { maximum }) if maximum == MAX_SNAPSHOT_BYTES
        ));
    }

    #[test]
    fn cancellable_snapshots_stop_before_opening_repository_data() {
        let (_temporary, root) = fixture();
        let path = RelativePath::parse(Path::new("missing.rs")).expect("fixture path is valid");
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));

        assert!(matches!(
            root.snapshot_cancellable(
                &path,
                1_024,
                &cancellation,
                Instant::now() + Duration::from_secs(1)
            ),
            Err(VfsError::Cancelled(CancellationReason::ClientRequest))
        ));
    }

    #[test]
    fn cancellable_snapshots_enforce_the_local_deadline() {
        let (_temporary, root) = fixture();
        let path = RelativePath::parse(Path::new("missing.rs")).expect("fixture path is valid");
        let now = Instant::now();
        let deadline = now.checked_sub(Duration::from_nanos(1)).unwrap_or(now);

        assert!(matches!(
            root.snapshot_cancellable(&path, 1_024, &Cancellation::new(), deadline),
            Err(VfsError::Cancelled(CancellationReason::DeadlineExceeded))
        ));
    }

    #[test]
    fn snapshot_capture_checks_control_between_read_chunks() {
        let (temporary, root) = fixture();
        fs::write(
            temporary.path().join("large.rs"),
            vec![b'x'; SNAPSHOT_READ_CHUNK_BYTES * 2],
        )
        .expect("large fixture is written");
        let path = RelativePath::parse(Path::new("large.rs")).expect("fixture path is valid");
        let checks = Cell::new(0usize);

        let result = root.snapshot_with_check(&path, MAX_SNAPSHOT_BYTES, || {
            let next = checks.get() + 1;
            checks.set(next);
            if next == 17 {
                Err(VfsError::Cancelled(CancellationReason::ClientRequest))
            } else {
                Ok(())
            }
        });

        assert!(matches!(
            result,
            Err(VfsError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(checks.get(), 17);
    }

    #[test]
    fn parent_traversal_checks_control_between_components() {
        let (temporary, root) = fixture();
        fs::create_dir_all(temporary.path().join("a/b"))
            .expect("nested fixture directories are created");
        let path = RelativePath::parse(Path::new("a/b/sample.rs")).expect("fixture path is valid");
        let checks = Cell::new(0usize);
        let mut check = || {
            let next = checks.get() + 1;
            checks.set(next);
            if next == 7 {
                Err(VfsError::Cancelled(CancellationReason::ClientRequest))
            } else {
                Ok(())
            }
        };

        assert!(matches!(
            root.open_parent(&path, &mut check),
            Err(VfsError::Cancelled(CancellationReason::ClientRequest))
        ));
        assert_eq!(checks.get(), 7);
    }

    #[test]
    fn post_operation_cancellation_precedes_capability_errors() {
        let checks = Cell::new(0usize);
        let mut check = || {
            let next = checks.get() + 1;
            checks.set(next);
            if next == 2 {
                Err(VfsError::Cancelled(CancellationReason::ClientRequest))
            } else {
                Ok(())
            }
        };
        let result: Result<(), VfsError> = controlled(&mut check, || {
            Err(VfsError::ReadFile {
                source: std::io::Error::other("fixture read failure"),
            })
        });

        assert!(matches!(
            result,
            Err(VfsError::Cancelled(CancellationReason::ClientRequest))
        ));
    }

    #[test]
    fn generation_bound_source_reads_verify_hash_and_span() {
        let (temporary, root) = fixture();
        fs::write(temporary.path().join("sample.rs"), b"abcdef").expect("fixture write succeeds");
        let path = RelativePath::parse(Path::new("sample.rs")).expect("fixture path is valid");
        let snapshot = root.snapshot(&path, 1024).expect("capture succeeds");
        let span = SourceSpan::new(snapshot.file(), 1, 4).expect("span is valid");
        let source = SourceRef::new(
            root.repository(),
            GenerationId::from_bytes([7; 20]),
            span,
            snapshot.content_hash(),
            None,
        );

        assert_eq!(
            root.read_source(&source, &path, 1024)
                .expect("source reference resolves"),
            b"bcd"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_roots_reject_symbolic_link_components() {
        use std::os::unix::fs::symlink;

        let base = local_tempdir();
        let real = base.path().join("real");
        fs::create_dir(&real).expect("real repository directory is created");
        symlink(&real, base.path().join("link")).expect("root link is created");
        let repository = derive_repository(b"linked-root").id();

        assert!(RepositoryRoot::open(repository, &base.path().join("link")).is_err());

        let nested = real.join("repository");
        fs::create_dir(&nested).expect("nested repository directory is created");
        assert!(RepositoryRoot::open(repository, &base.path().join("link/repository")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn snapshots_do_not_follow_symbolic_links() {
        use std::os::unix::fs::symlink;

        let (temporary, root) = fixture();
        let outside = local_tempdir();
        fs::write(outside.path().join("secret"), b"secret").expect("outside write succeeds");
        symlink(outside.path().join("secret"), temporary.path().join("link"))
            .expect("symlink creation succeeds");
        let path = RelativePath::parse(Path::new("link")).expect("link path is valid");

        assert!(root.snapshot(&path, 1024).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn joined_paths_preserve_embedded_link_validation() {
        use std::os::unix::fs::symlink;

        let (temporary, root) = fixture();
        let real = temporary.path().join("real");
        fs::create_dir(&real).expect("real directory is created");
        fs::write(real.join("sample.rs"), b"source").expect("fixture source is written");
        symlink(&real, temporary.path().join("link")).expect("directory link is created");
        let linked_parent =
            RelativePath::parse(Path::new("link")).expect("link path identity is valid");
        let path = linked_parent
            .join_name(OsStr::new("sample.rs"))
            .expect("leaf name is valid");

        assert!(root.snapshot(&path, 1_024).is_err());
    }
}
