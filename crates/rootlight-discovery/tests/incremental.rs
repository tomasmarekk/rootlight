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
    correlate_incremental_manifest, discover, discover_incremental,
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
        change.class() == ChangeClass::Surface
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
