//! Authoritative metadata reconciliation and bounded content-hash reuse.
//!
//! Watchers may schedule this work, but only a complete scan plus explicitly
//! trusted metadata can reuse a parent hash without reading file bytes.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId};
use serde::Serialize;

use crate::{IncrementalError, ResourceKind, model::validate_limit};

/// Hard ceiling for files in one baseline or authoritative scan.
pub(crate) const HARD_MAX_RECONCILE_FILES: usize = 1_000_000;

/// Per-reconcile file-count limit below the hard safety ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileLimits {
    max_files: usize,
}

impl ReconcileLimits {
    /// Creates a checked reconcile limit.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::InvalidLimit`] for zero or excessive values.
    pub fn new(max_files: usize) -> Result<Self, IncrementalError> {
        validate_limit(ResourceKind::Files, max_files, HARD_MAX_RECONCILE_FILES)?;
        Ok(Self { max_files })
    }
}

impl Default for ReconcileLimits {
    fn default() -> Self {
        Self { max_files: 100_000 }
    }
}

/// Stable platform identity of an opened regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformFileIdentity {
    volume: u64,
    file_index: u64,
}

impl PlatformFileIdentity {
    /// Creates a platform file identity from its source-free numeric parts.
    #[must_use]
    pub const fn new(volume: u64, file_index: u64) -> Self {
        Self { volume, file_index }
    }

    /// Returns the platform volume or device identity.
    #[must_use]
    pub const fn volume(self) -> u64 {
        self.volume
    }

    /// Returns the platform file index or inode identity.
    #[must_use]
    pub const fn file_index(self) -> u64 {
        self.file_index
    }
}

/// Whether the VFS attests that metadata is sufficient for no-op hash reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataReliability {
    /// Length, monotonic comparison semantics, and stable identity are trusted.
    Trusted,
    /// The filesystem or capture path cannot safely support metadata-only reuse.
    Untrusted,
}

/// Source-free metadata captured from an authoritative opened file handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileMetadata {
    length: u64,
    modified_ns: Option<u128>,
    change_token: Option<u128>,
    identity: Option<PlatformFileIdentity>,
    reliability: MetadataReliability,
}

impl FileMetadata {
    /// Creates metadata explicitly trusted for no-op hash reuse.
    ///
    /// Callers must use this constructor only when their VFS contract can
    /// reliably detect replacements and same-size rewrites through the supplied
    /// modification value and stable file identity.
    #[must_use]
    pub const fn trusted(length: u64, modified_ns: u128, identity: PlatformFileIdentity) -> Self {
        Self::trusted_with_change_token(length, modified_ns, modified_ns, identity)
    }

    /// Creates trusted metadata with an independent platform change token.
    ///
    /// The token supplements modification time for same-size rewrites and
    /// replacement detection. It must be stable across unchanged scans.
    #[must_use]
    pub const fn trusted_with_change_token(
        length: u64,
        modified_ns: u128,
        change_token: u128,
        identity: PlatformFileIdentity,
    ) -> Self {
        Self {
            length,
            modified_ns: Some(modified_ns),
            change_token: Some(change_token),
            identity: Some(identity),
            reliability: MetadataReliability::Trusted,
        }
    }

    /// Creates metadata that always requires authoritative content hashing.
    #[must_use]
    pub const fn untrusted(
        length: u64,
        modified_ns: Option<u128>,
        identity: Option<PlatformFileIdentity>,
    ) -> Self {
        Self::untrusted_with_change_token(length, modified_ns, None, identity)
    }

    /// Creates untrusted metadata while retaining an optional change token.
    #[must_use]
    pub const fn untrusted_with_change_token(
        length: u64,
        modified_ns: Option<u128>,
        change_token: Option<u128>,
        identity: Option<PlatformFileIdentity>,
    ) -> Self {
        Self {
            length,
            modified_ns,
            change_token,
            identity,
            reliability: MetadataReliability::Untrusted,
        }
    }

    /// Returns the observed file length.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// Returns the modification value when safely available.
    #[must_use]
    pub const fn modified_ns(self) -> Option<u128> {
        self.modified_ns
    }

    /// Returns the additional platform change-detection token, when available.
    #[must_use]
    pub const fn change_token(self) -> Option<u128> {
        self.change_token
    }

    /// Returns the stable platform identity when safely available.
    #[must_use]
    pub const fn identity(self) -> Option<PlatformFileIdentity> {
        self.identity
    }

    /// Returns the caller-attested metadata reliability.
    #[must_use]
    pub const fn reliability(self) -> MetadataReliability {
        self.reliability
    }

