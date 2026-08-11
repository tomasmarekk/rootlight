//! Cross-crate tests for capability-confined discovery reconciliation.
//!
//! The fixtures mutate only files beneath an opened repository root and verify
//! authoritative scans without supplying watcher events.

use std::{
    fs::{self, FileTimes, OpenOptions},
    time::{Duration, SystemTime},
};

use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_config::ConfigSnapshot;
use rootlight_discovery::{
    DiscoveryError, DiscoveryLimits, DiscoveryPolicy, IncrementalDiscoveryContext,
    IncrementalDiscoveryOptions, correlate_incremental_manifest, discover, discover_incremental,
    discover_incremental_with_progress, discover_with_snapshots,
};
use rootlight_ids::{FactId, RepositoryId, content_hash, derive_repository};
use rootlight_incremental::{ChangeClass, FileChangeKind, ReconcileMode};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::{TempDir, tempdir_in};

fn local_tempdir() -> TempDir {
    let current = std::env::current_dir().expect("current directory is available");
    tempdir_in(current).expect("local temporary directory is available")
}

fn root(temporary: &TempDir, seed: &[u8]) -> RepositoryRoot {
    RepositoryRoot::open(derive_repository(seed).id(), temporary.path())
        .expect("fixture repository opens")
}

fn policy() -> DiscoveryPolicy {
    DiscoveryPolicy::build(Vec::new(), false).expect("empty fixture policy builds")
}

fn limits() -> DiscoveryLimits {
    DiscoveryLimits::new(1_000, 16, 1024 * 1024, 100).expect("fixture limits are valid")
}

fn context(configuration: &[u8], provider: &[u8]) -> IncrementalDiscoveryContext {
    IncrementalDiscoveryContext::new(
        content_hash(configuration),
        FactId::from_bytes([7; 20]),
        content_hash(provider),
    )
}

#[test]
fn no_op_reuse_requires_a_platform_change_token_and_audit_always_rehashes() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-no-op");
    let policy = policy();
    let context = context(b"config-v1", b"provider-v1");

    let first = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");
    assert_eq!(first.hashed_files().len(), 1);

    let no_op = discover_incremental(
        &root,
        Some(first.baseline()),
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("no-op reconcile succeeds");
    #[cfg(unix)]
    assert!(no_op.hashed_files().is_empty());
    #[cfg(windows)]
    assert_eq!(no_op.hashed_files().len(), 1);
    assert!(no_op.changes().is_empty());
    assert_eq!(no_op.file_changes()[0].kind(), FileChangeKind::NoChange);

    let audit = discover_incremental(
        &root,
        Some(no_op.baseline()),
        context,
        &policy,
        ReconcileMode::Audit,
        limits(),
        &Cancellation::new(),
    )
    .expect("audit reconcile succeeds");
    assert_eq!(audit.hashed_files().len(), 1);
    assert!(audit.changes().is_empty());
}

#[test]
fn cold_incremental_discovery_reports_monotonic_file_and_byte_progress() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("first.rs"), b"aa").expect("first source is written");
    fs::write(temporary.path().join("second.rs"), b"bbbb").expect("second source is written");
    let root = root(&temporary, b"incremental-progress");
    let mut progress = Vec::new();

    let discovery = discover_incremental_with_progress(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy(),
        IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits()),
        &Cancellation::new(),
        |observed| progress.push(observed),
    )
    .expect("cold discovery succeeds");

    assert_eq!(discovery.hashed_files().len(), 2);
    assert_eq!(progress.len(), 2);
    assert_eq!(progress[0].files_examined, 1);
    assert_eq!(progress[1].files_examined, 2);
    assert!(progress[0].bytes_examined > 0);
    assert!(progress[1].bytes_examined >= progress[0].bytes_examined);
    assert_eq!(progress[1].bytes_examined, 6);
}

