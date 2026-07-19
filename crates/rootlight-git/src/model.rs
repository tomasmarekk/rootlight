//! Public data model for bounded Git state, changes, and lineage evidence.
//!
//! DTOs are importer-facing and may be untrusted; only `CanonicalGitSnapshot`
//! proves validation and deterministic normalization succeeded.

use rootlight_cancel::Cancelled;
use rootlight_ids::{ContentHash, RepositoryId, SymbolId, content_hash};
use serde::Serialize;

/// Current declarative Git contract version.
pub const GIT_CONTRACT_VERSION: GitContractVersion = GitContractVersion::new(1, 0);
/// Absolute commit ceiling independent of repository configuration.
pub const HARD_MAX_COMMITS: usize = 100_000;
/// Absolute file-change ceiling independent of repository configuration.
pub const HARD_MAX_CHANGES: usize = 1_000_000;
/// Absolute changed-span ceiling independent of repository configuration.
pub const HARD_MAX_CHANGED_SPANS: usize = 4_000_000;
/// Absolute symbol-lineage candidate ceiling independent of repository configuration.
pub const HARD_MAX_LINEAGE_CANDIDATES: usize = 1_000_000;
/// Absolute byte ceiling for one importer-provided text field.
pub const HARD_MAX_TEXT_FIELD_BYTES: usize = 16 * 1024;
/// Absolute aggregate text-byte ceiling for one snapshot.
pub const HARD_MAX_TOTAL_TEXT_BYTES: usize = 128 * 1024 * 1024;

/// Version of the importer-to-core Git evidence contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct GitContractVersion {
    /// Breaking schema generation.
    pub major: u16,
    /// Additive schema generation within one major version.
    pub minor: u16,
}

impl GitContractVersion {
    /// Creates a contract version from its numeric components.
    #[must_use]
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }
}

/// Per-import limits below the crate's absolute safety ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GitLimits {
    pub(crate) max_commits: usize,
    pub(crate) max_changes: usize,
    pub(crate) max_changed_spans: usize,
    pub(crate) max_lineage_candidates: usize,
    pub(crate) max_text_field_bytes: usize,
    pub(crate) max_total_text_bytes: usize,
}

impl GitLimits {
    /// Creates checked Git evidence limits.
    ///
    /// # Errors
    ///
    /// Returns [`GitContractError::InvalidLimits`] when a value is zero or
    /// exceeds its hard ceiling.
    pub fn new(
        max_commits: usize,
        max_changes: usize,
        max_changed_spans: usize,
        max_lineage_candidates: usize,
        max_text_field_bytes: usize,
        max_total_text_bytes: usize,
    ) -> Result<Self, GitContractError> {
        if max_commits == 0
            || max_commits > HARD_MAX_COMMITS
            || max_changes == 0
            || max_changes > HARD_MAX_CHANGES
            || max_changed_spans == 0
            || max_changed_spans > HARD_MAX_CHANGED_SPANS
            || max_lineage_candidates == 0
            || max_lineage_candidates > HARD_MAX_LINEAGE_CANDIDATES
            || max_text_field_bytes == 0
            || max_text_field_bytes > HARD_MAX_TEXT_FIELD_BYTES
            || max_total_text_bytes == 0
            || max_total_text_bytes > HARD_MAX_TOTAL_TEXT_BYTES
            || max_text_field_bytes > max_total_text_bytes
        {
            return Err(GitContractError::InvalidLimits);
        }
        Ok(Self {
            max_commits,
            max_changes,
            max_changed_spans,
            max_lineage_candidates,
            max_text_field_bytes,
            max_total_text_bytes,
        })
    }

    /// Returns the configured commit ceiling.
    #[must_use]
    pub const fn max_commits(&self) -> usize {
        self.max_commits
    }

    /// Returns the configured file-change ceiling.
    #[must_use]
    pub const fn max_changes(&self) -> usize {
        self.max_changes
    }

    /// Returns the configured changed-span ceiling.
    #[must_use]
    pub const fn max_changed_spans(&self) -> usize {
        self.max_changed_spans
    }

    /// Returns the configured symbol-lineage candidate ceiling.
    #[must_use]
    pub const fn max_lineage_candidates(&self) -> usize {
        self.max_lineage_candidates
    }