    fn can_reuse_hash_from(self, previous: Self) -> bool {
        self.reliability == MetadataReliability::Trusted
            && previous.reliability == MetadataReliability::Trusted
            && self == previous
    }
}

/// File identity, canonical path digest, and authoritative metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileDescriptor {
    file: FileId,
    path_hash: ContentHash,
    metadata: FileMetadata,
}

impl FileDescriptor {
    /// Creates a source-free file descriptor.
    #[must_use]
    pub const fn new(file: FileId, path_hash: ContentHash, metadata: FileMetadata) -> Self {
        Self {
            file,
            path_hash,
            metadata,
        }
    }

    /// Returns the repository-scoped file identity.
    #[must_use]
    pub const fn file(self) -> FileId {
        self.file
    }

    /// Returns the canonical path-semantics digest.
    #[must_use]
    pub const fn path_hash(self) -> ContentHash {
        self.path_hash
    }

    /// Returns source-free authoritative metadata.
    #[must_use]
    pub const fn metadata(self) -> FileMetadata {
        self.metadata
    }
}

/// One parent-generation file baseline with its authoritative content hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineFile {
    descriptor: FileDescriptor,
    content_hash: ContentHash,
}

impl BaselineFile {
    /// Creates one parent baseline record.
    #[must_use]
    pub const fn new(descriptor: FileDescriptor, content_hash: ContentHash) -> Self {
        Self {
            descriptor,
            content_hash,
        }
    }

    /// Returns the file descriptor.
    #[must_use]
    pub const fn descriptor(self) -> FileDescriptor {
        self.descriptor
    }

    /// Returns the previously verified actual-byte hash.
    #[must_use]
    pub const fn content_hash(self) -> ContentHash {
        self.content_hash
    }
}

/// One file observed by the current authoritative metadata scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ScannedFile(FileDescriptor);

impl ScannedFile {
    /// Creates one current scan record.
    #[must_use]
    pub const fn new(descriptor: FileDescriptor) -> Self {
        Self(descriptor)
    }

    /// Returns the observed file descriptor.
    #[must_use]
    pub const fn descriptor(self) -> FileDescriptor {
        self.0
    }
}

/// Canonical parent metadata and content-hash baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataBaseline {
    files: BTreeMap<FileId, BaselineFile>,
}

impl MetadataBaseline {
    /// Canonicalizes a parent baseline and rejects identity collisions.
    ///
    /// # Errors
    ///
    /// Returns a duplicate, path-collision, limit, or cancellation error.
    pub fn new(
        files: impl IntoIterator<Item = BaselineFile>,
        limits: ReconcileLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical = BTreeMap::new();
        let mut paths = BTreeMap::new();
        for file in files {
            cancellation.check()?;
            let descriptor = file.descriptor();
            insert_path_identity(&mut paths, descriptor.file(), descriptor.path_hash())?;
            if canonical.insert(descriptor.file(), file).is_some() {
                return Err(IncrementalError::DuplicateFile {
                    file: descriptor.file(),
                });
            }
            check_file_count(canonical.len(), limits)?;
        }
        Ok(Self { files: canonical })
    }

    /// Returns files in canonical identity order.
    pub fn files(&self) -> impl Iterator<Item = BaselineFile> + '_ {
        self.files.values().copied()
    }

    /// Returns the number of baseline files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Reports whether the baseline is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Canonical complete metadata view from one authoritative scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthoritativeScan {
    files: BTreeMap<FileId, ScannedFile>,
}

impl AuthoritativeScan {
    /// Canonicalizes a complete current scan and rejects identity collisions.
    ///
    /// # Errors
    ///
    /// Returns a duplicate, path-collision, limit, or cancellation error.
    pub fn new(
        files: impl IntoIterator<Item = ScannedFile>,
        limits: ReconcileLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical = BTreeMap::new();
        let mut paths = BTreeMap::new();
        for file in files {
            cancellation.check()?;
            let descriptor = file.descriptor();
            insert_path_identity(&mut paths, descriptor.file(), descriptor.path_hash())?;
            if canonical.insert(descriptor.file(), file).is_some() {
                return Err(IncrementalError::DuplicateFile {
                    file: descriptor.file(),
                });
            }
            check_file_count(canonical.len(), limits)?;
        }
        Ok(Self { files: canonical })
    }

