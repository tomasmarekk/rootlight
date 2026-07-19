//! Bounded declarative contracts for read-only Git and worktree evidence.
//!
//! Validates future audited-importer facts without repository I/O, hooks,
//! network, source bodies, or a decoder that could allocate before limits.

#![forbid(unsafe_code)]

mod model;
mod normalize;

pub use model::{
    ByteSpan, CandidateGroupId, CanonicalGitSnapshot, ChangeSet, ChangedSpan, CommitRecord,
    FileChange, FileChangeKind, GIT_CONTRACT_VERSION, GitCollection, GitContractError,
    GitContractVersion, GitErrorCode, GitLimits, GitNextAction, GitRepositoryState,
    GitSnapshotInput, GitTextKind, HARD_MAX_CHANGED_SPANS, HARD_MAX_CHANGES, HARD_MAX_COMMITS,
    HARD_MAX_LINEAGE_CANDIDATES, HARD_MAX_TEXT_FIELD_BYTES, HARD_MAX_TOTAL_TEXT_BYTES, HeadState,
    HeadUnavailableReason, HistoryCoverage, HistoryGapReason, HistoryState, HistoryTruncation,
    LineageEvidenceKind, LineageKind, NonGitReason, ObjectDatabaseId, ObjectFormat, ObjectId,
    RenameCandidate, RenameEvidenceKind, RepositoryState, RevisionSelector, SparseCheckoutState,
    SubmoduleCheckoutState, SubmoduleState, SymbolLineageCandidate, WorktreeState, WorktreeStatus,
};

use rootlight_cancel::Cancellation;

/// Validates and canonicalizes one importer-provided Git evidence snapshot.
///
/// The returned value has deterministic collection ordering while preserving
/// order-sensitive commit parents and sparse-checkout patterns. Rename and
/// lineage evidence remains candidate data and is never promoted to exact
/// identity by this function.
///
/// # Errors
///
/// Returns [`GitContractError`] for incompatible versions, malformed or
/// inconsistent evidence, configured resource limits, serialization failure,
/// or cooperative cancellation.
pub fn canonicalize_snapshot(
    input: GitSnapshotInput,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<CanonicalGitSnapshot, GitContractError> {
    normalize::canonicalize(input, limits, cancellation)
}
