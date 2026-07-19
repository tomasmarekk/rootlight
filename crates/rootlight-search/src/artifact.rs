//! Trusted manifests and bounded preflight for immutable lexical artifacts.
//!
//! Verification attests one point-in-time directory snapshot before Tantivy
//! opens it; callers retain responsibility for keeping that tree private and immutable.

use std::{
    collections::BTreeSet,
    fmt,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use rootlight_cancel::{Cancellation, CancellationReason};

use crate::model::{BuildStats, SearchError};

const ARTIFACT_MANIFEST_VERSION: u32 = 1;
const MAX_PORTABLE_NAME_BYTES: usize = 128;
const HASH_BUFFER_BYTES: usize = 64 * 1024;
const HARD_MAX_ARTIFACT_FILES: usize = 4_096;
const HARD_MAX_ARTIFACT_FILE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const HARD_MAX_ARTIFACT_TOTAL_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HARD_MAX_ARTIFACT_DURATION: Duration = Duration::from_secs(300);

/// Resource limits for artifact creation and verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactBudget {
    /// Maximum regular files in the flat artifact directory.
    pub max_files: usize,
    /// Maximum bytes accepted from one artifact file.
    pub max_file_bytes: u64,
    /// Maximum aggregate bytes accepted from the artifact.
    pub max_total_bytes: u64,
    /// Maximum monotonic wall time spent hashing and inspecting the artifact.
    pub max_duration: Duration,
}

impl Default for ArtifactBudget {
    fn default() -> Self {
        Self {
            max_files: 512,
            max_file_bytes: 1024 * 1024 * 1024,
            max_total_bytes: 4 * 1024 * 1024 * 1024,
            max_duration: Duration::from_secs(30),
        }
    }
}

/// A versioned trusted description of one closed lexical artifact.
///
/// Fields are intentionally opaque so only this crate can construct a manifest
/// whose file ledger and aggregate statistics agree.
#[derive(Clone, PartialEq, Eq)]
pub struct LexicalArtifactManifest {
    version: u32,
    stats: BuildStats,
    files: Vec<ArtifactFile>,
    total_bytes: u64,
}

impl LexicalArtifactManifest {
    /// Returns the artifact manifest format version.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the committed build statistics bound to this artifact.
    #[must_use]
    pub const fn stats(&self) -> BuildStats {
        self.stats
    }

    /// Returns the exact number of regular files in this artifact.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Returns the aggregate bytes recorded across all artifact files.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }
}

impl fmt::Debug for LexicalArtifactManifest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LexicalArtifactManifest")
            .field("version", &self.version)
            .field("stats", &self.stats)
            .field("file_count", &self.files.len())
            .field("total_bytes", &self.total_bytes)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
struct ArtifactFile {
    name: String,
    length: u64,
    blake3: [u8; 32],
}

/// A one-use proof that an artifact matched its trusted manifest.
///
/// Tantivy subsequently opens files by path. This handle therefore does not
/// prevent a writer with access to the caller-owned directory from replacing
/// files after preflight; the directory must remain private and immutable for
/// the lifetime of the opened [`crate::LexicalIndex`].
pub struct VerifiedLexicalArtifact {
    directory: PathBuf,
    manifest: LexicalArtifactManifest,
}

impl VerifiedLexicalArtifact {
    /// Verifies a closed artifact before it can be consumed by the reader.
    ///
    /// This rejects symbolic links, reparse points, hard links, special files,
    /// non-portable or colliding names, manifest drift, and all declared budget
    /// overruns. File contents are hashed through no-follow handles with
    /// cooperative cancellation checkpoints around every read.
    /// Production path-backed verification remains unavailable while the
    /// private file-handle boundary is disabled.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] when the manifest is incompatible, filesystem
    /// structure is insecure, bytes differ, budgets expire, or cancellation wins.
    pub fn verify(
        directory: &Path,
        manifest: LexicalArtifactManifest,
        budget: ArtifactBudget,
        cancellation: &Cancellation,
    ) -> Result<Self, SearchError> {
        crate::require_private_file_boundary(cfg!(test))?;
        validate_artifact_budget(budget)?;
        validate_manifest(&manifest, budget)?;
        let observed = inspect_directory(
            directory,
            budget,
            &ArtifactControl::new(cancellation, budget.max_duration),
        )?;
        if observed.files != manifest.files || observed.total_bytes != manifest.total_bytes {
            return Err(SearchError::ArtifactIntegrityMismatch);
        }
        Ok(Self {
            directory: directory.to_path_buf(),
            manifest,
        })
    }