    /// Returns the configured byte ceiling for one text field.
    #[must_use]
    pub const fn max_text_field_bytes(&self) -> usize {
        self.max_text_field_bytes
    }

    /// Returns the configured aggregate text-byte ceiling.
    #[must_use]
    pub const fn max_total_text_bytes(&self) -> usize {
        self.max_total_text_bytes
    }
}

impl Default for GitLimits {
    fn default() -> Self {
        Self {
            max_commits: 2_000,
            max_changes: 100_000,
            max_changed_spans: 400_000,
            max_lineage_candidates: 100_000,
            max_text_field_bytes: 4 * 1024,
            max_total_text_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Repository-level Git availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RepositoryState {
    /// The registered source root is not backed by usable Git metadata.
    NonGit {
        /// Stable reason Git evidence is absent.
        reason: NonGitReason,
    },
    /// Valid Git metadata was observed without executing repository code.
    Git(GitRepositoryState),
}

/// Source-free reason a repository has no usable Git evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonGitReason {
    /// No repository metadata was present.
    MetadataAbsent,
    /// Present metadata used an unsupported representation.
    UnsupportedMetadata,
    /// Repository policy disabled Git evidence collection.
    DisabledByPolicy,
}

/// Validated repository-wide Git state supplied by an importer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitRepositoryState {
    /// Object identifier format used by this object database.
    pub object_format: ObjectFormat,
    /// Source-free identity of the shared object database.
    pub object_database: ObjectDatabaseId,
    /// Completeness of the locally available object history.
    pub history: HistoryState,
    /// Actual bounded history window represented by this snapshot.
    pub coverage: HistoryCoverage,
}

/// Git object identifier algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectFormat {
    /// Twenty-byte SHA-1 object identifiers.
    Sha1,
    /// Thirty-two-byte SHA-256 object identifiers.
    Sha256,
}

/// A Git object identifier whose algorithm is explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "format", content = "bytes", rename_all = "snake_case")]
pub enum ObjectId {
    /// A twenty-byte SHA-1 identifier.
    Sha1([u8; 20]),
    /// A thirty-two-byte SHA-256 identifier.
    Sha256([u8; 32]),
}

impl ObjectId {
    /// Creates a SHA-1 object identifier from canonical bytes.
    #[must_use]
    pub const fn sha1(bytes: [u8; 20]) -> Self {
        Self::Sha1(bytes)
    }

    /// Creates a SHA-256 object identifier from canonical bytes.
    #[must_use]
    pub const fn sha256(bytes: [u8; 32]) -> Self {
        Self::Sha256(bytes)
    }

    /// Returns this identifier's declared object format.
    #[must_use]
    pub const fn format(self) -> ObjectFormat {
        match self {
            Self::Sha1(_) => ObjectFormat::Sha1,
            Self::Sha256(_) => ObjectFormat::Sha256,
        }
    }
}

/// Source-free identity of a Git object database shared by worktrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ObjectDatabaseId([u8; 32]);

impl ObjectDatabaseId {
    /// Creates an object-database identity from importer-derived canonical bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical object-database identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Locally available history completeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HistoryState {
    /// Every object required by the configured history window was available.
    Complete,
    /// The local clone ends at explicit shallow boundary commits.
    Shallow {
        /// Canonical shallow boundary object identifiers.
        boundary_commits: Vec<ObjectId>,
    },
    /// Required objects were unavailable for a source-free reason.
    Incomplete {
        /// Stable class of the history gap.
        reason: HistoryGapReason,
        /// Known unavailable object identifiers, if safely observable.
        missing_objects: Vec<ObjectId>,
    },
}

/// Source-free class of incomplete Git history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryGapReason {
    /// One or more referenced objects are absent.
    MissingObjects,
    /// One or more objects failed structural validation.
    CorruptObjects,
    /// Local policy or permissions denied object access.
    AccessDenied,
    /// The importer stopped before proving completeness.
    ImporterIncomplete,
}

/// Bounded history coverage reported by the importer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HistoryCoverage {
    /// Commits retained in this snapshot.
    pub imported_commits: u32,
    /// Requested commit window before other policy ceilings applied.
    pub requested_commit_limit: u32,
    /// Oldest retained author timestamp in Unix seconds, when available.
    pub oldest_imported_time_unix_seconds: Option<i64>,
    /// Canonical reasons the requested history window was truncated.
    pub truncation: Vec<HistoryTruncation>,
}

