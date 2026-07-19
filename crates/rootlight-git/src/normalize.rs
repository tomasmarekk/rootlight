//! Validation and deterministic normalization for hostile Git importer data.
//!
//! This module owns cross-record invariants and source-free limit failures;
//! it performs no repository, filesystem, process, hook, or network access.

use std::collections::BTreeSet;

use rootlight_cancel::Cancellation;

use crate::model::{
    CanonicalGitSnapshot, ChangeSet, FileChange, FileChangeKind, GIT_CONTRACT_VERSION,
    GitCollection, GitContractError, GitLimits, GitRepositoryState, GitSnapshotInput, GitTextKind,
    HeadState, HistoryGapReason, HistoryState, ObjectFormat, ObjectId, RenameCandidate,
    RepositoryState, RevisionSelector, SparseCheckoutState, SubmoduleState, SymbolLineageCandidate,
    WorktreeState,
};

const MAX_WORKTREES: usize = 256;
const MAX_CHANGE_SETS: usize = 4_096;
const MAX_COMMIT_PARENTS: usize = 64;
const MAX_HISTORY_OBJECTS: usize = 100_000;
const MAX_HISTORY_TRUNCATIONS: usize = 6;
const MAX_SPARSE_PATTERNS: usize = 16_384;
const MAX_RENAME_EVIDENCE: usize = 16;
const MAX_LINEAGE_EVIDENCE: usize = 16;
const MAX_SUBMODULES: usize = 100_000;
const CANCELLATION_INTERVAL: usize = 256;
const MAX_CONFIDENCE_BPS: u16 = 10_000;

pub(crate) fn canonicalize(
    mut input: GitSnapshotInput,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<CanonicalGitSnapshot, GitContractError> {
    cancellation.check()?;
    if input.version != GIT_CONTRACT_VERSION {
        return Err(GitContractError::UnsupportedVersion {
            major: input.version.major,
            minor: input.version.minor,
        });
    }

    if matches!(&input.state, RepositoryState::NonGit { .. }) {
        validate_non_git(&input)?;
    } else {
        let GitSnapshotInput {
            state,
            worktrees,
            commits,
            change_sets,
            rename_candidates,
            submodules,
            lineage_candidates,
            ..
        } = &mut input;
        if let RepositoryState::Git(state) = state {
            let mut text = TextBudget::new(limits);
            let payload = GitPayload {
                worktrees,
                commits,
                change_sets,
                rename_candidates,
                submodules,
                lineage_candidates,
            };
            normalize_git_state(state, payload, limits, &mut text, cancellation)?;
        }
    }
    cancellation.check()?;
    Ok(CanonicalGitSnapshot { input })
}

struct GitPayload<'a> {
    worktrees: &'a mut Vec<WorktreeState>,
    commits: &'a mut Vec<crate::model::CommitRecord>,
    change_sets: &'a mut Vec<ChangeSet>,
    rename_candidates: &'a mut Vec<RenameCandidate>,
    submodules: &'a mut Vec<SubmoduleState>,
    lineage_candidates: &'a mut Vec<SymbolLineageCandidate>,
}

fn validate_non_git(input: &GitSnapshotInput) -> Result<(), GitContractError> {
    if input.worktrees.is_empty()
        && input.commits.is_empty()
        && input.change_sets.is_empty()
        && input.rename_candidates.is_empty()
        && input.submodules.is_empty()
        && input.lineage_candidates.is_empty()
    {
        Ok(())
    } else {
        Err(GitContractError::InvalidHistory)
    }
}

fn normalize_git_state(
    state: &mut GitRepositoryState,
    payload: GitPayload<'_>,
    limits: &GitLimits,
    text: &mut TextBudget<'_>,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    normalize_history(state, payload.commits, limits, cancellation)?;
    let worktree_ids =
        normalize_worktrees(payload.worktrees, state.object_format, text, cancellation)?;
    normalize_commits(payload.commits, state.object_format, limits, cancellation)?;
    normalize_change_sets(
        payload.change_sets,
        state.object_format,
        &worktree_ids,
        limits,
        text,
        cancellation,
    )?;
    normalize_renames(
        payload.rename_candidates,
        payload.change_sets,
        state.object_format,
        &worktree_ids,
        limits,
        text,
        cancellation,
    )?;
    normalize_submodules(
        payload.submodules,
        state.object_format,
        &worktree_ids,
        text,
        cancellation,
    )?;
    normalize_lineage(payload.lineage_candidates, limits, cancellation)?;
    Ok(())
}

