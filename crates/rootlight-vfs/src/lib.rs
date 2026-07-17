//! Capability-confined repository access and immutable source snapshots.
//!
//! Repository paths are untrusted. Callers can address only validated relative
//! paths beneath an opened root, and every source read verifies file stability.

#![forbid(unsafe_code)]

use std::{
    ffi::{OsStr, OsString},
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

#[cfg(windows)]
use cap_fs_ext::OsMetadataExt as _;
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt as _, OpenOptionsFollowExt};
use cap_std::{
    ambient_authority,
    fs::{Dir, File, Metadata, OpenOptions},
};
use rootlight_ids::{ContentHash, FileId, FileIdentity, RepositoryId, content_hash, derive_file};
use rootlight_ir::SourceRef;

pub mod platform;

/// Hard ceiling for one VFS source capture, independent of caller configuration.
pub const MAX_SNAPSHOT_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum number of relative path components accepted by the VFS.
pub const MAX_PATH_COMPONENTS: usize = 256;
/// Maximum platform path bytes accepted by the VFS.
pub const MAX_PATH_BYTES: usize = 32 * 1024;

/// A validated repository-relative path with platform-stable identity bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

        let path_bytes = platform_path_bytes(path.as_os_str());
        if path_bytes.len() > MAX_PATH_BYTES {
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
            let raw_bytes = platform_path_bytes(component);
            if raw_bytes.is_empty() || contains_separator_alias(component) {
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
        let mut components = self.components.clone();
        if components.len() >= MAX_PATH_COMPONENTS || contains_separator_alias(name) {
            return Err(VfsError::InvalidRelativePath);
        }
        let raw_bytes = platform_path_bytes(name);
        if raw_bytes.is_empty() {
            return Err(VfsError::InvalidRelativePath);
        }
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

/// Immutable bytes captured from one stable regular file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    file: FileId,
    path: RelativePath,
    content: Vec<u8>,
    content_hash: ContentHash,
    metadata: SnapshotMetadata,
}

impl SourceSnapshot {
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

/// Source-free metadata retained with a source snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotMetadata {
    /// File length observed on the opened handle.
    pub length: u64,
    /// Last modification time in nanoseconds since the Unix epoch, when available.
    pub modified_ns: Option<u128>,
    /// Platform volume/device identity, when exposed safely.
    pub volume: Option<u64>,
    /// Platform file identity, when exposed safely.
    pub file_index: Option<u64>,
}

/// A capability handle confining all repository content access.
#[derive(Debug)]
pub struct RepositoryRoot {
    repository: RepositoryId,
    canonical_path: PathBuf,
    directory: Dir,
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
            entries.push(DirectoryEntry {
                name,
                kind,
                length: metadata.len(),
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
        let maximum_bytes = maximum_bytes.min(MAX_SNAPSHOT_BYTES);
        if maximum_bytes == 0 {
            return Err(VfsError::InvalidByteLimit);
        }
        let first = self.capture(path, maximum_bytes)?;
        let second = self.capture(path, maximum_bytes)?;
        if first.metadata != second.metadata || first.hash != second.hash {
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

    fn open_parent(&self, path: &RelativePath) -> Result<Dir, VfsError> {
        let mut directory = self
            .directory
            .try_clone()
            .map_err(|source| VfsError::OpenDirectory { source })?;
        for component in path.parent_components() {
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

    fn capture(&self, path: &RelativePath, maximum_bytes: u64) -> Result<Capture, VfsError> {
        let parent = self.open_parent(path)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        let mut file = parent
            .open_with(path.file_name(), &options)
            .map_err(|source| VfsError::OpenFile { source })?;
        let before = checked_metadata(&file, maximum_bytes)?;
        let capacity = usize::try_from(before.length).map_err(|_| VfsError::FileTooLarge {
            maximum: maximum_bytes,
        })?;
        let mut content = Vec::with_capacity(capacity);
        file.by_ref()
            .take(maximum_bytes.saturating_add(1))
            .read_to_end(&mut content)
            .map_err(|source| VfsError::ReadFile { source })?;
        if u64::try_from(content.len()).unwrap_or(u64::MAX) > maximum_bytes {
            return Err(VfsError::FileTooLarge {
                maximum: maximum_bytes,
            });
        }
        let after = checked_metadata(&file, maximum_bytes)?;
        if before != after || after.length != u64::try_from(content.len()).unwrap_or(u64::MAX) {
            return Err(VfsError::UnstableFile);
        }
        Ok(Capture {
            hash: content_hash(&content),
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
    let modified_ns = metadata.modified().ok().and_then(|modified| {
        modified
            .into_std()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|duration| duration.as_nanos())
    });
    SnapshotMetadata {
        length: metadata.len(),
        modified_ns,
        volume: Some(metadata.dev()),
        file_index: Some(metadata.ino()),
    }
}

#[cfg(windows)]
fn is_reparse_point(metadata: &Metadata) -> bool {
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &Metadata) -> bool {
    false
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

fn contains_separator_alias(component: &OsStr) -> bool {
    component
        .to_str()
        .is_some_and(|component| component.contains('\\'))
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
    /// The source reference does not identify this repository or file.
    #[error("source reference does not match repository path")]
    SourceReferenceMismatch,
    /// The source reference names an older content hash.
    #[error("source reference content hash is stale")]
    StaleContentHash,
    /// The source reference span lies outside the captured bytes.
    #[error("source reference span is outside captured content")]
    InvalidSourceSpan,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_ir::SourceSpan;
    use std::fs;
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
}