/// Policy or repository condition that bounded imported history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryTruncation {
    /// Configured commit count reached its limit.
    CommitLimit,
    /// Configured age window reached its limit.
    TimeLimit,
    /// Configured path scope excluded other history.
    PathScope,
    /// Configured disk budget stopped collection.
    DiskBudget,
    /// A shallow clone boundary stopped traversal.
    ShallowBoundary,
    /// Missing or unreadable objects stopped traversal.
    MissingObjects,
}

/// One worktree sharing the repository's object database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorktreeState {
    /// Stable importer-assigned worktree label, never an absolute path.
    pub id: String,
    /// Branch, detached, unborn, or unavailable HEAD state.
    pub head: HeadState,
    /// Tree represented by the index, when available.
    pub index_tree: Option<ObjectId>,
    /// Source-free dirty-state counters.
    pub status: WorktreeStatus,
    /// Sparse-checkout state without evaluating patterns.
    pub sparse_checkout: SparseCheckoutState,
}

/// Worktree HEAD state.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum HeadState {
    /// A fully qualified branch reference without a first commit.
    Unborn {
        /// Fully qualified branch reference.
        reference: String,
    },
    /// A fully qualified branch reference and resolved commit.
    Branch {
        /// Fully qualified branch reference.
        reference: String,
        /// Commit currently named by the branch.
        commit: ObjectId,
    },
    /// A detached commit.
    Detached {
        /// Commit checked out without a branch reference.
        commit: ObjectId,
    },
    /// HEAD could not be resolved safely.
    Unavailable {
        /// Stable reason HEAD resolution was incomplete.
        reason: HeadUnavailableReason,
    },
}

/// Source-free reason a worktree HEAD is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadUnavailableReason {
    /// The worktree has no HEAD metadata.
    MetadataAbsent,
    /// HEAD references an unavailable object.
    MissingObject,
    /// HEAD metadata failed structural validation.
    InvalidMetadata,
    /// The importer stopped before resolving HEAD.
    ImporterIncomplete,
}

/// Bounded source-free worktree status counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct WorktreeStatus {
    /// Tracked paths differing from the index.
    pub tracked_changes: u32,
    /// Untracked paths observed by the importer.
    pub untracked_paths: u32,
    /// Unmerged index entries.
    pub conflicts: u32,
}

/// Sparse-checkout state retained without touching the filesystem.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SparseCheckoutState {
    /// Sparse checkout is disabled.
    Disabled,
    /// Sparse checkout is enabled with bounded order-sensitive patterns.
    Enabled {
        /// Whether cone-mode interpretation was active.
        cone_mode: bool,
        /// Patterns in importer-observed evaluation order.
        patterns: Vec<String>,
    },
    /// Sparse state could not be determined safely.
    Unknown,
}

/// Source-free commit metadata retained by the bounded history overlay.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CommitRecord {
    /// Commit object identifier.
    pub id: ObjectId,
    /// Ordered parent commits; first-parent order is semantically significant.
    pub parents: Vec<ObjectId>,
    /// Root tree object identifier.
    pub tree: ObjectId,
    /// Author timestamp in Unix seconds.
    pub author_time_unix_seconds: i64,
}

/// A revision-like state that can participate in a comparison.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum RevisionSelector {
    /// One immutable commit object.
    Commit(ObjectId),
    /// A fully qualified branch or tag reference at an observed target.
    Reference {
        /// Fully qualified reference name.
        name: String,
        /// Object named by the reference in this snapshot.
        target: ObjectId,
    },
    /// The observed HEAD state of one worktree.
    Head {
        /// Stable worktree label.
        worktree: String,
    },
    /// The observed index state of one worktree.
    Index {
        /// Stable worktree label.
        worktree: String,
    },
    /// The observed filesystem state of one worktree.
    WorkingTree {
        /// Stable worktree label.
        worktree: String,
    },
}

/// A deterministic comparison between two Git or worktree states.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct ChangeSet {
    /// Comparison base state.
    pub base: RevisionSelector,
    /// Comparison head state.
    pub head: RevisionSelector,
    /// Canonical file deltas for the comparison.
    pub changes: Vec<FileChange>,
}

/// Observable file delta before any rename interpretation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// A path exists only in the head state.
    Added,
    /// A path exists only in the base state.
    Deleted,
    /// One path exists in both states with changed content.
    Modified,
    /// One path exists in both states with a changed Git entry type.
    TypeChanged,
}

