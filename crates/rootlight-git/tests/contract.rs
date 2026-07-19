//! Public contract tests for bounded importer-provided Git evidence.
//!
//! Fixtures contain only stable IDs, object hashes, canonical paths, and
//! source-free metadata so failures cannot expose repository source bodies.

use proptest::prelude::*;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_git::{
    ByteSpan, CandidateGroupId, ChangeSet, ChangedSpan, CommitRecord, FileChange, FileChangeKind,
    GitCollection, GitContractError, GitLimits, GitRepositoryState, GitSnapshotInput, GitTextKind,
    HeadState, HistoryCoverage, HistoryGapReason, HistoryState, HistoryTruncation,
    LineageEvidenceKind, LineageKind, NonGitReason, ObjectDatabaseId, ObjectFormat, ObjectId,
    RenameCandidate, RenameEvidenceKind, RepositoryState, RevisionSelector, SparseCheckoutState,
    SubmoduleCheckoutState, SubmoduleState, SymbolLineageCandidate, WorktreeState, WorktreeStatus,
    canonicalize_snapshot,
};
use rootlight_ids::{RepositoryId, SymbolId, SymbolIdentity, derive_repository, derive_symbol};

fn repository() -> RepositoryId {
    derive_repository(b"git-contract-test-repository").id()
}

fn object(byte: u8) -> ObjectId {
    ObjectId::sha1([byte; 20])
}

fn symbol(name: &str) -> SymbolId {
    derive_symbol(SymbolIdentity {
        repository: repository(),
        language: "rust",
        semantic_kind: "function",
        container_identity: b"crate",
        declared_identity: name,
        signature_discriminator: b"()",
        build_context_discriminator: b"default",
    })
    .id()
}

fn worktree(id: &str) -> WorktreeState {
    WorktreeState {
        id: id.to_owned(),
        head: HeadState::Branch {
            reference: format!("refs/heads/{id}"),
            commit: object(1),
        },
        index_tree: Some(object(2)),
        status: WorktreeStatus::default(),
        sparse_checkout: SparseCheckoutState::Disabled,
    }
}

fn commit(id: u8) -> CommitRecord {
    CommitRecord {
        id: object(id),
        parents: Vec::new(),
        tree: object(id.wrapping_add(100)),
        author_time_unix_seconds: i64::from(id),
    }
}

fn git_snapshot() -> GitSnapshotInput {
    GitSnapshotInput {
        version: rootlight_git::GIT_CONTRACT_VERSION,
        repository: repository(),
        state: RepositoryState::Git(GitRepositoryState {
            object_format: ObjectFormat::Sha1,
            object_database: ObjectDatabaseId::from_bytes([7; 32]),
            history: HistoryState::Complete,
            coverage: HistoryCoverage {
                imported_commits: 1,
                requested_commit_limit: 32,
                oldest_imported_time_unix_seconds: Some(1),
                truncation: Vec::new(),
            },
        }),
        worktrees: vec![worktree("main")],
        commits: vec![commit(1)],
        change_sets: Vec::new(),
        rename_candidates: Vec::new(),
        submodules: Vec::new(),
        lineage_candidates: Vec::new(),
    }
}

fn modified_change(path: &str, spans: Vec<ChangedSpan>) -> FileChange {
    FileChange {
        kind: FileChangeKind::Modified,
        before_path: Some(path.to_owned()),
        after_path: Some(path.to_owned()),
        spans,
    }
}

fn span(start: u64, end: u64) -> ByteSpan {
    ByteSpan::new(start, end).expect("fixture span is valid")
}