#[test]
fn complete_scan_detects_missed_same_size_rewrite_with_clock_regression() {
    let temporary = local_tempdir();
    let source_path = temporary.path().join("clock.rs");
    fs::write(&source_path, b"aaaa").expect("fixture source is written");
    let root = root(&temporary, b"incremental-clock");
    let policy = policy();
    let context = context(b"config-v1", b"provider-v1");
    let first = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");

    fs::write(&source_path, b"bbbb").expect("same-size rewrite succeeds");
    let file = OpenOptions::new()
        .write(true)
        .open(&source_path)
        .expect("rewritten fixture opens");
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)))
        .expect("fixture modification clock regresses");

    let update = discover_incremental(
        &root,
        Some(first.baseline()),
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("authoritative scan detects rewrite without a watcher event");
    let file_id = root.file_id(
        &RelativePath::parse(std::path::Path::new("clock.rs")).expect("fixture path is valid"),
    );

    assert_eq!(update.hashed_files(), &[file_id]);
    assert_eq!(update.file_changes()[0].kind(), FileChangeKind::Modified);
    assert!(update.changes().changes().iter().any(|change| {
        change.class() == ChangeClass::BodyOnly
            && change.key() == rootlight_incremental::InputKey::FileContent(file_id)
    }));
}

#[test]
fn replacement_rename_and_delete_produce_canonical_file_changes() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("replace.rs"), b"old!")
        .expect("replacement fixture is written");
    fs::write(temporary.path().join("move.rs"), b"same").expect("move fixture is written");
    fs::write(temporary.path().join("delete.rs"), b"gone").expect("delete fixture is written");
    let root = root(&temporary, b"incremental-file-transitions");
    let policy = policy();
    let context = context(b"config-v1", b"provider-v1");
    let first = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");

    let replacement = temporary.path().join("replacement.tmp");
    fs::write(&replacement, b"new!").expect("replacement bytes are written");
    fs::remove_file(temporary.path().join("replace.rs")).expect("old file is removed");
    fs::rename(&replacement, temporary.path().join("replace.rs"))
        .expect("replacement enters the repository");
    fs::rename(
        temporary.path().join("move.rs"),
        temporary.path().join("moved.rs"),
    )
    .expect("fixture file is renamed");
    fs::remove_file(temporary.path().join("delete.rs")).expect("fixture file is deleted");

    let update = discover_incremental(
        &root,
        Some(first.baseline()),
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("file transitions reconcile");
    let kinds: Vec<_> = update
        .file_changes()
        .iter()
        .map(|change| change.kind())
        .collect();

    assert!(kinds.contains(&FileChangeKind::Modified));
    assert!(kinds.contains(&FileChangeKind::Moved));
    assert!(kinds.contains(&FileChangeKind::Deleted));
    assert!(!update.changes().is_empty());
}