/// One canonical path delta with bounded changed spans.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct FileChange {
    /// Observable delta class; renames remain separate candidates.
    pub kind: FileChangeKind,
    /// Canonical repository-relative base path, when present.
    pub before_path: Option<String>,
    /// Canonical repository-relative head path, when present.
    pub after_path: Option<String>,
    /// Changed byte spans ordered by base then head coordinates.
    pub spans: Vec<ChangedSpan>,
}

/// One half-open byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ByteSpan {
    /// Inclusive start byte.
    pub start: u64,
    /// Exclusive end byte.
    pub end: u64,
}

impl ByteSpan {
    /// Creates a half-open byte range.
    ///
    /// # Errors
    ///
    /// Returns [`GitContractError::InvalidSpan`] when `end` precedes `start`.
    pub fn new(start: u64, end: u64) -> Result<Self, GitContractError> {
        if end < start {
            return Err(GitContractError::InvalidSpan);
        }
        Ok(Self { start, end })
    }
}

/// Base and head byte ranges for one changed region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct ChangedSpan {
    /// Base-state byte range, absent for pure additions.
    pub before: Option<ByteSpan>,
    /// Head-state byte range, absent for pure deletions.
    pub after: Option<ByteSpan>,
}

/// Stable binary grouping key for ambiguous rename or lineage candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CandidateGroupId([u8; 16]);

impl CandidateGroupId {
    /// Creates a candidate group from canonical importer-derived bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 16]) -> Self {
        Self(bytes)
    }

    /// Returns the canonical candidate-group bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 16] {
        &self.0
    }
}

/// Evidence that one deleted path and one added path may be a rename.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct RenameCandidate {
    /// Comparison base state containing the deletion.
    pub base: RevisionSelector,
    /// Comparison head state containing the addition.
    pub head: RevisionSelector,
    /// Candidate base path.
    pub before_path: String,
    /// Candidate head path.
    pub after_path: String,
    /// Ambiguity group retained for downstream ranking.
    pub group: CandidateGroupId,
    /// Fixed-point confidence in basis points, from zero through ten thousand.
    pub confidence_bps: u16,
    /// Canonical evidence classes supporting the candidate.
    pub evidence: Vec<RenameEvidenceKind>,
}

/// Source-free evidence class for a rename candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RenameEvidenceKind {
    /// Base and head entries reference the same object identifier.
    ExactObject,
    /// Importer-computed content hashes are equal.
    ExactContent,
    /// Git-style bounded similarity contributed evidence.
    Similarity,
    /// Declaration fingerprints contributed evidence.
    DeclarationFingerprint,
    /// Bounded surrounding structure contributed evidence.
    Neighborhood,
    /// An audited importer reported native rename metadata.
    ImporterSignal,
}

/// One submodule gitlink and local checkout observation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SubmoduleState {
    /// Stable worktree label containing this submodule entry.
    pub worktree: String,
    /// Canonical repository-relative submodule path.
    pub path: String,
    /// Commit recorded by the parent repository gitlink.
    pub recorded_commit: ObjectId,
    /// Local checkout state observed without executing submodule commands.
    pub checkout: SubmoduleCheckoutState,
}

/// Local submodule checkout state.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum SubmoduleCheckoutState {
    /// No local checkout is initialized.
    Uninitialized,
    /// A local checkout commit was observed.
    Present {
        /// Checked-out commit with its submodule repository's object format.
        commit: ObjectId,
    },
    /// Local metadata references an unavailable object.
    MissingObject {
        /// Referenced object in the submodule's own format, when observable.
        commit: Option<ObjectId>,
    },
    /// Local checkout state could not be determined.
    Unknown,
}

/// Candidate historical relationship between distinct semantic symbol IDs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SymbolLineageCandidate {
    /// Ambiguity group retained for downstream ranking.
    pub group: CandidateGroupId,
    /// Historical symbol identity.
    pub prior: SymbolId,
    /// Current symbol identity.
    pub current: SymbolId,
    /// Candidate lineage relation.
    pub kind: LineageKind,
    /// Fixed-point confidence in basis points, from zero through ten thousand.
    pub confidence_bps: u16,
    /// Canonical evidence classes supporting the candidate.
    pub evidence: Vec<LineageEvidenceKind>,
}