    /// Returns the immutable generation recorded by the trusted manifest.
    #[must_use]
    pub const fn generation(&self) -> rootlight_ids::GenerationId {
        self.manifest.stats.generation
    }

    /// Returns the verified aggregate artifact bytes.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.manifest.total_bytes
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) const fn stats(&self) -> BuildStats {
        self.manifest.stats
    }
}

impl fmt::Debug for VerifiedLexicalArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedLexicalArtifact")
            .field("generation", &self.manifest.stats.generation)
            .field("file_count", &self.manifest.files.len())
            .field("total_bytes", &self.manifest.total_bytes)
            .finish_non_exhaustive()
    }
}

pub(crate) fn create_manifest(
    directory: &Path,
    stats: BuildStats,
    budget: ArtifactBudget,
    cancellation: &Cancellation,
) -> Result<LexicalArtifactManifest, SearchError> {
    validate_artifact_budget(budget)?;
    let inspected = inspect_directory(
        directory,
        budget,
        &ArtifactControl::new(cancellation, budget.max_duration),
    )?;
    Ok(LexicalArtifactManifest {
        version: ARTIFACT_MANIFEST_VERSION,
        stats,
        files: inspected.files,
        total_bytes: inspected.total_bytes,
    })
}

fn validate_artifact_budget(budget: ArtifactBudget) -> Result<(), SearchError> {
    for (resource, value, maximum) in [
        (
            "files",
            u64::try_from(budget.max_files)
                .map_err(|_| SearchError::InvalidArtifactBudget { resource: "files" })?,
            u64::try_from(HARD_MAX_ARTIFACT_FILES)
                .map_err(|_| SearchError::InvalidArtifactBudget { resource: "files" })?,
        ),
        (
            "file_bytes",
            budget.max_file_bytes,
            HARD_MAX_ARTIFACT_FILE_BYTES,
        ),
        (
            "total_bytes",
            budget.max_total_bytes,
            HARD_MAX_ARTIFACT_TOTAL_BYTES,
        ),
    ] {
        if value == 0 || value > maximum {
            return Err(SearchError::InvalidArtifactBudget { resource });
        }
    }
    if budget.max_file_bytes > budget.max_total_bytes {
        return Err(SearchError::InvalidArtifactBudget {
            resource: "file_bytes",
        });
    }
    if budget.max_duration.is_zero() || budget.max_duration > HARD_MAX_ARTIFACT_DURATION {
        return Err(SearchError::InvalidArtifactBudget {
            resource: "duration",
        });
    }
    Ok(())
}

fn validate_manifest(
    manifest: &LexicalArtifactManifest,
    budget: ArtifactBudget,
) -> Result<(), SearchError> {
    if manifest.version != ARTIFACT_MANIFEST_VERSION
        || manifest.files.is_empty()
        || manifest.files.len() > budget.max_files
    {
        return Err(SearchError::IncompatibleIndex);
    }
    let mut names = BTreeSet::new();
    let mut portable_names = BTreeSet::new();
    let mut total_bytes = 0u64;
    for file in &manifest.files {
        validate_portable_name(&file.name)?;
        if !names.insert(file.name.as_str())
            || !portable_names.insert(file.name.to_ascii_lowercase())
            || file.length > budget.max_file_bytes
        {
            return Err(SearchError::IncompatibleIndex);
        }
        total_bytes = total_bytes
            .checked_add(file.length)
            .ok_or(SearchError::IncompatibleIndex)?;
        if total_bytes > budget.max_total_bytes {
            return Err(SearchError::IncompatibleIndex);
        }
    }
    if total_bytes != manifest.total_bytes {
        return Err(SearchError::IncompatibleIndex);
    }
    Ok(())
}

struct InspectedArtifact {
    files: Vec<ArtifactFile>,
    total_bytes: u64,
}