fn normalize_history(
    state: &mut GitRepositoryState,
    commits: &[crate::model::CommitRecord],
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    ensure_count(commits.len(), limits.max_commits, GitCollection::Commits)?;
    let imported = usize::try_from(state.coverage.imported_commits)
        .map_err(|_| GitContractError::InvalidHistory)?;
    let requested = usize::try_from(state.coverage.requested_commit_limit)
        .map_err(|_| GitContractError::InvalidHistory)?;
    if imported != commits.len()
        || imported > requested
        || requested == 0
        || requested > limits.max_commits
        || (commits.is_empty() && state.coverage.oldest_imported_time_unix_seconds.is_some())
    {
        return Err(GitContractError::InvalidHistory);
    }
    ensure_count(
        state.coverage.truncation.len(),
        MAX_HISTORY_TRUNCATIONS,
        GitCollection::HistoryObjects,
    )?;
    state.coverage.truncation.sort_unstable();
    state.coverage.truncation.dedup();

    match &mut state.history {
        HistoryState::Complete => {}
        HistoryState::Shallow { boundary_commits } => {
            if !state
                .coverage
                .truncation
                .contains(&crate::model::HistoryTruncation::ShallowBoundary)
            {
                return Err(GitContractError::InvalidHistory);
            }
            ensure_count(
                boundary_commits.len(),
                MAX_HISTORY_OBJECTS,
                GitCollection::HistoryObjects,
            )?;
            if boundary_commits.is_empty() {
                return Err(GitContractError::InvalidHistory);
            }
            validate_object_list(boundary_commits, state.object_format, cancellation)?;
            sort_vec_cancellable(
                boundary_commits,
                cancellation,
                GitCollection::HistoryObjects,
            )?;
            boundary_commits.dedup();
        }
        HistoryState::Incomplete {
            reason,
            missing_objects,
        } => {
            if *reason == HistoryGapReason::MissingObjects
                && !state
                    .coverage
                    .truncation
                    .contains(&crate::model::HistoryTruncation::MissingObjects)
            {
                return Err(GitContractError::InvalidHistory);
            }
            ensure_count(
                missing_objects.len(),
                MAX_HISTORY_OBJECTS,
                GitCollection::HistoryObjects,
            )?;
            if *reason == HistoryGapReason::MissingObjects && missing_objects.is_empty() {
                return Err(GitContractError::InvalidHistory);
            }
            validate_object_list(missing_objects, state.object_format, cancellation)?;
            sort_vec_cancellable(missing_objects, cancellation, GitCollection::HistoryObjects)?;
            missing_objects.dedup();
        }
    }
    Ok(())
}

fn validate_object_list(
    objects: &[ObjectId],
    format: ObjectFormat,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    for (index, object) in objects.iter().enumerate() {
        checkpoint(cancellation, index)?;
        validate_object(*object, format)?;
    }
    Ok(())
}

fn normalize_worktrees(
    worktrees: &mut [WorktreeState],
    format: ObjectFormat,
    text: &mut TextBudget<'_>,
    cancellation: &Cancellation,
) -> Result<BTreeSet<String>, GitContractError> {
    ensure_count(worktrees.len(), MAX_WORKTREES, GitCollection::Worktrees)?;
    let mut identifiers = BTreeSet::new();
    for (index, worktree) in worktrees.iter_mut().enumerate() {
        checkpoint(cancellation, index)?;
        text.consume(GitTextKind::Worktree, &worktree.id)?;
        if !valid_label(&worktree.id) {
            return Err(GitContractError::InvalidText {
                kind: GitTextKind::Worktree,
            });
        }
        if !identifiers.insert(worktree.id.clone()) {
            return Err(GitContractError::Duplicate {
                collection: GitCollection::Worktrees,
            });
        }
        validate_head(&worktree.head, format, text)?;
        if let Some(tree) = worktree.index_tree {
            validate_object(tree, format)?;
        }
        normalize_sparse(&mut worktree.sparse_checkout, text, cancellation)?;
    }
    worktrees.sort_unstable();
    cancellation.check()?;
    Ok(identifiers)
}