/// Directional historical symbol relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageKind {
    /// Current symbol may have been renamed from the prior symbol.
    RenamedFrom,
    /// Current symbol may have moved from the prior symbol.
    MovedFrom,
    /// Current symbol may be one output of a split from the prior symbol.
    SplitFrom,
    /// Current symbol may merge behavior from the prior symbol.
    MergedFrom,
}

/// Source-free evidence class for a symbol-lineage candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LineageEvidenceKind {
    /// Bounded Git rename evidence contributed to this candidate.
    GitRename,
    /// Exact content identity contributed to this candidate.
    ExactContent,
    /// Declaration fingerprints contributed to this candidate.
    DeclarationFingerprint,
    /// Bounded surrounding structure contributed to this candidate.
    Neighborhood,
    /// Import or reference continuity contributed to this candidate.
    ReferenceContinuity,
}

/// Untrusted declarative snapshot supplied by a future audited Git importer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitSnapshotInput {
    /// Import contract version.
    pub version: GitContractVersion,
    /// Stable repository identity independent of Git remotes.
    pub repository: RepositoryId,
    /// Explicit Git or non-Git repository state.
    pub state: RepositoryState,
    /// Worktrees sharing the Git object database.
    pub worktrees: Vec<WorktreeState>,
    /// Bounded source-free commit metadata.
    pub commits: Vec<CommitRecord>,
    /// Bounded comparisons across commit, reference, index, and worktree states.
    pub change_sets: Vec<ChangeSet>,
    /// Rename candidates kept separate from observable add and delete changes.
    pub rename_candidates: Vec<RenameCandidate>,
    /// Bounded submodule gitlink and checkout observations.
    pub submodules: Vec<SubmoduleState>,
    /// Ambiguity-preserving symbol-lineage candidates.
    pub lineage_candidates: Vec<SymbolLineageCandidate>,
}

impl GitSnapshotInput {
    /// Creates an empty explicit non-Git snapshot.
    #[must_use]
    pub fn non_git(repository: RepositoryId, reason: NonGitReason) -> Self {
        Self {
            version: GIT_CONTRACT_VERSION,
            repository,
            state: RepositoryState::NonGit { reason },
            worktrees: Vec::new(),
            commits: Vec::new(),
            change_sets: Vec::new(),
            rename_candidates: Vec::new(),
            submodules: Vec::new(),
            lineage_candidates: Vec::new(),
        }
    }
}

/// Validated, bounded, deterministically ordered Git evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CanonicalGitSnapshot {
    pub(crate) input: GitSnapshotInput,
}

impl CanonicalGitSnapshot {
    /// Returns the validated declarative snapshot.
    #[must_use]
    pub const fn as_input(&self) -> &GitSnapshotInput {
        &self.input
    }

    /// Serializes deterministic canonical JSON bytes.
    ///
    /// # Errors
    ///
    /// Returns [`GitContractError::SerializeSnapshot`] if the fixed validated
    /// data model cannot be serialized.
    pub fn canonical_json(&self) -> Result<Vec<u8>, GitContractError> {
        serde_json::to_vec(self).map_err(|_| GitContractError::SerializeSnapshot)
    }

    /// Hashes the deterministic canonical JSON representation.
    ///
    /// # Errors
    ///
    /// Propagates [`GitContractError::SerializeSnapshot`].
    pub fn hash(&self) -> Result<ContentHash, GitContractError> {
        self.canonical_json().map(|bytes| content_hash(&bytes))
    }
}

/// Bounded collection named by a source-free validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GitCollection {
    /// Worktree records.
    #[error("worktrees")]
    Worktrees,
    /// Commit records.
    #[error("commits")]
    Commits,
    /// Parents on one commit.
    #[error("commit parents")]
    CommitParents,
    /// Revision comparisons.
    #[error("change sets")]
    ChangeSets,
    /// File changes across all comparisons.
    #[error("file changes")]
    Changes,
    /// Changed byte spans across all file changes.
    #[error("changed spans")]
    ChangedSpans,
    /// Rename candidates.
    #[error("rename candidates")]
    RenameCandidates,
    /// Evidence classes on one rename candidate.
    #[error("rename evidence")]
    RenameEvidence,
    /// Submodule records.
    #[error("submodules")]
    Submodules,
    /// Sparse-checkout patterns on one worktree.
    #[error("sparse patterns")]
    SparsePatterns,
    /// Missing or shallow object identifiers.
    #[error("history objects")]
    HistoryObjects,
    /// Symbol-lineage candidates.
    #[error("lineage candidates")]
    LineageCandidates,
    /// Evidence classes on one lineage candidate.
    #[error("lineage evidence")]
    LineageEvidence,
}