#[test]
fn rename_evidence_never_collapses_add_and_delete() {
    let mut input = git_snapshot();
    let base = RevisionSelector::Commit(object(1));
    let head = RevisionSelector::WorkingTree {
        worktree: "main".to_owned(),
    };
    input.change_sets.push(ChangeSet {
        base: base.clone(),
        head: head.clone(),
        changes: vec![
            FileChange {
                kind: FileChangeKind::Added,
                before_path: None,
                after_path: Some("src/new.rs".to_owned()),
                spans: vec![ChangedSpan {
                    before: None,
                    after: Some(span(0, 12)),
                }],
            },
            FileChange {
                kind: FileChangeKind::Deleted,
                before_path: Some("src/old.rs".to_owned()),
                after_path: None,
                spans: vec![ChangedSpan {
                    before: Some(span(0, 12)),
                    after: None,
                }],
            },
        ],
    });
    input.rename_candidates.push(RenameCandidate {
        base,
        head,
        before_path: "src/old.rs".to_owned(),
        after_path: "src/new.rs".to_owned(),
        group: CandidateGroupId::from_bytes([1; 16]),
        confidence_bps: 10_000,
        evidence: vec![
            RenameEvidenceKind::ImporterSignal,
            RenameEvidenceKind::ExactContent,
        ],
    });

    let canonical = canonicalize_snapshot(input, &GitLimits::default(), &Cancellation::new())
        .expect("rename evidence validates");
    let snapshot = canonical.as_input();
    assert_eq!(snapshot.rename_candidates.len(), 1);
    assert_eq!(snapshot.change_sets[0].changes.len(), 2);
    assert!(
        snapshot.change_sets[0]
            .changes
            .iter()
            .any(|change| change.kind == FileChangeKind::Added)
    );
    assert!(
        snapshot.change_sets[0]
            .changes
            .iter()
            .any(|change| change.kind == FileChangeKind::Deleted)
    );
}

#[test]
fn worktrees_and_revision_states_are_canonicalized_without_roots() {
    let mut input = git_snapshot();
    input.worktrees = vec![worktree("main"), worktree("aux")];
    input.change_sets = vec![
        ChangeSet {
            base: RevisionSelector::Head {
                worktree: "main".to_owned(),
            },
            head: RevisionSelector::WorkingTree {
                worktree: "main".to_owned(),
            },
            changes: Vec::new(),
        },
        ChangeSet {
            base: RevisionSelector::Index {
                worktree: "aux".to_owned(),
            },
            head: RevisionSelector::WorkingTree {
                worktree: "aux".to_owned(),
            },
            changes: Vec::new(),
        },
    ];

    let canonical = canonicalize_snapshot(input, &GitLimits::default(), &Cancellation::new())
        .expect("worktree states validate");
    let ids: Vec<_> = canonical
        .as_input()
        .worktrees
        .iter()
        .map(|worktree| worktree.id.as_str())
        .collect();
    assert_eq!(ids, ["aux", "main"]);
    assert_eq!(canonical.as_input().change_sets.len(), 2);
}

#[test]
fn shallow_and_missing_history_remain_explicit() {
    let mut shallow = git_snapshot();
    let RepositoryState::Git(state) = &mut shallow.state else {
        panic!("fixture is a Git repository");
    };
    state.history = HistoryState::Shallow {
        boundary_commits: vec![object(9), object(8), object(9)],
    };
    state.coverage.truncation = vec![
        HistoryTruncation::ShallowBoundary,
        HistoryTruncation::ShallowBoundary,
    ];

    let shallow = canonicalize_snapshot(shallow, &GitLimits::default(), &Cancellation::new())
        .expect("shallow history validates");
    let RepositoryState::Git(state) = &shallow.as_input().state else {
        panic!("canonical fixture remains Git");
    };
    assert_eq!(
        state.history,
        HistoryState::Shallow {
            boundary_commits: vec![object(8), object(9)]
        }
    );

    let mut missing = git_snapshot();
    let RepositoryState::Git(state) = &mut missing.state else {
        panic!("fixture is a Git repository");
    };
    state.history = HistoryState::Incomplete {
        reason: HistoryGapReason::MissingObjects,
        missing_objects: vec![object(12)],
    };
    state.coverage.truncation = vec![HistoryTruncation::MissingObjects];
    let missing = canonicalize_snapshot(missing, &GitLimits::default(), &Cancellation::new())
        .expect("missing-object history validates");
    let RepositoryState::Git(state) = &missing.as_input().state else {
        panic!("canonical fixture remains Git");
    };
    assert!(matches!(
        state.history,
        HistoryState::Incomplete {
            reason: HistoryGapReason::MissingObjects,
            ..
        }
    ));
}

#[test]
fn sparse_patterns_and_submodules_are_retained_without_execution() {
    let mut input = git_snapshot();
    input.worktrees[0].sparse_checkout = SparseCheckoutState::Enabled {
        cone_mode: false,
        patterns: vec!["/src/".to_owned(), "!/src/generated/".to_owned()],
    };
    input.submodules = vec![
        SubmoduleState {
            worktree: "main".to_owned(),
            path: "vendor/zeta".to_owned(),
            recorded_commit: object(30),
            checkout: SubmoduleCheckoutState::Uninitialized,
        },
        SubmoduleState {
            worktree: "main".to_owned(),
            path: "vendor/alpha".to_owned(),
            recorded_commit: object(31),
            checkout: SubmoduleCheckoutState::Present {
                commit: ObjectId::sha256([32; 32]),
            },
        },
    ];

    let canonical = canonicalize_snapshot(input, &GitLimits::default(), &Cancellation::new())
        .expect("sparse and submodule evidence validates");
    assert_eq!(canonical.as_input().submodules[0].path, "vendor/alpha");
    let SparseCheckoutState::Enabled { patterns, .. } =
        &canonical.as_input().worktrees[0].sparse_checkout
    else {
        panic!("sparse checkout remains enabled");
    };
    assert_eq!(patterns, &["/src/", "!/src/generated/"]);
}