fn validate_head(
    head: &HeadState,
    format: ObjectFormat,
    text: &mut TextBudget<'_>,
) -> Result<(), GitContractError> {
    match head {
        HeadState::Unborn { reference } => {
            text.consume(GitTextKind::Reference, reference)?;
            validate_reference(reference)
        }
        HeadState::Branch { reference, commit } => {
            text.consume(GitTextKind::Reference, reference)?;
            validate_reference(reference)?;
            validate_object(*commit, format)
        }
        HeadState::Detached { commit } => validate_object(*commit, format),
        HeadState::Unavailable { .. } => Ok(()),
    }
}

fn normalize_sparse(
    state: &mut SparseCheckoutState,
    text: &mut TextBudget<'_>,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    let SparseCheckoutState::Enabled { patterns, .. } = state else {
        return Ok(());
    };
    ensure_count(
        patterns.len(),
        MAX_SPARSE_PATTERNS,
        GitCollection::SparsePatterns,
    )?;
    for (index, pattern) in patterns.iter().enumerate() {
        checkpoint(cancellation, index)?;
        text.consume(GitTextKind::SparsePattern, pattern)?;
        if !valid_general_text(pattern) {
            return Err(GitContractError::InvalidText {
                kind: GitTextKind::SparsePattern,
            });
        }
    }
    Ok(())
}

fn normalize_commits(
    commits: &mut Vec<crate::model::CommitRecord>,
    format: ObjectFormat,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    ensure_count(commits.len(), limits.max_commits, GitCollection::Commits)?;
    for (index, commit) in commits.iter().enumerate() {
        checkpoint(cancellation, index)?;
        validate_object(commit.id, format)?;
        validate_object(commit.tree, format)?;
        ensure_count(
            commit.parents.len(),
            MAX_COMMIT_PARENTS,
            GitCollection::CommitParents,
        )?;
        let mut parents = BTreeSet::new();
        for parent in &commit.parents {
            validate_object(*parent, format)?;
            if !parents.insert(*parent) {
                return Err(GitContractError::Duplicate {
                    collection: GitCollection::CommitParents,
                });
            }
        }
    }
    sort_vec_cancellable(commits, cancellation, GitCollection::Commits)?;
    ensure_unique_by(commits, |commit| commit.id, GitCollection::Commits)?;
    cancellation.check()?;
    Ok(())
}

fn normalize_change_sets(
    change_sets: &mut Vec<ChangeSet>,
    format: ObjectFormat,
    worktrees: &BTreeSet<String>,
    limits: &GitLimits,
    text: &mut TextBudget<'_>,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    ensure_count(
        change_sets.len(),
        MAX_CHANGE_SETS,
        GitCollection::ChangeSets,
    )?;
    let mut total_changes = 0usize;
    let mut total_spans = 0usize;
    for (set_index, change_set) in change_sets.iter_mut().enumerate() {
        checkpoint(cancellation, set_index)?;
        validate_selector(&change_set.base, format, worktrees, text)?;
        validate_selector(&change_set.head, format, worktrees, text)?;
        total_changes = total_changes.checked_add(change_set.changes.len()).ok_or(
            GitContractError::CollectionLimit {
                collection: GitCollection::Changes,
                maximum: limits.max_changes,
            },
        )?;
        ensure_count(total_changes, limits.max_changes, GitCollection::Changes)?;
        for change in &mut change_set.changes {
            validate_change(change, text)?;
            total_spans = total_spans.checked_add(change.spans.len()).ok_or(
                GitContractError::CollectionLimit {
                    collection: GitCollection::ChangedSpans,
                    maximum: limits.max_changed_spans,
                },
            )?;
            ensure_count(
                total_spans,
                limits.max_changed_spans,
                GitCollection::ChangedSpans,
            )?;
            sort_vec_cancellable(&mut change.spans, cancellation, GitCollection::ChangedSpans)?;
            change.spans.dedup();
        }
        sort_vec_cancellable(
            &mut change_set.changes,
            cancellation,
            GitCollection::Changes,
        )?;
        for pair in change_set.changes.windows(2) {
            let [left, right] = pair else {
                continue;
            };
            if left.kind == right.kind
                && left.before_path == right.before_path
                && left.after_path == right.after_path
            {
                return Err(GitContractError::Duplicate {
                    collection: GitCollection::Changes,
                });
            }
        }
    }
    sort_vec_cancellable(change_sets, cancellation, GitCollection::ChangeSets)?;
    for pair in change_sets.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if left.base == right.base && left.head == right.head {
            return Err(GitContractError::Duplicate {
                collection: GitCollection::ChangeSets,
            });
        }
    }
    cancellation.check()?;
    Ok(())
}