#[test]
fn configuration_and_provider_drift_change_typed_inputs_independently() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-context");
    let policy = policy();
    let first = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("initial reconcile succeeds");

    let configured = discover_incremental(
        &root,
        Some(first.baseline()),
        context(b"config-v2", b"provider-v1"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("configuration drift reconciles");
    #[cfg(unix)]
    assert!(configured.hashed_files().is_empty());
    #[cfg(windows)]
    assert_eq!(configured.hashed_files().len(), 1);
    assert_eq!(configured.changes().changes().len(), 1);
    assert_eq!(
        configured.changes().changes()[0].class(),
        ChangeClass::Configuration
    );

    let provider = discover_incremental(
        &root,
        Some(configured.baseline()),
        context(b"config-v2", b"provider-v2"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("provider drift reconciles");
    #[cfg(unix)]
    assert!(provider.hashed_files().is_empty());
    #[cfg(windows)]
    assert_eq!(provider.hashed_files().len(), 1);
    assert_eq!(provider.changes().changes().len(), 1);
    assert_eq!(
        provider.changes().changes()[0].class(),
        ChangeClass::ProviderChange
    );
}

#[test]
fn cancelled_incremental_scan_stops_before_repository_work() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-cancel");
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(CancellationReason::ClientRequest));

    assert!(matches!(
        discover_incremental(
            &root,
            None,
            context(b"config-v1", b"provider-v1"),
            &policy(),
            ReconcileMode::Normal,
            limits(),
            &cancellation,
        ),
        Err(rootlight_discovery::DiscoveryError::Cancelled(cancelled))
            if cancelled.reason() == CancellationReason::ClientRequest
    ));
}

#[test]
fn scoped_ignores_exclude_files_and_subtrees_before_incremental_hashing() {
    let temporary = local_tempdir();
    fs::create_dir(temporary.path().join("ignored")).expect("ignored fixture directory is created");
    fs::write(
        temporary.path().join(".gitignore"),
        b"ignored/\nignored.txt\n",
    )
    .expect("ignore fixture is written");
    fs::write(temporary.path().join("included.rs"), b"fn included() {}\n")
        .expect("included fixture is written");
    fs::write(temporary.path().join("ignored.txt"), b"ignored file")
        .expect("ignored file fixture is written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        fs::set_permissions(
            temporary.path().join("ignored.txt"),
            fs::Permissions::from_mode(0o000),
        )
        .expect("ignored file is made unreadable");
    }
    fs::write(
        temporary.path().join("ignored").join("nested.rs"),
        b"fn ignored_nested() {}\n",
    )
    .expect("ignored subtree fixture is written");
    let root = root(&temporary, b"incremental-scoped-ignore");

    let discovery = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy(),
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("ignored paths do not enter incremental hashing");
    let ignored_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("ignored.txt"))
            .expect("ignored fixture path is valid"),
    );
    let ignored_nested = root.file_id(
        &RelativePath::parse(std::path::Path::new("ignored/nested.rs"))
            .expect("ignored nested fixture path is valid"),
    );

    assert!(!discovery.hashed_files().contains(&ignored_file));
    assert!(!discovery.hashed_files().contains(&ignored_nested));
    assert_eq!(discovery.baseline().metadata().len(), 2);
}

#[test]
fn vcs_negations_cannot_reopen_default_exclusions() {
    let temporary = local_tempdir();
    fs::create_dir(temporary.path().join(".git")).expect("git fixture directory is created");
    fs::create_dir(temporary.path().join("target")).expect("target fixture directory is created");
    fs::create_dir(temporary.path().join("nested")).expect("nested fixture directory is created");
    fs::create_dir(temporary.path().join("nested").join("target"))
        .expect("nested target fixture directory is created");
    fs::write(
        temporary.path().join(".gitignore"),
        b"!.git/\n!.git/*.rs\n!target/\n!target/*.rs\n",
    )
    .expect("malicious ignore fixture is written");
    fs::write(
        temporary.path().join("nested").join(".gitignore"),
        b"!target/\n!target/*.rs\n",
    )
    .expect("nested malicious ignore fixture is written");
    fs::write(
        temporary.path().join(".git").join("leaked.rs"),
        b"git secret",
    )
    .expect("git fixture is written");
    fs::write(
        temporary.path().join("target").join("leaked.rs"),
        b"build secret",
    )
    .expect("target fixture is written");
    fs::write(
        temporary
            .path()
            .join("nested")
            .join("target")
            .join("leaked.rs"),
        b"nested build secret",
    )
    .expect("nested target fixture is written");
    fs::write(temporary.path().join("visible.rs"), b"fn visible() {}\n")
        .expect("visible fixture is written");
    let root = root(&temporary, b"default-exclusion-negation");
    let policy = policy();
    let cancellation = Cancellation::new();

    let incremental = discover_incremental(
        &root,
        None,
        context(b"config-v1", b"provider-v1"),
        &policy,
        ReconcileMode::Normal,
        limits(),
        &cancellation,
    )
    .expect("incremental discovery succeeds");
    let manifest = discover(
        &root,
        &ConfigSnapshot::resolve(&[]).expect("default config resolves"),
        &policy,
        limits(),
        &cancellation,
    )
    .expect("clean discovery succeeds");
    let git_file = root.file_id(
        &RelativePath::parse(std::path::Path::new(".git/leaked.rs"))
            .expect("git fixture path is valid"),
    );
    let target_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("target/leaked.rs"))
            .expect("target fixture path is valid"),
    );
    let nested_target_file = root.file_id(
        &RelativePath::parse(std::path::Path::new("nested/target/leaked.rs"))
            .expect("nested target fixture path is valid"),
    );

    assert!(!incremental.hashed_files().contains(&git_file));
    assert!(!incremental.hashed_files().contains(&target_file));
    assert!(!incremental.hashed_files().contains(&nested_target_file));
    assert_eq!(
        manifest
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .collect::<Vec<_>>(),
        [".gitignore", "nested/.gitignore", "visible.rs"]
    );
}