#[test]
fn non_git_repositories_are_explicit_and_reject_git_facts() {
    let non_git = GitSnapshotInput::non_git(repository(), NonGitReason::MetadataAbsent);
    let canonical =
        canonicalize_snapshot(non_git.clone(), &GitLimits::default(), &Cancellation::new())
            .expect("empty non-Git evidence validates");
    assert!(matches!(
        canonical.as_input().state,
        RepositoryState::NonGit { .. }
    ));

    let mut inconsistent = non_git;
    inconsistent.commits.push(commit(1));
    assert_eq!(
        canonicalize_snapshot(inconsistent, &GitLimits::default(), &Cancellation::new()),
        Err(GitContractError::InvalidHistory)
    );
}

#[test]
fn ambiguous_symbol_lineage_candidates_are_all_preserved() {
    let mut input = git_snapshot();
    let group = CandidateGroupId::from_bytes([3; 16]);
    input.lineage_candidates = vec![
        SymbolLineageCandidate {
            group,
            prior: symbol("prior"),
            current: symbol("candidate_b"),
            kind: LineageKind::MovedFrom,
            confidence_bps: 7_500,
            evidence: vec![LineageEvidenceKind::GitRename],
        },
        SymbolLineageCandidate {
            group,
            prior: symbol("prior"),
            current: symbol("candidate_a"),
            kind: LineageKind::MovedFrom,
            confidence_bps: 7_500,
            evidence: vec![LineageEvidenceKind::GitRename],
        },
    ];

    let canonical = canonicalize_snapshot(input, &GitLimits::default(), &Cancellation::new())
        .expect("ambiguous lineage validates");
    assert_eq!(canonical.as_input().lineage_candidates.len(), 2);
    assert!(
        canonical
            .as_input()
            .lineage_candidates
            .iter()
            .all(|candidate| candidate.group == group)
    );
}