fn validate_selector(
    selector: &RevisionSelector,
    format: ObjectFormat,
    worktrees: &BTreeSet<String>,
    text: &mut TextBudget<'_>,
) -> Result<(), GitContractError> {
    match selector {
        RevisionSelector::Commit(commit) => validate_object(*commit, format),
        RevisionSelector::Reference { name, target } => {
            text.consume(GitTextKind::Reference, name)?;
            validate_reference(name)?;
            validate_object(*target, format)
        }
        RevisionSelector::Head { worktree }
        | RevisionSelector::Index { worktree }
        | RevisionSelector::WorkingTree { worktree } => {
            text.consume(GitTextKind::Worktree, worktree)?;
            if valid_label(worktree) && worktrees.contains(worktree) {
                Ok(())
            } else {
                Err(GitContractError::InvalidWorktree)
            }
        }
    }
}

fn validate_change(change: &FileChange, text: &mut TextBudget<'_>) -> Result<(), GitContractError> {
    if let Some(path) = &change.before_path {
        text.consume(GitTextKind::RepositoryPath, path)?;
        validate_repository_path(path)?;
    }
    if let Some(path) = &change.after_path {
        text.consume(GitTextKind::RepositoryPath, path)?;
        validate_repository_path(path)?;
    }
    let shape_valid = match change.kind {
        FileChangeKind::Added => change.before_path.is_none() && change.after_path.is_some(),
        FileChangeKind::Deleted => change.before_path.is_some() && change.after_path.is_none(),
        FileChangeKind::Modified | FileChangeKind::TypeChanged => {
            change.before_path.is_some() && change.before_path == change.after_path
        }
    };
    if !shape_valid {
        return Err(GitContractError::InvalidChange);
    }
    for span in &change.spans {
        if span.before.is_none() && span.after.is_none() {
            return Err(GitContractError::InvalidChange);
        }
        for coordinate in [span.before, span.after].into_iter().flatten() {
            if coordinate.end < coordinate.start {
                return Err(GitContractError::InvalidSpan);
            }
        }
        match change.kind {
            FileChangeKind::Added if span.before.is_some() || span.after.is_none() => {
                return Err(GitContractError::InvalidChange);
            }
            FileChangeKind::Deleted if span.before.is_none() || span.after.is_some() => {
                return Err(GitContractError::InvalidChange);
            }
            FileChangeKind::Added
            | FileChangeKind::Deleted
            | FileChangeKind::Modified
            | FileChangeKind::TypeChanged => {}
        }
    }
    Ok(())
}