/// Bounded text category named by a source-free validation error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GitTextKind {
    /// Stable worktree label.
    #[error("worktree label")]
    Worktree,
    /// Fully qualified Git reference.
    #[error("reference")]
    Reference,
    /// Canonical repository-relative path.
    #[error("repository path")]
    RepositoryPath,
    /// Order-sensitive sparse-checkout pattern.
    #[error("sparse pattern")]
    SparsePattern,
    /// Aggregate bytes across every retained text field.
    #[error("aggregate text")]
    Aggregate,
}

/// Stable source-free error family for importer callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitErrorCode {
    /// Configured limits are invalid.
    InvalidLimits,
    /// The input contract version is unsupported.
    UnsupportedVersion,
    /// A bounded collection exceeded its limit.
    CollectionLimit,
    /// A bounded text field or aggregate exceeded its limit.
    TextLimit,
    /// Canonicalization could not reserve bounded working memory.
    ResourceExhausted,
    /// Input text is not canonical for its category.
    InvalidText,
    /// An object identifier disagrees with repository format.
    ObjectFormatMismatch,
    /// History coverage contradicts retained facts.
    InvalidHistory,
    /// Worktree or revision state is inconsistent.
    InvalidWorktree,
    /// A file change or changed span is malformed.
    InvalidChange,
    /// Rename evidence does not match observable changes.
    InvalidRename,
    /// Submodule evidence is inconsistent.
    InvalidSubmodule,
    /// Symbol-lineage evidence is malformed.
    InvalidLineage,
    /// Canonical snapshot serialization failed.
    Serialization,
    /// Cooperative cancellation stopped validation.
    Cancelled,
}

/// Stable source-free remediation class for importer callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitNextAction {
    /// Reduce the requested history or change scope and retry.
    ReduceScope,
    /// Repair or regenerate importer output before retrying.
    RepairImporterOutput,
    /// Upgrade the importer or core to a compatible contract.
    UpgradeContract,
    /// Retry only if the owning operation is still desired.
    RetryOperation,
    /// Escalate an invariant failure because retrying unchanged input is unsafe.
    InvestigateInvariant,
}