#[test]
fn clean_manifest_must_match_the_incremental_observation() {
    let temporary = local_tempdir();
    fs::write(temporary.path().join("lib.rs"), b"pub fn value() {}\n")
        .expect("fixture source is written");
    let root = root(&temporary, b"incremental-manifest-correlation");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let observed = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &Cancellation::new(),
    )
    .expect("incremental observation succeeds");
    let manifest = discover(&root, &config, &policy, limits(), &Cancellation::new())
        .expect("clean discovery succeeds");
    let correlated = correlate_incremental_manifest(
        &observed,
        None,
        context,
        &manifest,
        limits(),
        &Cancellation::new(),
    )
    .expect("matching observations correlate");
    assert_eq!(
        correlated.baseline().metadata().len(),
        manifest.inputs.len()
    );

    let mut wrong_repository = manifest.clone();
    wrong_repository.repository = RepositoryId::from_bytes([0x44; 16]);
    assert!(matches!(
        correlate_incremental_manifest(
            &observed,
            None,
            context,
            &wrong_repository,
            limits(),
            &Cancellation::new(),
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));

    let mut drifted = manifest;
    drifted.inputs[0].content_hash = content_hash(b"different bytes");
    assert!(matches!(
        correlate_incremental_manifest(
            &observed,
            None,
            context,
            &drifted,
            limits(),
            &Cancellation::new(),
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));
}

#[test]
fn cached_snapshot_revalidates_metadata_only_changes() {
    let temporary = local_tempdir();
    let source_path = temporary.path().join("lib.rs");
    fs::write(&source_path, b"pub fn value() {}\n").expect("fixture source is written");
    let root = root(&temporary, b"incremental-cached-metadata");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let mut observed = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &cancellation,
    )
    .expect("incremental observation succeeds");
    let cached_snapshots = observed.take_hashed_snapshots();
    let file = OpenOptions::new()
        .write(true)
        .open(&source_path)
        .expect("fixture opens for metadata update");
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(2)))
        .expect("fixture modification time changes");

    let discovery = discover_with_snapshots(
        &root,
        &config,
        &policy,
        limits(),
        cached_snapshots,
        &cancellation,
    )
    .expect("unchanged content survives metadata revalidation");
    let (manifest, snapshots) = discovery.into_parts();
    assert_eq!(manifest.inputs.len(), 1);
    assert_eq!(snapshots.len(), 1);
}

#[test]
fn cached_snapshot_revalidation_rejects_same_length_rewrites() {
    let temporary = local_tempdir();
    let source_path = temporary.path().join("lib.rs");
    fs::write(&source_path, b"aaaa").expect("fixture source is written");
    let root = root(&temporary, b"incremental-cached-content");
    let config = ConfigSnapshot::resolve(&[]).expect("default config resolves");
    let policy = policy();
    let context = IncrementalDiscoveryContext::new(
        config.hash(),
        FactId::from_bytes([7; 20]),
        content_hash(b"provider-v1"),
    );
    let cancellation = Cancellation::new();
    let mut observed = discover_incremental(
        &root,
        None,
        context,
        &policy,
        ReconcileMode::Normal,
        limits(),
        &cancellation,
    )
    .expect("incremental observation succeeds");
    let cached_snapshots = observed.take_hashed_snapshots();
    fs::write(&source_path, b"bbbb").expect("same-length rewrite succeeds");
    let file = OpenOptions::new()
        .write(true)
        .open(&source_path)
        .expect("rewritten fixture opens");
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(3)))
        .expect("rewrite modification time changes");

    assert!(matches!(
        discover_with_snapshots(
            &root,
            &config,
            &policy,
            limits(),
            cached_snapshots,
            &cancellation,
        ),
        Err(DiscoveryError::IncrementalDrift)
    ));
}