fn inspect_directory(
    directory: &Path,
    budget: ArtifactBudget,
    control: &ArtifactControl<'_>,
) -> Result<InspectedArtifact, SearchError> {
    control.check()?;
    validate_directory(directory)?;
    let mut entries = fs::read_dir(directory).map_err(|_| operation("artifact_directory"))?;
    let mut files = Vec::new();
    let mut portable_names = BTreeSet::new();
    let mut total_bytes = 0u64;

    loop {
        control.check()?;
        let next = entries.next();
        control.check()?;
        let Some(entry) = next else {
            break;
        };
        if files.len() >= budget.max_files {
            return Err(SearchError::ArtifactBudgetExceeded { resource: "files" });
        }
        let entry = entry.map_err(|_| operation("artifact_directory"))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| SearchError::InsecureArtifact)?;
        validate_portable_name(&name)?;
        if !portable_names.insert(name.to_ascii_lowercase()) {
            return Err(SearchError::InsecureArtifact);
        }
        let remaining_total_bytes = budget.max_total_bytes.checked_sub(total_bytes).ok_or(
            SearchError::ArtifactBudgetExceeded {
                resource: "total_bytes",
            },
        )?;
        let file = inspect_file(&entry.path(), name, budget, remaining_total_bytes, control)?;
        total_bytes =
            total_bytes
                .checked_add(file.length)
                .ok_or(SearchError::ArtifactBudgetExceeded {
                    resource: "total_bytes",
                })?;
        if total_bytes > budget.max_total_bytes {
            return Err(SearchError::ArtifactBudgetExceeded {
                resource: "total_bytes",
            });
        }
        files.push(file);
    }

    if files.is_empty() {
        return Err(SearchError::IncompatibleIndex);
    }
    files.sort_unstable_by(|left, right| left.name.cmp(&right.name));
    Ok(InspectedArtifact { files, total_bytes })
}

fn validate_directory(directory: &Path) -> Result<(), SearchError> {
    let metadata = fs::symlink_metadata(directory).map_err(|_| operation("artifact_directory"))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(SearchError::InsecureArtifact);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
            return Err(SearchError::InsecureArtifact);
        }
    }
    Ok(())
}

fn inspect_file(
    path: &Path,
    name: String,
    budget: ArtifactBudget,
    remaining_total_bytes: u64,
    control: &ArtifactControl<'_>,
) -> Result<ArtifactFile, SearchError> {
    control.check()?;
    let path_metadata = fs::symlink_metadata(path).map_err(|_| operation("artifact_entry"))?;
    if !path_metadata.file_type().is_file() || path_metadata.file_type().is_symlink() {
        return Err(SearchError::InsecureArtifact);
    }
    let mut file = cap_std::fs::File::from_std(open_no_follow(path)?);
    let opened_metadata = file.metadata().map_err(|_| operation("artifact_entry"))?;
    validate_opened_file(&path_metadata, &opened_metadata)?;
    if opened_metadata.len() > budget.max_file_bytes {
        return Err(SearchError::ArtifactBudgetExceeded {
            resource: "file_bytes",
        });
    }
    if opened_metadata.len() > remaining_total_bytes {
        return Err(SearchError::ArtifactBudgetExceeded {
            resource: "total_bytes",
        });
    }

    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; HASH_BUFFER_BYTES];
    let mut observed_bytes = 0u64;
    loop {
        control.check()?;
        let read = file
            .read(&mut buffer)
            .map_err(|_| operation("artifact_read"))?;
        control.check()?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes
            .checked_add(
                u64::try_from(read).map_err(|_| SearchError::ArtifactBudgetExceeded {
                    resource: "file_bytes",
                })?,
            )
            .ok_or(SearchError::ArtifactBudgetExceeded {
                resource: "file_bytes",
            })?;
        if observed_bytes > budget.max_file_bytes {
            return Err(SearchError::ArtifactBudgetExceeded {
                resource: "file_bytes",
            });
        }
        if observed_bytes > remaining_total_bytes {
            return Err(SearchError::ArtifactBudgetExceeded {
                resource: "total_bytes",
            });
        }
        hasher.update(&buffer[..read]);
    }
    let final_metadata = file.metadata().map_err(|_| operation("artifact_entry"))?;
    validate_same_open_file(&opened_metadata, &final_metadata)?;
    if observed_bytes != opened_metadata.len() || final_metadata.len() != opened_metadata.len() {
        return Err(SearchError::ArtifactIntegrityMismatch);
    }

    Ok(ArtifactFile {
        name,
        length: observed_bytes,
        blake3: *hasher.finalize().as_bytes(),
    })
}