fn normalize_renames(
    candidates: &mut Vec<RenameCandidate>,
    change_sets: &[ChangeSet],
    format: ObjectFormat,
    worktrees: &BTreeSet<String>,
    limits: &GitLimits,
    text: &mut TextBudget<'_>,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    ensure_count(
        candidates.len(),
        limits.max_changes,
        GitCollection::RenameCandidates,
    )?;
    for (index, candidate) in candidates.iter_mut().enumerate() {
        checkpoint(cancellation, index)?;
        if candidate.confidence_bps > MAX_CONFIDENCE_BPS || candidate.evidence.is_empty() {
            return Err(GitContractError::InvalidRename);
        }
        ensure_count(
            candidate.evidence.len(),
            MAX_RENAME_EVIDENCE,
            GitCollection::RenameEvidence,
        )?;
        candidate.evidence.sort_unstable();
        candidate.evidence.dedup();
        validate_selector(&candidate.base, format, worktrees, text)?;
        validate_selector(&candidate.head, format, worktrees, text)?;
        text.consume(GitTextKind::RepositoryPath, &candidate.before_path)?;
        text.consume(GitTextKind::RepositoryPath, &candidate.after_path)?;
        validate_repository_path(&candidate.before_path)?;
        validate_repository_path(&candidate.after_path)?;

        let comparison = change_sets.iter().find(|change_set| {
            change_set.base == candidate.base && change_set.head == candidate.head
        });
        let Some(comparison) = comparison else {
            return Err(GitContractError::InvalidRename);
        };
        let has_delete = comparison.changes.iter().any(|change| {
            change.kind == FileChangeKind::Deleted
                && change.before_path.as_deref() == Some(candidate.before_path.as_str())
        });
        let has_add = comparison.changes.iter().any(|change| {
            change.kind == FileChangeKind::Added
                && change.after_path.as_deref() == Some(candidate.after_path.as_str())
        });
        if !has_delete || !has_add {
            return Err(GitContractError::InvalidRename);
        }
    }
    sort_vec_cancellable(candidates, cancellation, GitCollection::RenameCandidates)?;
    candidates.dedup();
    cancellation.check()?;
    Ok(())
}

fn normalize_submodules(
    submodules: &mut Vec<SubmoduleState>,
    format: ObjectFormat,
    worktrees: &BTreeSet<String>,
    text: &mut TextBudget<'_>,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    ensure_count(submodules.len(), MAX_SUBMODULES, GitCollection::Submodules)?;
    let mut keys = BTreeSet::new();
    for (index, submodule) in submodules.iter().enumerate() {
        checkpoint(cancellation, index)?;
        text.consume(GitTextKind::Worktree, &submodule.worktree)?;
        text.consume(GitTextKind::RepositoryPath, &submodule.path)?;
        if !worktrees.contains(&submodule.worktree) || !valid_label(&submodule.worktree) {
            return Err(GitContractError::InvalidSubmodule);
        }
        validate_repository_path(&submodule.path)?;
        validate_object(submodule.recorded_commit, format)?;
        if !keys.insert((submodule.worktree.as_str(), submodule.path.as_str())) {
            return Err(GitContractError::Duplicate {
                collection: GitCollection::Submodules,
            });
        }
    }
    sort_vec_cancellable(submodules, cancellation, GitCollection::Submodules)?;
    cancellation.check()?;
    Ok(())
}

fn normalize_lineage(
    candidates: &mut Vec<SymbolLineageCandidate>,
    limits: &GitLimits,
    cancellation: &Cancellation,
) -> Result<(), GitContractError> {
    ensure_count(
        candidates.len(),
        limits.max_lineage_candidates,
        GitCollection::LineageCandidates,
    )?;
    for (index, candidate) in candidates.iter_mut().enumerate() {
        checkpoint(cancellation, index)?;
        if candidate.prior == candidate.current
            || candidate.confidence_bps > MAX_CONFIDENCE_BPS
            || candidate.evidence.is_empty()
        {
            return Err(GitContractError::InvalidLineage);
        }
        ensure_count(
            candidate.evidence.len(),
            MAX_LINEAGE_EVIDENCE,
            GitCollection::LineageEvidence,
        )?;
        candidate.evidence.sort_unstable();
        candidate.evidence.dedup();
    }
    sort_vec_cancellable(candidates, cancellation, GitCollection::LineageCandidates)?;
    candidates.dedup();
    cancellation.check()?;
    Ok(())
}

fn validate_object(object: ObjectId, format: ObjectFormat) -> Result<(), GitContractError> {
    if object.format() == format {
        Ok(())
    } else {
        Err(GitContractError::ObjectFormatMismatch)
    }
}

