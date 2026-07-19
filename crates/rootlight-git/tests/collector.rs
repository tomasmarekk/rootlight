//! Integration tests for the fixed-command read-only Git collector.
//!
//! Temporary repositories use English-only fixtures and assert source-free,
//! bounded evidence without depending on the Rootlight checkout itself.

use std::{fs, path::Path, process::Command, time::Duration};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_git::{
    FileChangeKind, GitCollectErrorCode, GitCollectLimits, GitLimits, HeadState, HistoryState,
    HistoryTruncation, ObjectFormat, RenameEvidenceKind, RepositoryState, SubmoduleCheckoutState,
    collect_repository,
};
use rootlight_ids::{RepositoryId, derive_repository};
use tempfile::TempDir;

fn repository() -> RepositoryId {
    derive_repository(b"git-collector-test-repository").id()
}

fn run_git(root: &Path, arguments: &[&str]) {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("test Git command starts");
    assert!(
        output.status.success(),
        "test Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(root: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("test Git command starts");
    assert!(
        output.status.success(),
        "test Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("test Git output is UTF-8")
        .trim()
        .to_owned()
}

fn initialized_repository() -> TempDir {
    let directory = tempfile::tempdir().expect("temporary repository is created");
    run_git(directory.path(), &["init", "--quiet"]);
    run_git(directory.path(), &["config", "user.name", "Rootlight Test"]);
    run_git(
        directory.path(),
        &["config", "user.email", "rootlight@example.invalid"],
    );
    directory
}

fn write_and_commit(root: &Path, path: &str, body: &str, message: &str) {
    fs::write(root.join(path), body).expect("fixture source is written");
    run_git(root, &["add", "--", path]);
    run_git(root, &["commit", "--quiet", "-m", message]);
}

fn collection_limits(history_commits: usize) -> GitCollectLimits {
    GitCollectLimits::new(history_commits, 2 * 1024 * 1024, Duration::from_secs(10))
        .expect("fixture collection limits are valid")
}

#[test]
fn collects_head_history_status_and_worktree_changes() {
    let directory = initialized_repository();
    write_and_commit(
        directory.path(),
        "tracked.rs",
        "pub fn answer() -> u32 { 42 }\n",
        "initial",
    );
    write_and_commit(
        directory.path(),
        ".gitattributes",
        "tracked.rs filter=rootlight\n",
        "attributes",
    );
    run_git(
        directory.path(),
        &[
            "config",
            "filter.rootlight.clean",
            "echo invoked > rootlight-filter-marker && cat",
        ],
    );
    run_git(
        directory.path(),
        &[
            "config",
            "filter.rootlight.process",
            "rootlight-helper-must-not-run",
        ],
    );
    run_git(
        directory.path(),
        &["config", "filter.rootlight.required", "true"],
    );
    fs::write(
        directory.path().join("tracked.rs"),
        "pub fn answer() -> u32 { 43 }\n",
    )
    .expect("tracked fixture is modified");
    fs::write(
        directory.path().join("untracked.rs"),
        "pub fn pending() {}\n",
    )
    .expect("untracked fixture is written");

    // These repository settings would execute helpers if collection trusted
    // hostile config instead of applying its fixed fail-closed overrides.
    run_git(
        directory.path(),
        &["config", "diff.external", "rootlight-helper-must-not-run"],
    );
    run_git(
        directory.path(),
        &["config", "core.fsmonitor", "rootlight-helper-must-not-run"],
    );

    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("read-only collection succeeds");
    let input = snapshot.as_input();
    assert!(matches!(input.state, RepositoryState::Git(_)));
    assert_eq!(input.commits.len(), 2);
    assert_eq!(input.worktrees.len(), 1);
    assert_eq!(input.worktrees[0].status.tracked_changes, 1);
    assert_eq!(input.worktrees[0].status.untracked_paths, 1);
    assert!(input.change_sets.iter().any(|set| {
        set.changes.iter().any(|change| {
            change.kind == FileChangeKind::Modified
                && change.after_path.as_deref() == Some("tracked.rs")
        })
    }));
    assert!(input.change_sets.iter().any(|set| {
        set.changes.iter().any(|change| {
            change.kind == FileChangeKind::Added
                && change.after_path.as_deref() == Some("untracked.rs")
        })
    }));
    assert!(
        !directory.path().join("rootlight-filter-marker").exists(),
        "collection must not execute repository filter commands"
    );
}

#[test]
fn history_window_is_bounded_and_reports_truncation() {
    let directory = initialized_repository();
    write_and_commit(directory.path(), "one.rs", "fn one() {}\n", "one");
    write_and_commit(directory.path(), "two.rs", "fn two() {}\n", "two");
    write_and_commit(directory.path(), "three.rs", "fn three() {}\n", "three");

    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(2),
        &Cancellation::new(),
    )
    .expect("bounded history collection succeeds");
    let RepositoryState::Git(state) = &snapshot.as_input().state else {
        panic!("fixture must be a Git repository");
    };
    assert_eq!(snapshot.as_input().commits.len(), 2);
    assert_eq!(state.coverage.imported_commits, 2);
    assert_eq!(state.coverage.requested_commit_limit, 2);
    assert!(
        state
            .coverage
            .truncation
            .contains(&HistoryTruncation::CommitLimit)
    );
}

#[test]
fn unborn_repository_reports_indexed_files_as_staged_additions() {
    let directory = initialized_repository();
    fs::write(
        directory.path().join("staged.rs"),
        "pub fn staged_before_first_commit() {}\n",
    )
    .expect("unborn fixture source is written");
    run_git(directory.path(), &["add", "--", "staged.rs"]);

    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("unborn repository collection succeeds");
    let input = snapshot.as_input();
    assert!(matches!(input.worktrees[0].head, HeadState::Unborn { .. }));
    assert!(input.change_sets.iter().any(|set| {
        matches!(set.base, rootlight_git::RevisionSelector::Head { .. })
            && matches!(set.head, rootlight_git::RevisionSelector::Index { .. })
            && set.changes.iter().any(|change| {
                change.kind == FileChangeKind::Added
                    && change.after_path.as_deref() == Some("staged.rs")
            })
    }));
}

#[test]
fn rename_signal_remains_candidate_evidence_beside_add_and_delete() {
    let directory = initialized_repository();
    write_and_commit(
        directory.path(),
        "before.rs",
        "pub fn preserved() {}\n",
        "initial",
    );
    fs::rename(
        directory.path().join("before.rs"),
        directory.path().join("after.rs"),
    )
    .expect("fixture path is renamed");
    run_git(directory.path(), &["add", "--all"]);

    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("rename collection succeeds");
    let input = snapshot.as_input();
    assert!(input.change_sets.iter().any(|set| {
        set.changes
            .iter()
            .any(|change| change.kind == FileChangeKind::Added)
            && set
                .changes
                .iter()
                .any(|change| change.kind == FileChangeKind::Deleted)
    }));
    assert!(input.rename_candidates.iter().any(|candidate| {
        candidate.before_path == "before.rs"
            && candidate.after_path == "after.rs"
            && candidate
                .evidence
                .contains(&RenameEvidenceKind::ImporterSignal)
    }));
}

#[test]
fn non_git_root_is_explicit_and_pre_cancelled_collection_spawns_nothing() {
    let directory = tempfile::tempdir().expect("temporary non-Git root is created");
    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("non-Git root has an explicit snapshot");
    assert!(matches!(
        snapshot.as_input().state,
        RepositoryState::NonGit { .. }
    ));

    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    let error = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &cancellation,
    )
    .expect_err("pre-cancelled collection fails before command execution");
    assert_eq!(error.code(), GitCollectErrorCode::Cancelled);
}