fn validate_opened_file(
    path_metadata: &fs::Metadata,
    opened_metadata: &cap_std::fs::Metadata,
) -> Result<(), SearchError> {
    if !opened_metadata.file_type().is_file() {
        return Err(SearchError::InsecureArtifact);
    }
    if cap_fs_ext::MetadataExt::nlink(opened_metadata) != 1 {
        return Err(SearchError::InsecureArtifact);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;

        if path_metadata.dev() != cap_fs_ext::MetadataExt::dev(opened_metadata)
            || path_metadata.ino() != cap_fs_ext::MetadataExt::ino(opened_metadata)
        {
            return Err(SearchError::InsecureArtifact);
        }
    }
    #[cfg(windows)]
    {
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        let _ = path_metadata;
        if cap_std::fs::MetadataExt::file_attributes(opened_metadata)
            & FILE_ATTRIBUTE_REPARSE_POINT.0
            != 0
        {
            return Err(SearchError::InsecureArtifact);
        }
    }
    Ok(())
}

fn validate_same_open_file(
    before: &cap_std::fs::Metadata,
    after: &cap_std::fs::Metadata,
) -> Result<(), SearchError> {
    if cap_fs_ext::MetadataExt::dev(before) != cap_fs_ext::MetadataExt::dev(after)
        || cap_fs_ext::MetadataExt::ino(before) != cap_fs_ext::MetadataExt::ino(after)
        || cap_fs_ext::MetadataExt::nlink(after) != 1
    {
        return Err(SearchError::ArtifactIntegrityMismatch);
    }
    Ok(())
}

fn open_no_follow(path: &Path) -> Result<File, SearchError> {
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
        .map_err(|_| operation("artifact_open"))?;
        Ok(File::from(descriptor))
    }
    #[cfg(windows)]
    {
        use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt as _};
        use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0);
        options.open(path).map_err(|_| operation("artifact_open"))
    }
}

fn validate_portable_name(name: &str) -> Result<(), SearchError> {
    let stem = name
        .split_once('.')
        .map_or(name, |(stem, _extension)| stem)
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem.strip_prefix("COM").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        })
        || stem.strip_prefix("LPT").is_some_and(|suffix| {
            matches!(suffix, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
        });
    let valid_characters = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if name.is_empty()
        || name.len() > MAX_PORTABLE_NAME_BYTES
        || matches!(name, "." | "..")
        || name.ends_with(['.', ' '])
        || !valid_characters
        || reserved
    {
        return Err(SearchError::InsecureArtifact);
    }
    Ok(())
}

struct ArtifactControl<'a> {
    cancellation: &'a Cancellation,
    started: Instant,
    max_duration: Duration,
}

impl<'a> ArtifactControl<'a> {
    fn new(cancellation: &'a Cancellation, max_duration: Duration) -> Self {
        Self {
            cancellation,
            started: Instant::now(),
            max_duration,
        }
    }

    fn check(&self) -> Result<(), SearchError> {
        self.cancellation.check()?;
        if self.started.elapsed() >= self.max_duration {
            return Err(SearchError::Cancelled(CancellationReason::DeadlineExceeded));
        }
        Ok(())
    }
}