fn validate_reference(reference: &str) -> Result<(), GitContractError> {
    let invalid = !reference.starts_with("refs/")
        || reference.ends_with('/')
        || reference.contains("//")
        || reference.contains("..")
        || reference.contains("@{")
        || reference
            .chars()
            .any(|character| character.is_control() || " ~^:?*[\\".contains(character));
    if invalid {
        Err(GitContractError::InvalidText {
            kind: GitTextKind::Reference,
        })
    } else {
        Ok(())
    }
}

fn validate_repository_path(path: &str) -> Result<(), GitContractError> {
    let invalid = path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || !valid_general_text(path)
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."));
    if invalid {
        Err(GitContractError::InvalidText {
            kind: GitTextKind::RepositoryPath,
        })
    } else {
        Ok(())
    }
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b':'))
}

fn valid_general_text(value: &str) -> bool {
    !value.is_empty() && value.trim() == value && !value.chars().any(char::is_control)
}

fn checkpoint(cancellation: &Cancellation, index: usize) -> Result<(), GitContractError> {
    if index.is_multiple_of(CANCELLATION_INTERVAL) {
        cancellation.check()?;
    }
    Ok(())
}

fn ensure_count(
    actual: usize,
    maximum: usize,
    collection: GitCollection,
) -> Result<(), GitContractError> {
    if actual > maximum {
        Err(GitContractError::CollectionLimit {
            collection,
            maximum,
        })
    } else {
        Ok(())
    }
}

fn ensure_unique_by<T, K: PartialEq>(
    values: &[T],
    key: impl Fn(&T) -> K,
    collection: GitCollection,
) -> Result<(), GitContractError> {
    for pair in values.windows(2) {
        let [left, right] = pair else {
            continue;
        };
        if key(left) == key(right) {
            return Err(GitContractError::Duplicate { collection });
        }
    }
    Ok(())
}

fn sort_vec_cancellable<T: Ord>(
    values: &mut Vec<T>,
    cancellation: &Cancellation,
    collection: GitCollection,
) -> Result<(), GitContractError> {
    cancellation.check()?;
    if values.len() <= CANCELLATION_INTERVAL {
        values.sort_unstable();
        cancellation.check()?;
        return Ok(());
    }

    let input = std::mem::take(values);
    let run_count = input.len().div_ceil(CANCELLATION_INTERVAL);
    let mut runs = Vec::new();
    reserve_exact(&mut runs, run_count, collection)?;
    let mut run = Vec::new();
    reserve_exact(&mut run, CANCELLATION_INTERVAL, collection)?;
    for (index, value) in input.into_iter().enumerate() {
        checkpoint(cancellation, index)?;
        run.push(value);
        if run.len() == CANCELLATION_INTERVAL {
            run.sort_unstable();
            runs.push(run);
            run = Vec::new();
            reserve_exact(&mut run, CANCELLATION_INTERVAL, collection)?;
        }
    }
    if !run.is_empty() {
        run.sort_unstable();
        runs.push(run);
    }

    while runs.len() > 1 {
        cancellation.check()?;
        let next_count = runs.len().div_ceil(2);
        let mut next_runs = Vec::new();
        reserve_exact(&mut next_runs, next_count, collection)?;
        let mut current = runs.into_iter();
        while let Some(left) = current.next() {
            cancellation.check()?;
            if let Some(right) = current.next() {
                next_runs.push(merge_runs(left, right, cancellation, collection)?);
            } else {
                next_runs.push(left);
            }
        }
        runs = next_runs;
    }
    if let Some(sorted) = runs.pop() {
        *values = sorted;
    }
    cancellation.check()?;
    Ok(())
}