    /// Returns files in canonical identity order.
    pub fn files(&self) -> impl Iterator<Item = ScannedFile> + '_ {
        self.files.values().copied()
    }

    /// Returns the number of observed files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Reports whether the scan is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn insert_path_identity(
    paths: &mut BTreeMap<ContentHash, FileId>,
    file: FileId,
    path_hash: ContentHash,
) -> Result<(), IncrementalError> {
    if let Some(first) = paths.insert(path_hash, file)
        && first != file
    {
        return Err(IncrementalError::PathIdentityCollision {
            first,
            second: file,
        });
    }
    Ok(())
}

fn check_file_count(observed: usize, limits: ReconcileLimits) -> Result<(), IncrementalError> {
    if observed > limits.max_files {
        return Err(IncrementalError::ResourceLimit {
            resource: ResourceKind::Files,
            observed,
            limit: limits.max_files,
        });
    }
    Ok(())
}

/// Reconcile behavior for trusted metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileMode {
    /// Reuse a hash only when both metadata records are explicitly trusted and equal.
    Normal,
    /// Hash every current file regardless of metadata equality.
    Audit,
}

/// Stable reason for reusing or recomputing one content hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HashDecisionReason {
    /// Trusted identity and metadata are unchanged.
    TrustedMetadataUnchanged,
    /// Audit mode requires an actual-byte hash.
    AuditMode,
    /// No parent file matches this current input.
    NewFile,
    /// Trusted metadata changed, including clock regression or replacement.
    MetadataChanged,
    /// At least one metadata record is not trusted for no-op reuse.
    MetadataUntrusted,
}

/// Hash action for one current file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HashDecision {
    /// Reuse a previously verified content hash.
    Reuse {
        /// Current file identity.
        file: FileId,
        /// Parent file identity, which may differ after a move.
        previous_file: FileId,
        /// Reused actual-byte hash.
        content_hash: ContentHash,
        /// Stable reuse reason.
        reason: HashDecisionReason,
    },
    /// Read and hash the current file through the authoritative VFS.
    Hash {
        /// Current file identity.
        file: FileId,
        /// Stable reason content must be read.
        reason: HashDecisionReason,
    },
}

impl HashDecision {
    /// Returns the current file identity.
    #[must_use]
    pub const fn file(self) -> FileId {
        match self {
            Self::Reuse { file, .. } | Self::Hash { file, .. } => file,
        }
    }

    /// Reports whether the caller must read and hash file bytes.
    #[must_use]
    pub const fn requires_hash(self) -> bool {
        matches!(self, Self::Hash { .. })
    }

    /// Returns the stable decision reason.
    #[must_use]
    pub const fn reason(self) -> HashDecisionReason {
        match self {
            Self::Reuse { reason, .. } | Self::Hash { reason, .. } => reason,
        }
    }
}

/// First phase of authoritative reconcile, awaiting exactly the requested hashes.
#[derive(Debug, Clone)]
pub struct ReconcilePlan {
    baseline: MetadataBaseline,
    scan: AuthoritativeScan,
    matches: BTreeMap<FileId, FileId>,
    decisions: BTreeMap<FileId, HashDecision>,
}

impl ReconcilePlan {
    /// Returns hash decisions in canonical current-file order.
    pub fn decisions(&self) -> impl Iterator<Item = HashDecision> + '_ {
        self.decisions.values().copied()
    }

    /// Returns the files whose actual bytes must be hashed.
    pub fn files_to_hash(&self) -> impl Iterator<Item = FileId> + '_ {
        self.decisions
            .values()
            .copied()
            .filter(|decision| decision.requires_hash())
            .map(HashDecision::file)
    }

    /// Completes reconcile with exactly the hashes requested by this plan.
    ///
    /// # Errors
    ///
    /// Returns a missing-hash, unexpected-hash, limit, or cancellation error.
    pub fn finish(
        self,
        hashes: &BTreeMap<FileId, ContentHash>,
        limits: ReconcileLimits,
        cancellation: &Cancellation,
    ) -> Result<ReconcileOutcome, IncrementalError> {
        for file in hashes.keys().copied() {
            cancellation.check()?;
            if !self
                .decisions
                .get(&file)
                .is_some_and(|decision| decision.requires_hash())
            {
                return Err(IncrementalError::UnexpectedHash { file });
            }
        }

        let mut next_files = Vec::with_capacity(self.scan.len().min(limits.max_files));
        let mut changes = Vec::with_capacity(
            self.baseline
                .len()
                .saturating_add(self.scan.len())
                .min(limits.max_files.saturating_mul(2)),
        );
        let mut matched_previous = BTreeSet::new();

        for scanned in self.scan.files() {
            cancellation.check()?;
            let descriptor = scanned.descriptor();
            let decision = self.decisions.get(&descriptor.file()).copied().ok_or(
                IncrementalError::MissingHash {
                    file: descriptor.file(),
                },
            )?;
            let content_hash = match decision {
                HashDecision::Reuse { content_hash, .. } => content_hash,
                HashDecision::Hash { file, .. } => hashes
                    .get(&file)
                    .copied()
                    .ok_or(IncrementalError::MissingHash { file })?,
            };
            let next = BaselineFile::new(descriptor, content_hash);
            next_files.push(next);

            let previous = self.matches.get(&descriptor.file()).copied();
            if let Some(previous_file) = previous {
                matched_previous.insert(previous_file);
                let previous_record = self.baseline.files.get(&previous_file).copied().ok_or(
                    IncrementalError::MissingHash {
                        file: previous_file,
                    },
                )?;
                changes.push(FileChange::between(previous_record, next));
            } else {
                changes.push(FileChange::added(next));
            }
        }

        for previous in self.baseline.files() {
            cancellation.check()?;
            if !matched_previous.contains(&previous.descriptor().file()) {
                changes.push(FileChange::deleted(previous));
            }
        }
        changes.sort();

        let baseline = MetadataBaseline::new(next_files, limits, cancellation)?;
        Ok(ReconcileOutcome { baseline, changes })
    }
}