fn operation(operation: &'static str) -> SearchError {
    SearchError::IndexOperation { operation }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rootlight_cancel::{Cancellation, CancellationReason};
    use rootlight_ids::GenerationId;
    use tempfile::TempDir;

    use super::*;

    fn stats() -> BuildStats {
        BuildStats {
            generation: GenerationId::from_bytes([7; 20]),
            documents: 1,
            text_bytes: 4,
        }
    }

    #[test]
    fn manifest_verification_detects_content_drift() {
        let directory = TempDir::new().expect("temp directory");
        fs::write(directory.path().join("meta.json"), b"safe").expect("fixture");
        let manifest = create_manifest(
            directory.path(),
            stats(),
            ArtifactBudget::default(),
            &Cancellation::new(),
        )
        .expect("manifest");
        fs::write(directory.path().join("meta.json"), b"drift").expect("fixture");

        assert_eq!(
            VerifiedLexicalArtifact::verify(
                directory.path(),
                manifest,
                ArtifactBudget::default(),
                &Cancellation::new(),
            )
            .expect_err("drift is rejected"),
            SearchError::ArtifactIntegrityMismatch
        );
    }

    #[test]
    fn artifact_preflight_rejects_hard_links_and_special_entries() {
        let linked = TempDir::new().expect("temp directory");
        let original = linked.path().join("meta.json");
        fs::write(&original, b"safe").expect("fixture");
        fs::hard_link(&original, linked.path().join("alias.json")).expect("hard link fixture");
        assert_eq!(
            create_manifest(
                linked.path(),
                stats(),
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::InsecureArtifact)
        );

        let nested = TempDir::new().expect("temp directory");
        fs::create_dir(nested.path().join("nested")).expect("fixture");
        assert_eq!(
            create_manifest(
                nested.path(),
                stats(),
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::InsecureArtifact)
        );

        #[cfg(unix)]
        {
            let linked = TempDir::new().expect("temp directory");
            let target = linked.path().join("meta.json");
            fs::write(&target, b"safe").expect("fixture");
            std::os::unix::fs::symlink(&target, linked.path().join("alias.json"))
                .expect("symbolic link fixture");
            assert_eq!(
                create_manifest(
                    linked.path(),
                    stats(),
                    ArtifactBudget::default(),
                    &Cancellation::new(),
                ),
                Err(SearchError::InsecureArtifact)
            );
        }
    }

    #[test]
    fn artifact_preflight_rejects_nonportable_names_and_cancellation() {
        let invalid = TempDir::new().expect("temp directory");
        fs::write(invalid.path().join("not portable"), b"safe").expect("fixture");
        assert_eq!(
            create_manifest(
                invalid.path(),
                stats(),
                ArtifactBudget::default(),
                &Cancellation::new(),
            ),
            Err(SearchError::InsecureArtifact)
        );

        let cancelled = TempDir::new().expect("temp directory");
        fs::write(cancelled.path().join("meta.json"), b"safe").expect("fixture");
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert_eq!(
            create_manifest(
                cancelled.path(),
                stats(),
                ArtifactBudget::default(),
                &cancellation,
            ),
            Err(SearchError::Cancelled(CancellationReason::ClientRequest))
        );
    }

    #[test]
    fn artifact_preflight_enforces_file_count_and_byte_budgets_before_hashing_past_them() {
        let oversized = TempDir::new().expect("temp directory");
        fs::write(oversized.path().join("meta.json"), b"four").expect("fixture");
        assert_eq!(
            create_manifest(
                oversized.path(),
                stats(),
                ArtifactBudget {
                    max_file_bytes: 3,
                    max_total_bytes: 3,
                    ..ArtifactBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::ArtifactBudgetExceeded {
                resource: "file_bytes",
            })
        );

        let counted = TempDir::new().expect("temp directory");
        fs::write(counted.path().join("one"), b"1").expect("fixture");
        fs::write(counted.path().join("two"), b"2").expect("fixture");
        assert_eq!(
            create_manifest(
                counted.path(),
                stats(),
                ArtifactBudget {
                    max_files: 1,
                    ..ArtifactBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::ArtifactBudgetExceeded { resource: "files" })
        );

        let aggregate = TempDir::new().expect("temp directory");
        fs::write(aggregate.path().join("one"), b"four").expect("fixture");
        fs::write(aggregate.path().join("two"), b"four").expect("fixture");
        assert_eq!(
            create_manifest(
                aggregate.path(),
                stats(),
                ArtifactBudget {
                    max_file_bytes: 7,
                    max_total_bytes: 7,
                    ..ArtifactBudget::default()
                },
                &Cancellation::new(),
            ),
            Err(SearchError::ArtifactBudgetExceeded {
                resource: "total_bytes",
            })
        );
    }
}