#[test]
fn configured_commit_change_span_lineage_and_text_limits_fail_closed() {
    let mut commits = git_snapshot();
    commits.commits.push(commit(2));
    let RepositoryState::Git(state) = &mut commits.state else {
        panic!("fixture is Git");
    };
    state.coverage.imported_commits = 2;
    state.coverage.requested_commit_limit = 1;
    let one_commit = GitLimits::new(1, 10, 10, 10, 128, 4_096).expect("limits are valid");
    assert!(matches!(
        canonicalize_snapshot(commits, &one_commit, &Cancellation::new()),
        Err(GitContractError::CollectionLimit {
            collection: GitCollection::Commits,
            maximum: 1
        })
    ));

    let mut changes = git_snapshot();
    changes.change_sets.push(ChangeSet {
        base: RevisionSelector::Commit(object(1)),
        head: RevisionSelector::WorkingTree {
            worktree: "main".to_owned(),
        },
        changes: vec![
            modified_change(
                "src/a.rs",
                vec![ChangedSpan {
                    before: Some(span(0, 1)),
                    after: Some(span(0, 1)),
                }],
            ),
            modified_change(
                "src/b.rs",
                vec![ChangedSpan {
                    before: Some(span(0, 1)),
                    after: Some(span(0, 1)),
                }],
            ),
        ],
    });
    let one_change = GitLimits::new(64, 1, 10, 10, 128, 4_096).expect("limits are valid");
    assert!(matches!(
        canonicalize_snapshot(changes, &one_change, &Cancellation::new()),
        Err(GitContractError::CollectionLimit {
            collection: GitCollection::Changes,
            maximum: 1
        })
    ));

    let mut spans = git_snapshot();
    spans.change_sets.push(ChangeSet {
        base: RevisionSelector::Commit(object(1)),
        head: RevisionSelector::WorkingTree {
            worktree: "main".to_owned(),
        },
        changes: vec![modified_change(
            "src/lib.rs",
            vec![
                ChangedSpan {
                    before: Some(span(0, 1)),
                    after: Some(span(0, 1)),
                },
                ChangedSpan {
                    before: Some(span(2, 3)),
                    after: Some(span(2, 3)),
                },
            ],
        )],
    });
    let one_span = GitLimits::new(64, 10, 1, 10, 128, 4_096).expect("limits are valid");
    assert!(matches!(
        canonicalize_snapshot(spans, &one_span, &Cancellation::new()),
        Err(GitContractError::CollectionLimit {
            collection: GitCollection::ChangedSpans,
            maximum: 1
        })
    ));

    let mut lineage = git_snapshot();
    lineage.lineage_candidates = vec![
        SymbolLineageCandidate {
            group: CandidateGroupId::from_bytes([1; 16]),
            prior: symbol("old_a"),
            current: symbol("new_a"),
            kind: LineageKind::RenamedFrom,
            confidence_bps: 8_000,
            evidence: vec![LineageEvidenceKind::DeclarationFingerprint],
        },
        SymbolLineageCandidate {
            group: CandidateGroupId::from_bytes([2; 16]),
            prior: symbol("old_b"),
            current: symbol("new_b"),
            kind: LineageKind::RenamedFrom,
            confidence_bps: 8_000,
            evidence: vec![LineageEvidenceKind::DeclarationFingerprint],
        },
    ];
    let one_lineage = GitLimits::new(64, 10, 10, 1, 128, 4_096).expect("limits are valid");
    assert!(matches!(
        canonicalize_snapshot(lineage, &one_lineage, &Cancellation::new()),
        Err(GitContractError::CollectionLimit {
            collection: GitCollection::LineageCandidates,
            maximum: 1
        })
    ));

    let text_limits = GitLimits::new(64, 10, 10, 10, 3, 4_096).expect("limits are valid");
    assert_eq!(
        canonicalize_snapshot(git_snapshot(), &text_limits, &Cancellation::new()),
        Err(GitContractError::TextLimit {
            kind: GitTextKind::Worktree,
            maximum: 3
        })
    );

    let aggregate_limits = GitLimits::new(64, 10, 10, 10, 16, 18).expect("limits are valid");
    assert_eq!(
        canonicalize_snapshot(git_snapshot(), &aggregate_limits, &Cancellation::new()),
        Err(GitContractError::TextLimit {
            kind: GitTextKind::Aggregate,
            maximum: 18
        })
    );
}

#[test]
fn cancellation_is_observed_before_import_work() {
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));

    assert!(matches!(
        canonicalize_snapshot(git_snapshot(), &GitLimits::default(), &cancellation),
        Err(GitContractError::Cancelled(_))
    ));
}

#[test]
fn validation_errors_never_echo_hostile_repository_text() {
    let mut input = git_snapshot();
    let secret_path = "../private-token-super-secret";
    input.change_sets.push(ChangeSet {
        base: RevisionSelector::Commit(object(1)),
        head: RevisionSelector::WorkingTree {
            worktree: "main".to_owned(),
        },
        changes: vec![modified_change(secret_path, Vec::new())],
    });

    let error = canonicalize_snapshot(input, &GitLimits::default(), &Cancellation::new())
        .expect_err("parent traversal is rejected");
    assert_eq!(
        error,
        GitContractError::InvalidText {
            kind: GitTextKind::RepositoryPath
        }
    );
    assert!(!error.to_string().contains(secret_path));
    assert!(!error.to_string().contains("private-token"));
}

proptest! {
    #[test]
    fn canonical_json_is_independent_of_commit_input_order(
        bytes in prop::collection::btree_set(1_u8..=99, 1..32)
    ) {
        let mut ascending = git_snapshot();
        ascending.commits = bytes.iter().copied().map(commit).collect();
        let RepositoryState::Git(state) = &mut ascending.state else {
            unreachable!("fixture is Git");
        };
        state.coverage.imported_commits =
            u32::try_from(ascending.commits.len()).expect("fixture count fits u32");
        state.coverage.requested_commit_limit = 64;

        let mut descending = ascending.clone();
        descending.commits.reverse();

        let first = canonicalize_snapshot(
            ascending,
            &GitLimits::default(),
            &Cancellation::new()
        )
        .expect("ascending input validates")
        .canonical_json()
        .expect("canonical snapshot serializes");
        let second = canonicalize_snapshot(
            descending,
            &GitLimits::default(),
            &Cancellation::new()
        )
        .expect("descending input validates")
        .canonical_json()
        .expect("canonical snapshot serializes");

        prop_assert_eq!(first, second);
    }
}