/// Builds a two-phase reconcile plan from complete parent and current metadata.
///
/// Watcher events are intentionally absent from this API: they may prioritize
/// when the caller obtains `scan`, but cannot change the semantic decision.
///
/// # Errors
///
/// Returns a limit or cancellation error.
pub fn plan_reconcile(
    baseline: &MetadataBaseline,
    scan: &AuthoritativeScan,
    mode: ReconcileMode,
    limits: ReconcileLimits,
    cancellation: &Cancellation,
) -> Result<ReconcilePlan, IncrementalError> {
    check_file_count(baseline.len(), limits)?;
    check_file_count(scan.len(), limits)?;

    let mut matches = BTreeMap::new();
    let mut matched_previous = BTreeSet::new();
    for file in scan.files.keys().copied() {
        cancellation.check()?;
        if baseline.files.contains_key(&file) {
            matches.insert(file, file);
            matched_previous.insert(file);
        }
    }

    let previous_by_identity = unique_unmatched_identities(&baseline.files, &matched_previous);
    let current_matched: BTreeSet<FileId> = matches.keys().copied().collect();
    let current_by_identity = unique_unmatched_scan_identities(&scan.files, &current_matched);
    for (identity, current) in current_by_identity {
        cancellation.check()?;
        if let Some(previous) = previous_by_identity.get(&identity).copied() {
            matches.insert(current, previous);
            matched_previous.insert(previous);
        }
    }

    let mut decisions = BTreeMap::new();
    for scanned in scan.files() {
        cancellation.check()?;
        let descriptor = scanned.descriptor();
        let decision = match matches.get(&descriptor.file()).copied() {
            Some(previous_file) => {
                let previous = baseline.files.get(&previous_file).copied().ok_or(
                    IncrementalError::MissingHash {
                        file: previous_file,
                    },
                )?;
                if mode == ReconcileMode::Audit {
                    HashDecision::Hash {
                        file: descriptor.file(),
                        reason: HashDecisionReason::AuditMode,
                    }
                } else if descriptor
                    .metadata()
                    .can_reuse_hash_from(previous.descriptor().metadata())
                {
                    HashDecision::Reuse {
                        file: descriptor.file(),
                        previous_file,
                        content_hash: previous.content_hash(),
                        reason: HashDecisionReason::TrustedMetadataUnchanged,
                    }
                } else {
                    let reason = if descriptor.metadata().reliability()
                        == MetadataReliability::Untrusted
                        || previous.descriptor().metadata().reliability()
                            == MetadataReliability::Untrusted
                    {
                        HashDecisionReason::MetadataUntrusted
                    } else {
                        HashDecisionReason::MetadataChanged
                    };
                    HashDecision::Hash {
                        file: descriptor.file(),
                        reason,
                    }
                }
            }
            None => HashDecision::Hash {
                file: descriptor.file(),
                reason: if mode == ReconcileMode::Audit {
                    HashDecisionReason::AuditMode
                } else {
                    HashDecisionReason::NewFile
                },
            },
        };
        decisions.insert(descriptor.file(), decision);
    }

    Ok(ReconcilePlan {
        baseline: baseline.clone(),
        scan: scan.clone(),
        matches,
        decisions,
    })
}