fn merge_runs<T: Ord>(
    left: Vec<T>,
    right: Vec<T>,
    cancellation: &Cancellation,
    collection: GitCollection,
) -> Result<Vec<T>, GitContractError> {
    let capacity = left
        .len()
        .checked_add(right.len())
        .ok_or(GitContractError::AllocationFailure { collection })?;
    let mut merged = Vec::new();
    reserve_exact(&mut merged, capacity, collection)?;
    let mut left = left.into_iter().peekable();
    let mut right = right.into_iter().peekable();
    let mut index = 0usize;
    loop {
        checkpoint(cancellation, index)?;
        match (left.peek(), right.peek()) {
            (Some(left_value), Some(right_value)) => {
                if left_value <= right_value {
                    if let Some(value) = left.next() {
                        merged.push(value);
                    }
                } else if let Some(value) = right.next() {
                    merged.push(value);
                }
            }
            (Some(_), None) => {
                for value in left {
                    checkpoint(cancellation, index)?;
                    merged.push(value);
                    index = index.saturating_add(1);
                }
                break;
            }
            (None, Some(_)) => {
                for value in right {
                    checkpoint(cancellation, index)?;
                    merged.push(value);
                    index = index.saturating_add(1);
                }
                break;
            }
            (None, None) => break,
        }
        index = index.saturating_add(1);
    }
    Ok(merged)
}

fn reserve_exact<T>(
    values: &mut Vec<T>,
    additional: usize,
    collection: GitCollection,
) -> Result<(), GitContractError> {
    values
        .try_reserve_exact(additional)
        .map_err(|_| GitContractError::AllocationFailure { collection })
}

struct TextBudget<'a> {
    limits: &'a GitLimits,
    total: usize,
}

impl<'a> TextBudget<'a> {
    fn new(limits: &'a GitLimits) -> Self {
        Self { limits, total: 0 }
    }

    fn consume(&mut self, kind: GitTextKind, value: &str) -> Result<(), GitContractError> {
        if value.len() > self.limits.max_text_field_bytes {
            return Err(GitContractError::TextLimit {
                kind,
                maximum: self.limits.max_text_field_bytes,
            });
        }
        self.total = self
            .total
            .checked_add(value.len())
            .ok_or(GitContractError::TextLimit {
                kind: GitTextKind::Aggregate,
                maximum: self.limits.max_total_text_bytes,
            })?;
        if self.total > self.limits.max_total_text_bytes {
            return Err(GitContractError::TextLimit {
                kind: GitTextKind::Aggregate,
                maximum: self.limits.max_total_text_bytes,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cmp::Ordering,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AtomicOrdering},
        },
    };

    use rootlight_cancel::CancellationReason;

    use super::*;

    #[test]
    fn checkpointed_merge_sort_matches_total_order() {
        let mut values: Vec<_> = (0_u32..1_000).rev().collect();

        sort_vec_cancellable(&mut values, &Cancellation::new(), GitCollection::Commits)
            .expect("bounded sort completes");

        assert!(values.windows(2).all(|pair| {
            let [left, right] = pair else {
                return true;
            };
            left <= right
        }));
    }

    #[test]
    fn checkpointed_merge_sort_observes_mid_sort_cancellation() {
        let cancellation = Cancellation::new();
        let comparisons = Arc::new(AtomicUsize::new(0));
        let mut values: Vec<_> = (0_u32..1_000)
            .rev()
            .map(|value| CancellingOrd {
                value,
                cancellation: cancellation.clone(),
                comparisons: Arc::clone(&comparisons),
            })
            .collect();

        assert!(matches!(
            sort_vec_cancellable(&mut values, &cancellation, GitCollection::Commits),
            Err(GitContractError::Cancelled(_))
        ));
        assert!(comparisons.load(AtomicOrdering::Relaxed) > 0);
    }

    #[derive(Debug)]
    struct CancellingOrd {
        value: u32,
        cancellation: Cancellation,
        comparisons: Arc<AtomicUsize>,
    }

    impl PartialEq for CancellingOrd {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Eq for CancellingOrd {}

    impl PartialOrd for CancellingOrd {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for CancellingOrd {
        fn cmp(&self, other: &Self) -> Ordering {
            if self.comparisons.fetch_add(1, AtomicOrdering::Relaxed) == 0 {
                let _ = self.cancellation.cancel(CancellationReason::ClientRequest);
            }
            self.value.cmp(&other.value)
        }
    }
}