/// Typed source-free failures returned by Git evidence validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GitContractError {
    /// One or more configured limits were outside supported ceilings.
    #[error("invalid git evidence limits")]
    InvalidLimits,
    /// The importer contract version is not supported.
    #[error("unsupported git evidence contract version {major}.{minor}")]
    UnsupportedVersion {
        /// Unsupported major version.
        major: u16,
        /// Unsupported minor version.
        minor: u16,
    },
    /// A collection exceeded a configured or hard ceiling.
    #[error("{collection} exceed the limit of {maximum}")]
    CollectionLimit {
        /// Source-free collection class.
        collection: GitCollection,
        /// Maximum accepted entries.
        maximum: usize,
    },
    /// One text field exceeded its configured byte ceiling.
    #[error("{kind} exceeds the byte limit of {maximum}")]
    TextLimit {
        /// Source-free text category.
        kind: GitTextKind,
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// One text field is not canonical for its category.
    #[error("invalid canonical {kind}")]
    InvalidText {
        /// Source-free text category.
        kind: GitTextKind,
    },
    /// An object identifier uses the wrong repository object format.
    #[error("git object identifier format does not match the repository")]
    ObjectFormatMismatch,
    /// History completeness or coverage contradicts retained records.
    #[error("git history coverage is inconsistent")]
    InvalidHistory,
    /// A worktree label, HEAD, index, or revision selector is inconsistent.
    #[error("git worktree state is inconsistent")]
    InvalidWorktree,
    /// A file-change shape or byte span is invalid.
    #[error("git file change is inconsistent")]
    InvalidChange,
    /// A half-open changed span has descending bounds.
    #[error("git changed span has invalid bounds")]
    InvalidSpan,
    /// Rename evidence does not reference a matching delete and add pair.
    #[error("git rename candidate is inconsistent")]
    InvalidRename,
    /// Submodule evidence references an unknown worktree or invalid object.
    #[error("git submodule state is inconsistent")]
    InvalidSubmodule,
    /// Symbol lineage is reflexive, unproven, or out of confidence range.
    #[error("git symbol lineage candidate is inconsistent")]
    InvalidLineage,
    /// A canonical collection contains conflicting duplicate keys.
    #[error("{collection} contain conflicting duplicate keys")]
    Duplicate {
        /// Source-free collection class.
        collection: GitCollection,
    },
    /// Canonicalization could not reserve bounded working memory.
    #[error("insufficient memory to canonicalize {collection}")]
    AllocationFailure {
        /// Source-free collection class.
        collection: GitCollection,
    },
    /// Canonical serialization failed without exposing importer text.
    #[error("failed to serialize canonical git evidence")]
    SerializeSnapshot,
    /// Cooperative cancellation stopped validation.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}

impl GitContractError {
    /// Returns a stable source-free error family.
    #[must_use]
    pub const fn code(&self) -> GitErrorCode {
        match self {
            Self::InvalidLimits => GitErrorCode::InvalidLimits,
            Self::UnsupportedVersion { .. } => GitErrorCode::UnsupportedVersion,
            Self::CollectionLimit { .. } => GitErrorCode::CollectionLimit,
            Self::TextLimit { .. } => GitErrorCode::TextLimit,
            Self::AllocationFailure { .. } => GitErrorCode::ResourceExhausted,
            Self::InvalidText { .. } => GitErrorCode::InvalidText,
            Self::ObjectFormatMismatch => GitErrorCode::ObjectFormatMismatch,
            Self::InvalidHistory => GitErrorCode::InvalidHistory,
            Self::InvalidWorktree => GitErrorCode::InvalidWorktree,
            Self::InvalidChange | Self::InvalidSpan => GitErrorCode::InvalidChange,
            Self::InvalidRename => GitErrorCode::InvalidRename,
            Self::InvalidSubmodule => GitErrorCode::InvalidSubmodule,
            Self::InvalidLineage => GitErrorCode::InvalidLineage,
            Self::Duplicate { collection } => match collection {
                GitCollection::Worktrees => GitErrorCode::InvalidWorktree,
                GitCollection::Commits
                | GitCollection::CommitParents
                | GitCollection::HistoryObjects => GitErrorCode::InvalidHistory,
                GitCollection::ChangeSets
                | GitCollection::Changes
                | GitCollection::ChangedSpans => GitErrorCode::InvalidChange,
                GitCollection::RenameCandidates | GitCollection::RenameEvidence => {
                    GitErrorCode::InvalidRename
                }
                GitCollection::Submodules => GitErrorCode::InvalidSubmodule,
                GitCollection::SparsePatterns => GitErrorCode::InvalidWorktree,
                GitCollection::LineageCandidates | GitCollection::LineageEvidence => {
                    GitErrorCode::InvalidLineage
                }
            },
            Self::SerializeSnapshot => GitErrorCode::Serialization,
            Self::Cancelled(_) => GitErrorCode::Cancelled,
        }
    }

    /// Returns whether a caller can retry after changing only scope or timing.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::CollectionLimit { .. }
                | Self::TextLimit { .. }
                | Self::AllocationFailure { .. }
                | Self::Cancelled(_)
        )
    }

    /// Returns the stable next-action class for this failure.
    #[must_use]
    pub const fn next_action(&self) -> GitNextAction {
        match self {
            Self::CollectionLimit { .. }
            | Self::TextLimit { .. }
            | Self::AllocationFailure { .. } => GitNextAction::ReduceScope,
            Self::UnsupportedVersion { .. } => GitNextAction::UpgradeContract,
            Self::Cancelled(_) => GitNextAction::RetryOperation,
            Self::SerializeSnapshot => GitNextAction::InvestigateInvariant,
            Self::InvalidLimits
            | Self::InvalidText { .. }
            | Self::ObjectFormatMismatch
            | Self::InvalidHistory
            | Self::InvalidWorktree
            | Self::InvalidChange
            | Self::InvalidSpan
            | Self::InvalidRename
            | Self::InvalidSubmodule
            | Self::InvalidLineage
            | Self::Duplicate { .. } => GitNextAction::RepairImporterOutput,
        }
    }
}