#[test]
fn command_bytes_and_contract_collections_fail_at_configured_limits() {
    let directory = initialized_repository();
    write_and_commit(
        directory.path(),
        "tracked.rs",
        "fn tracked() {}\n",
        "initial",
    );

    let tiny_output = GitCollectLimits::new(8, 1, Duration::from_secs(10))
        .expect("one-byte command limit is structurally valid");
    let output_error = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        tiny_output,
        &Cancellation::new(),
    )
    .expect_err("repository probe exceeds one retained byte");
    assert_eq!(output_error.code(), GitCollectErrorCode::CommandOutputLimit);

    fs::write(directory.path().join("first.rs"), "fn first() {}\n")
        .expect("first untracked fixture is written");
    fs::write(directory.path().join("second.rs"), "fn second() {}\n")
        .expect("second untracked fixture is written");
    let contract_limits = GitLimits::new(8, 1, 8, 8, 4 * 1024, 64 * 1024)
        .expect("narrow declarative limits are valid");
    let contract_error = collect_repository(
        directory.path(),
        repository(),
        &contract_limits,
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect_err("untracked collection exceeds the configured change count");
    assert_eq!(contract_error.code(), GitCollectErrorCode::Contract);
}

#[test]
fn sha256_object_format_is_preserved_without_sha1_assumptions() {
    let directory = tempfile::tempdir().expect("temporary SHA-256 repository is created");
    run_git(
        directory.path(),
        &["init", "--quiet", "--object-format=sha256"],
    );
    run_git(directory.path(), &["config", "user.name", "Rootlight Test"]);
    run_git(
        directory.path(),
        &["config", "user.email", "rootlight@example.invalid"],
    );
    write_and_commit(
        directory.path(),
        "sha256.rs",
        "pub fn sha256_repository() {}\n",
        "initial",
    );

    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("SHA-256 repository collection succeeds");
    let RepositoryState::Git(state) = &snapshot.as_input().state else {
        panic!("fixture must be a Git repository");
    };
    assert_eq!(state.object_format, ObjectFormat::Sha256);
    assert!(
        snapshot
            .as_input()
            .commits
            .iter()
            .all(|commit| commit.id.format() == ObjectFormat::Sha256)
    );
}