fn unique_unmatched_identities(
    files: &BTreeMap<FileId, BaselineFile>,
    matched: &BTreeSet<FileId>,
) -> BTreeMap<PlatformFileIdentity, FileId> {
    unique_identities(files.iter().filter_map(|(file, entry)| {
        (!matched.contains(file))
            .then_some((entry.descriptor().metadata().identity(), *file))
            .and_then(|(identity, file)| identity.map(|identity| (identity, file)))
    }))
}

fn unique_unmatched_scan_identities(
    files: &BTreeMap<FileId, ScannedFile>,
    matched: &BTreeSet<FileId>,
) -> BTreeMap<PlatformFileIdentity, FileId> {
    unique_identities(files.iter().filter_map(|(file, entry)| {
        (!matched.contains(file))
            .then_some((entry.descriptor().metadata().identity(), *file))
            .and_then(|(identity, file)| identity.map(|identity| (identity, file)))
    }))
}

fn unique_identities(
    entries: impl IntoIterator<Item = (PlatformFileIdentity, FileId)>,
) -> BTreeMap<PlatformFileIdentity, FileId> {
    let mut unique = BTreeMap::new();
    let mut ambiguous = BTreeSet::new();
    for (identity, file) in entries {
        if unique.insert(identity, file).is_some() {
            ambiguous.insert(identity);
        }
    }
    for identity in ambiguous {
        unique.remove(&identity);
    }
    unique
}

/// Canonical authoritative file transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// Path and actual bytes are unchanged.
    NoChange,
    /// A new file appeared.
    Added,
    /// Existing file bytes changed without a path move.
    Modified,
    /// Content stayed equal while path or file identity changed.
    Moved,
    /// Stable platform identity moved and its content changed.
    MovedAndModified,
    /// A parent file disappeared.
    Deleted,
}

/// One source-free canonical file transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileChange {
    kind: FileChangeKind,
    previous_file: Option<FileId>,
    current_file: Option<FileId>,
    previous_hash: Option<ContentHash>,
    current_hash: Option<ContentHash>,
}

impl FileChange {
    fn between(previous: BaselineFile, current: BaselineFile) -> Self {
        let moved = previous.descriptor().file() != current.descriptor().file()
            || previous.descriptor().path_hash() != current.descriptor().path_hash();
        let modified = previous.content_hash() != current.content_hash();
        let kind = match (moved, modified) {
            (false, false) => FileChangeKind::NoChange,
            (false, true) => FileChangeKind::Modified,
            (true, false) => FileChangeKind::Moved,
            (true, true) => FileChangeKind::MovedAndModified,
        };
        Self {
            kind,
            previous_file: Some(previous.descriptor().file()),
            current_file: Some(current.descriptor().file()),
            previous_hash: Some(previous.content_hash()),
            current_hash: Some(current.content_hash()),
        }
    }

    fn added(current: BaselineFile) -> Self {
        Self {
            kind: FileChangeKind::Added,
            previous_file: None,
            current_file: Some(current.descriptor().file()),
            previous_hash: None,
            current_hash: Some(current.content_hash()),
        }
    }

    fn deleted(previous: BaselineFile) -> Self {
        Self {
            kind: FileChangeKind::Deleted,
            previous_file: Some(previous.descriptor().file()),
            current_file: None,
            previous_hash: Some(previous.content_hash()),
            current_hash: None,
        }
    }

    /// Returns the canonical change class.
    #[must_use]
    pub const fn kind(self) -> FileChangeKind {
        self.kind
    }

    /// Returns the parent file identity, when it existed.
    #[must_use]
    pub const fn previous_file(self) -> Option<FileId> {
        self.previous_file
    }

    /// Returns the current file identity, when it exists.
    #[must_use]
    pub const fn current_file(self) -> Option<FileId> {
        self.current_file
    }

    /// Returns the parent actual-byte hash, when the file existed.
    #[must_use]
    pub const fn previous_hash(self) -> Option<ContentHash> {
        self.previous_hash
    }

    /// Returns the current actual-byte hash, when the file exists.
    #[must_use]
    pub const fn current_hash(self) -> Option<ContentHash> {
        self.current_hash
    }
}

/// Completed authoritative baseline and deterministic file changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    baseline: MetadataBaseline,
    changes: Vec<FileChange>,
}

impl ReconcileOutcome {
    /// Returns the next complete metadata and content-hash baseline.
    #[must_use]
    pub const fn baseline(&self) -> &MetadataBaseline {
        &self.baseline
    }

    /// Returns every file transition in canonical order, including no-op files.
    #[must_use]
    pub fn changes(&self) -> &[FileChange] {
        &self.changes
    }
}