#[test]
fn staged_gitlink_is_reported_without_running_submodule_commands() {
    let directory = initialized_repository();
    write_and_commit(directory.path(), "root.rs", "pub fn root() {}\n", "initial");
    let commit = git_stdout(directory.path(), &["rev-parse", "HEAD"]);
    let cache_info = format!("160000,{commit},vendor/example");
    run_git(
        directory.path(),
        &["update-index", "--add", "--cacheinfo", &cache_info],
    );

    let snapshot = collect_repository(
        directory.path(),
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("gitlink collection succeeds");
    assert!(snapshot.as_input().submodules.iter().any(|submodule| {
        submodule.path == "vendor/example" && submodule.checkout == SubmoduleCheckoutState::Unknown
    }));
}

#[test]
fn shallow_history_remains_explicit_and_never_triggers_lazy_fetch() {
    let parent = tempfile::tempdir().expect("temporary clone parent is created");
    let source = parent.path().join("source");
    let clone = parent.path().join("clone");
    fs::create_dir(&source).expect("source repository directory is created");
    run_git(&source, &["init", "--quiet"]);
    run_git(&source, &["config", "user.name", "Rootlight Test"]);
    run_git(
        &source,
        &["config", "user.email", "rootlight@example.invalid"],
    );
    write_and_commit(&source, "one.rs", "fn one() {}\n", "one");
    write_and_commit(&source, "two.rs", "fn two() {}\n", "two");
    write_and_commit(&source, "three.rs", "fn three() {}\n", "three");

    let output = Command::new("git")
        .current_dir(parent.path())
        .arg("clone")
        .arg("--quiet")
        .arg("--depth=1")
        .arg("--no-local")
        .arg(&source)
        .arg(&clone)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("shallow clone command starts");
    assert!(
        output.status.success(),
        "shallow clone failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let snapshot = collect_repository(
        &clone,
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("shallow repository collection succeeds");
    let RepositoryState::Git(state) = &snapshot.as_input().state else {
        panic!("fixture must be a Git repository");
    };
    assert!(matches!(state.history, HistoryState::Incomplete { .. }));
    assert!(
        state
            .coverage
            .truncation
            .contains(&HistoryTruncation::ShallowBoundary)
    );
    assert_eq!(snapshot.as_input().commits.len(), 1);
}

#[test]
fn linked_worktree_resolves_the_shared_object_database_without_root_paths() {
    let parent = tempfile::tempdir().expect("temporary worktree parent is created");
    let primary = parent.path().join("primary");
    let linked = parent.path().join("linked");
    fs::create_dir(&primary).expect("primary repository directory is created");
    run_git(&primary, &["init", "--quiet"]);
    run_git(&primary, &["config", "user.name", "Rootlight Test"]);
    run_git(
        &primary,
        &["config", "user.email", "rootlight@example.invalid"],
    );
    write_and_commit(&primary, "worktree.rs", "pub fn worktree() {}\n", "initial");
    let linked_text = linked
        .to_str()
        .expect("temporary worktree path is valid UTF-8");
    run_git(
        &primary,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            linked_text,
            "HEAD",
        ],
    );

    let primary_snapshot = collect_repository(
        &primary,
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("primary worktree collection succeeds");
    let linked_snapshot = collect_repository(
        &linked,
        repository(),
        &GitLimits::default(),
        collection_limits(8),
        &Cancellation::new(),
    )
    .expect("linked worktree collection succeeds");
    let RepositoryState::Git(primary_state) = &primary_snapshot.as_input().state else {
        panic!("primary fixture must be a Git repository");
    };
    let RepositoryState::Git(linked_state) = &linked_snapshot.as_input().state else {
        panic!("linked fixture must be a Git repository");
    };
    assert_eq!(
        primary_state.object_database, linked_state.object_database,
        "linked worktrees must share one source-free object database identity"
    );
    assert!(matches!(
        linked_snapshot.as_input().worktrees[0].head,
        HeadState::Detached { .. }
    ));
}
