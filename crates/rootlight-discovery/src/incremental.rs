//! Capability-confined metadata scans for authoritative incremental reconcile.
//!
//! Watcher events never enter this API. Complete bounded scans decide which
//! files require content hashing and derive canonical typed generation changes.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    ffi::OsStr,
    path::{Path, PathBuf},
};

use ignore::gitignore::GitignoreBuilder;
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, RepositoryId, content_hash};
use rootlight_incremental::{
    AuthoritativeScan, ChangeSet, FileChange, FileDescriptor, FileMetadata, IncrementalError,
    InputFingerprint, InputKey, InputSnapshot, MetadataBaseline, PlanningLimits,
    PlatformFileIdentity, ReconcileLimits, ReconcileMode, ScannedFile, plan_reconcile,
};
use rootlight_vfs::{EntryKind, RelativePath, RepositoryRoot, SnapshotMetadata, SourceSnapshot};

use crate::{
    DiscoveryError, DiscoveryLimits, DiscoveryManifest, DiscoveryPolicy, MAX_RETAINED_SOURCE_BYTES,
    RetainedSnapshotBudget, ScopedIgnores, child_path,
};

/// Configuration and provider identities included in one incremental input set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncrementalDiscoveryContext {
    configuration_revision: ContentHash,
    provider: FactId,
    provider_revision: ContentHash,
}

impl IncrementalDiscoveryContext {
    /// Creates a complete discovery context.
    ///
    /// The provider identity must remain stable while its revision hash changes.
    #[must_use]
    pub const fn new(
        configuration_revision: ContentHash,
        provider: FactId,
        provider_revision: ContentHash,
    ) -> Self {
        Self {
            configuration_revision,
            provider,
            provider_revision,
        }
    }

    /// Returns the complete analysis-configuration revision.
    #[must_use]
    pub const fn configuration_revision(self) -> ContentHash {
        self.configuration_revision
    }

    /// Returns the stable provider-set identity.
    #[must_use]
    pub const fn provider(self) -> FactId {
        self.provider
    }

    /// Returns the complete provider-set revision.
    #[must_use]
    pub const fn provider_revision(self) -> ContentHash {
        self.provider_revision
    }
}

/// Parent state required by the next authoritative metadata reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalDiscoveryBaseline {
    metadata: MetadataBaseline,
    inputs: InputSnapshot,
}

impl IncrementalDiscoveryBaseline {
    /// Reconstitutes a previously validated durable baseline.
    ///
    /// Callers must rebuild both parts through their bounded constructors before
    /// using this function. Keeping that validation outside the value prevents
    /// deserialization from bypassing file-count and identity-collision checks.
    #[must_use]
    pub const fn from_validated_parts(metadata: MetadataBaseline, inputs: InputSnapshot) -> Self {
        Self { metadata, inputs }
    }

    /// Returns the source-free metadata and verified content-hash baseline.
    #[must_use]
    pub const fn metadata(&self) -> &MetadataBaseline {
        &self.metadata
    }

    /// Returns the complete typed discovery input fingerprint.
    #[must_use]
    pub const fn inputs(&self) -> &InputSnapshot {
        &self.inputs
    }
}

/// Result of one complete authoritative incremental discovery scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncrementalDiscovery {
    repository: RepositoryId,
    baseline: IncrementalDiscoveryBaseline,
    changes: ChangeSet,
    file_changes: Vec<FileChange>,
    hashed_files: Vec<FileId>,
    hashed_snapshots: BTreeMap<FileId, SourceSnapshot>,
}

/// Monotonic source observations emitted during authoritative content hashing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncrementalDiscoveryProgress {
    /// Source files whose stable snapshots have been examined.
    pub files_examined: u64,
    /// Source bytes contained by the examined stable snapshots.
    pub bytes_examined: u64,
}

/// Immutable reconciliation settings shared by progress-aware discovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IncrementalDiscoveryOptions {
    mode: ReconcileMode,
    limits: DiscoveryLimits,
    maximum_retained_source_bytes: u64,
}

impl IncrementalDiscoveryOptions {
    /// Binds one reconciliation mode to its checked resource limits.
    #[must_use]
    pub const fn new(mode: ReconcileMode, limits: DiscoveryLimits) -> Self {
        Self {
            mode,
            limits,
            maximum_retained_source_bytes: MAX_RETAINED_SOURCE_BYTES,
        }
    }
}

impl IncrementalDiscovery {
    /// Returns the repository whose authoritative handle produced this scan.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns state suitable as the parent of the next reconcile.
    #[must_use]
    pub const fn baseline(&self) -> &IncrementalDiscoveryBaseline {
        &self.baseline
    }

    /// Returns canonical typed file, configuration, and provider transitions.
    #[must_use]
    pub const fn changes(&self) -> &ChangeSet {
        &self.changes
    }

    /// Returns canonical file transitions, including no-op records.
    #[must_use]
    pub fn file_changes(&self) -> &[FileChange] {
        &self.file_changes
    }

    /// Returns files whose bytes were authoritatively hashed during this scan.
    #[must_use]
    pub fn hashed_files(&self) -> &[FileId] {
        &self.hashed_files
    }

    /// Moves snapshots already captured for authoritative content hashing.
    ///
    /// A following clean discovery can reuse these immutable bytes instead of
    /// opening the same files again.
    #[must_use]
    pub fn take_hashed_snapshots(&mut self) -> BTreeMap<FileId, SourceSnapshot> {
        std::mem::take(&mut self.hashed_snapshots)
    }
}

/// Reconciles a complete metadata scan against an optional parent baseline.
///
/// The scan uses only repository-root capabilities and validated relative
/// paths. It reads bounded repository-scoped ignore files before visiting each
/// directory so excluded files and subtrees never enter content hashing.
/// Downstream clean discovery independently reapplies the same policy while
/// classifying admitted content.
///
/// # Errors
///
/// Returns a typed discovery, VFS, incremental-contract, resource-limit,
/// cancellation, or scan/snapshot drift error. Callers should retry drift from
/// a new complete scan.
pub fn discover_incremental(
    root: &RepositoryRoot,
    parent: Option<&IncrementalDiscoveryBaseline>,
    context: IncrementalDiscoveryContext,
    policy: &DiscoveryPolicy,
    mode: ReconcileMode,
    limits: DiscoveryLimits,
    cancellation: &Cancellation,
) -> Result<IncrementalDiscovery, DiscoveryError> {
    discover_incremental_with_progress(
        root,
        parent,
        context,
        policy,
        IncrementalDiscoveryOptions::new(mode, limits),
        cancellation,
        |_| {},
    )
}

/// Reconciles authoritative metadata while reporting content-hashing progress.
///
/// The observer runs after each stable source snapshot and must remain
/// lightweight. Observations contain counts only and never source paths or bytes.
///
/// # Errors
///
/// Returns the same failures as [`discover_incremental`].
pub fn discover_incremental_with_progress(
    root: &RepositoryRoot,
    parent: Option<&IncrementalDiscoveryBaseline>,
    context: IncrementalDiscoveryContext,
    policy: &DiscoveryPolicy,
    options: IncrementalDiscoveryOptions,
    cancellation: &Cancellation,
    mut observe_progress: impl FnMut(IncrementalDiscoveryProgress),
) -> Result<IncrementalDiscovery, DiscoveryError> {
    let IncrementalDiscoveryOptions {
        mode,
        limits,
        maximum_retained_source_bytes,
    } = options;
    let reconcile_limits =
        ReconcileLimits::new(limits.max_entries).map_err(map_incremental_error)?;
    let planning_limits = planning_limits(limits)?;
    let candidate_scan = scan_candidates(root, policy, limits, reconcile_limits, cancellation)?;
    let empty_metadata =
        MetadataBaseline::new([], reconcile_limits, cancellation).map_err(map_incremental_error)?;
    let empty_inputs =
        InputSnapshot::new([], planning_limits, cancellation).map_err(map_incremental_error)?;
    let parent_metadata = parent.map_or(&empty_metadata, IncrementalDiscoveryBaseline::metadata);
    let parent_inputs = parent.map_or(&empty_inputs, IncrementalDiscoveryBaseline::inputs);
    let plan = plan_reconcile(
        parent_metadata,
        &candidate_scan.scan,
        mode,
        reconcile_limits,
        cancellation,
    )
    .map_err(map_incremental_error)?;
    let hashed_files: Vec<_> = plan.files_to_hash().collect();
    let mut snapshot_budget = RetainedSnapshotBudget::new(maximum_retained_source_bytes);
    for file in &hashed_files {
        let expected = candidate_scan
            .descriptors
            .get(file)
            .copied()
            .ok_or(DiscoveryError::IncrementalDrift)?;
        snapshot_budget.reserve(expected.metadata().length())?;
    }
    let mut hashes = BTreeMap::new();
    let mut hashed_snapshots = BTreeMap::new();
    let mut files_examined = 0_u64;
    let mut bytes_examined = 0_u64;
    for file in &hashed_files {
        cancellation.check()?;
        let path = candidate_scan
            .paths
            .get(file)
            .ok_or(DiscoveryError::IncrementalDrift)?;
        let expected = candidate_scan
            .descriptors
            .get(file)
            .copied()
            .ok_or(DiscoveryError::IncrementalDrift)?;
        // The complete scan supplied the aggregate reservation. A smaller
        // capture ceiling keeps a racing file growth outside that reservation.
        let capture_limit = expected.metadata().length().max(1);
        let snapshot = root.snapshot_with_cancellation(path, capture_limit, cancellation)?;
        if snapshot.file() != *file
            || incremental_metadata(snapshot.metadata()) != expected.metadata()
        {
            return Err(DiscoveryError::IncrementalDrift);
        }
        files_examined = files_examined
            .checked_add(1)
            .ok_or(DiscoveryError::IncrementalDrift)?;
        let snapshot_bytes = u64::try_from(snapshot.content().len()).map_err(|_| {
            DiscoveryError::RetainedSnapshotByteLimit {
                observed: u64::MAX,
                maximum: maximum_retained_source_bytes,
            }
        })?;
        bytes_examined = bytes_examined.checked_add(snapshot_bytes).ok_or(
            DiscoveryError::RetainedSnapshotByteLimit {
                observed: u64::MAX,
                maximum: maximum_retained_source_bytes,
            },
        )?;
        observe_progress(IncrementalDiscoveryProgress {
            files_examined,
            bytes_examined,
        });
        hashes.insert(*file, snapshot.content_hash());
        if hashed_snapshots.insert(*file, snapshot).is_some() {
            return Err(DiscoveryError::IncrementalDrift);
        }
    }
    let outcome = plan
        .finish(&hashes, reconcile_limits, cancellation)
        .map_err(map_incremental_error)?;
    let current_inputs = build_inputs(outcome.baseline(), context, planning_limits, cancellation)?;
    let changes = parent_inputs
        .changes_to(&current_inputs, planning_limits, cancellation)
        .map_err(map_incremental_error)?;
    let file_changes = outcome.changes().to_vec();
    let baseline = IncrementalDiscoveryBaseline {
        metadata: outcome.baseline().clone(),
        inputs: current_inputs,
    };

    Ok(IncrementalDiscovery {
        repository: root.repository(),
        baseline,
        changes,
        file_changes,
        hashed_files,
        hashed_snapshots,
    })
}

/// Correlates an incremental metadata result with one clean discovery manifest.
///
/// Clean discovery reapplies repository-scoped ignore files and content
/// classification after the incremental candidate scan. This function makes the
/// clean manifest the generation boundary: only its inputs enter the next
/// baseline, and their paths, lengths, and content hashes must agree with the
/// independently observed incremental result. No filesystem reads occur here.
///
/// # Errors
///
/// Returns [`DiscoveryError::IncrementalDrift`] when the two observations do
/// not describe the same generation inputs or when the supplied context does
/// not match them. Typed limit and cancellation errors are propagated.
pub fn correlate_incremental_manifest(
    observed: &IncrementalDiscovery,
    parent: Option<&IncrementalDiscoveryBaseline>,
    context: IncrementalDiscoveryContext,
    manifest: &DiscoveryManifest,
    limits: DiscoveryLimits,
    cancellation: &Cancellation,
) -> Result<IncrementalDiscovery, DiscoveryError> {
    cancellation.check()?;
    if manifest.repository != observed.repository()
        || manifest.configuration_hash != context.configuration_revision()
        || u64::try_from(manifest.inputs.len()).ok() != Some(manifest.coverage.included)
    {
        return Err(DiscoveryError::IncrementalDrift);
    }

    let reconcile_limits =
        ReconcileLimits::new(limits.max_entries).map_err(map_incremental_error)?;
    let planning_limits = planning_limits(limits)?;
    let expected_observed_inputs = build_inputs(
        observed.baseline().metadata(),
        context,
        planning_limits,
        cancellation,
    )?;
    if &expected_observed_inputs != observed.baseline().inputs() {
        return Err(DiscoveryError::IncrementalDrift);
    }

    let observed_files: BTreeMap<_, _> = observed
        .baseline()
        .metadata()
        .files()
        .map(|file| (file.descriptor().file(), file))
        .collect();
    let mut included = BTreeSet::new();
    let mut included_paths = BTreeSet::new();
    let mut scanned = Vec::with_capacity(manifest.inputs.len());
    let mut manifest_hashes = BTreeMap::new();
    for input in &manifest.inputs {
        cancellation.check()?;
        if !included.insert(input.file) {
            return Err(DiscoveryError::Incremental(
                IncrementalError::DuplicateFile { file: input.file },
            ));
        }
        if !included_paths.insert(input.path.as_str()) {
            return Err(DiscoveryError::IncrementalDrift);
        }
        let path = RelativePath::parse(Path::new(&input.path))?;
        let observed_file = observed_files
            .get(&input.file)
            .copied()
            .ok_or(DiscoveryError::IncrementalDrift)?;
        let descriptor = observed_file.descriptor();
        if descriptor.path_hash() != content_hash(path.identity_bytes())
            || descriptor.metadata().length() != input.bytes
            || observed_file.content_hash() != input.content_hash
        {
            return Err(DiscoveryError::IncrementalDrift);
        }
        scanned.push(ScannedFile::new(descriptor));
        manifest_hashes.insert(input.file, input.content_hash);
    }

    let scan = AuthoritativeScan::new(scanned, reconcile_limits, cancellation)
        .map_err(map_incremental_error)?;
    let empty_metadata =
        MetadataBaseline::new([], reconcile_limits, cancellation).map_err(map_incremental_error)?;
    let empty_inputs =
        InputSnapshot::new([], planning_limits, cancellation).map_err(map_incremental_error)?;
    let parent_metadata = parent.map_or(&empty_metadata, IncrementalDiscoveryBaseline::metadata);
    let parent_inputs = parent.map_or(&empty_inputs, IncrementalDiscoveryBaseline::inputs);
    let plan = plan_reconcile(
        parent_metadata,
        &scan,
        ReconcileMode::Normal,
        reconcile_limits,
        cancellation,
    )
    .map_err(map_incremental_error)?;
    let mut requested_hashes = BTreeMap::new();
    for file in plan.files_to_hash() {
        cancellation.check()?;
        let hash = manifest_hashes
            .get(&file)
            .copied()
            .ok_or(DiscoveryError::IncrementalDrift)?;
        requested_hashes.insert(file, hash);
    }
    let outcome = plan
        .finish(&requested_hashes, reconcile_limits, cancellation)
        .map_err(map_incremental_error)?;
    let current_inputs = build_inputs(outcome.baseline(), context, planning_limits, cancellation)?;
    let changes = parent_inputs
        .changes_to(&current_inputs, planning_limits, cancellation)
        .map_err(map_incremental_error)?;
    let file_changes = outcome.changes().to_vec();
    let hashed_files = observed
        .hashed_files()
        .iter()
        .copied()
        .filter(|file| included.contains(file))
        .collect();
    let hashed_snapshots = observed
        .hashed_snapshots
        .iter()
        .filter_map(|(file, snapshot)| included.contains(file).then_some((*file, snapshot.clone())))
        .collect();
    let baseline = IncrementalDiscoveryBaseline {
        metadata: outcome.baseline().clone(),
        inputs: current_inputs,
    };

    Ok(IncrementalDiscovery {
        repository: observed.repository(),
        baseline,
        changes,
        file_changes,
        hashed_files,
        hashed_snapshots,
    })
}

struct CandidateScan {
    scan: AuthoritativeScan,
    paths: BTreeMap<FileId, RelativePath>,
    descriptors: BTreeMap<FileId, FileDescriptor>,
}

fn scan_candidates(
    root: &RepositoryRoot,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    reconcile_limits: ReconcileLimits,
    cancellation: &Cancellation,
) -> Result<CandidateScan, DiscoveryError> {
    let mut queue = VecDeque::from([(None, 0_usize)]);
    let mut scanned = Vec::new();
    let mut paths = BTreeMap::new();
    let mut descriptors = BTreeMap::new();
    let mut scoped_ignores = ScopedIgnores::default();
    let mut visited = 0_usize;

    while let Some((directory, depth)) = queue.pop_front() {
        cancellation.check()?;
        let entries = root.read_directory(directory.as_ref())?;
        cancellation.check()?;
        if entries.len() > limits.max_entries.saturating_sub(visited) {
            return Err(DiscoveryError::EntryLimit {
                maximum: limits.max_entries,
            });
        }
        load_incremental_scoped_ignore(
            root,
            directory.as_ref(),
            &entries,
            &mut scoped_ignores,
            limits,
            cancellation,
        )?;
        for entry in entries {
            cancellation.check()?;
            visited = visited.saturating_add(1);
            let path = child_path(directory.as_ref(), &entry.name)?;
            let is_directory = entry.kind == EntryKind::Directory;
            let decision =
                policy.decision_with_scoped_ignores(&path, is_directory, &scoped_ignores);
            if decision.excluded && !decision.included {
                continue;
            }
            match entry.kind {
                EntryKind::Directory if depth < limits.max_depth => {
                    queue.push_back((Some(path), depth + 1));
                }
                // `read_directory` leaves platform identity absent when its
                // no-follow file open fails. Clean discovery excludes the same
                // entry as unreadable, so it must not enter reconcile or hashing.
                EntryKind::File
                    if entry.metadata.length <= limits.max_file_bytes
                        && entry.metadata.volume.is_some()
                        && entry.metadata.file_index.is_some() =>
                {
                    let file = root.file_id(&path);
                    let descriptor = FileDescriptor::new(
                        file,
                        content_hash(path.identity_bytes()),
                        incremental_metadata(entry.metadata),
                    );
                    if paths.insert(file, path).is_some()
                        || descriptors.insert(file, descriptor).is_some()
                    {
                        return Err(DiscoveryError::Incremental(
                            IncrementalError::DuplicateFile { file },
                        ));
                    }
                    scanned.push(ScannedFile::new(descriptor));
                }
                EntryKind::File | EntryKind::Directory | EntryKind::Link | EntryKind::Special => {}
            }
        }
    }
    let scan = AuthoritativeScan::new(scanned, reconcile_limits, cancellation)
        .map_err(map_incremental_error)?;
    Ok(CandidateScan {
        scan,
        paths,
        descriptors,
    })
}

fn load_incremental_scoped_ignore(
    root: &RepositoryRoot,
    directory: Option<&RelativePath>,
    entries: &[rootlight_vfs::DirectoryEntry],
    scoped_ignores: &mut ScopedIgnores,
    limits: DiscoveryLimits,
    cancellation: &Cancellation,
) -> Result<(), DiscoveryError> {
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.name == OsStr::new(".gitignore") && entry.kind == EntryKind::File)
    else {
        return Ok(());
    };
    let path = child_path(directory, &entry.name)?;
    let capture_limit = entry.metadata.length.min(limits.max_file_bytes).max(1);
    let snapshot = root.snapshot_with_cancellation(&path, capture_limit, cancellation)?;
    let contents =
        std::str::from_utf8(snapshot.content()).map_err(|_| DiscoveryError::InvalidPolicy)?;
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let scope = directory.map_or("", RelativePath::as_str);
    let source = PathBuf::from(path.as_str());
    let mut builder = GitignoreBuilder::new(Path::new(scope));
    for line in contents.lines() {
        cancellation.check()?;
        builder
            .add_line(Some(source.clone()), line)
            .map_err(|_| DiscoveryError::InvalidPolicy)?;
    }
    cancellation.check()?;
    let matcher = builder.build().map_err(|_| DiscoveryError::InvalidPolicy)?;
    cancellation.check()?;
    scoped_ignores.insert(scope, matcher);
    Ok(())
}

fn incremental_metadata(metadata: SnapshotMetadata) -> FileMetadata {
    let identity = metadata
        .volume
        .zip(metadata.file_index)
        .map(|(volume, file_index)| PlatformFileIdentity::new(volume, file_index));
    match (
        metadata.modified_ns,
        metadata.change_token,
        identity,
        metadata.supports_hash_reuse(),
    ) {
        (Some(modified_ns), Some(change_token), Some(identity), true) => {
            FileMetadata::trusted_with_change_token(
                metadata.length,
                modified_ns,
                change_token,
                identity,
            )
        }
        _ => FileMetadata::untrusted_with_change_token(
            metadata.length,
            metadata.modified_ns,
            metadata.change_token,
            identity,
        ),
    }
}

fn build_inputs(
    baseline: &MetadataBaseline,
    context: IncrementalDiscoveryContext,
    limits: PlanningLimits,
    cancellation: &Cancellation,
) -> Result<InputSnapshot, DiscoveryError> {
    let files = baseline.files().flat_map(|file| {
        let descriptor = file.descriptor();
        [
            InputFingerprint::new(
                InputKey::FileContent(descriptor.file()),
                file.content_hash(),
            ),
            InputFingerprint::new(
                InputKey::FilePath(descriptor.file()),
                descriptor.path_hash(),
            ),
        ]
    });
    let context = [
        InputFingerprint::new(
            InputKey::ConfigurationRevision,
            context.configuration_revision(),
        ),
        InputFingerprint::new(
            InputKey::AdapterVersion(context.provider()),
            context.provider_revision(),
        ),
    ];
    InputSnapshot::new(files.chain(context), limits, cancellation).map_err(map_incremental_error)
}

fn planning_limits(limits: DiscoveryLimits) -> Result<PlanningLimits, DiscoveryError> {
    let max_inputs = limits
        .max_entries
        .checked_mul(2)
        .and_then(|value| value.checked_add(2))
        .ok_or(DiscoveryError::InvalidLimits)?;
    PlanningLimits::new(max_inputs, 1, 1, max_inputs).map_err(map_incremental_error)
}

fn map_incremental_error(error: IncrementalError) -> DiscoveryError {
    match error {
        IncrementalError::Cancelled(cancelled) => DiscoveryError::Cancelled(cancelled),
        error => DiscoveryError::Incremental(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::derive_repository;
    use rootlight_incremental::{
        BaselineFile, HashDecisionReason, MetadataReliability, ReconcileMode,
    };
    use std::fs;
    use tempfile::tempdir_in;

    #[test]
    fn incremental_hashing_preflights_aggregate_snapshot_bytes() {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("local temporary directory is available");
        fs::write(temporary.path().join("first.rs"), b"aa").expect("first fixture is written");
        fs::write(temporary.path().join("second.rs"), b"bbb").expect("second fixture is written");
        let root = RepositoryRoot::open(
            derive_repository(b"incremental-snapshot-budget").id(),
            temporary.path(),
        )
        .expect("fixture repository opens");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let context = IncrementalDiscoveryContext::new(
            content_hash(b"config"),
            FactId::from_bytes([7; 20]),
            content_hash(b"provider"),
        );
        let limits =
            DiscoveryLimits::new(10, 4, 16, 10).expect("fixture limits are within hard ceilings");

        let exact = discover_incremental_with_progress(
            &root,
            None,
            context,
            &policy,
            IncrementalDiscoveryOptions {
                mode: ReconcileMode::Normal,
                limits,
                maximum_retained_source_bytes: 5,
            },
            &Cancellation::new(),
            |_| {},
        )
        .expect("exact aggregate snapshot bytes are admitted");
        assert_eq!(
            exact
                .hashed_snapshots
                .values()
                .map(|snapshot| snapshot.content().len())
                .sum::<usize>(),
            5
        );

        assert!(matches!(
            discover_incremental_with_progress(
                &root,
                None,
                context,
                &policy,
                IncrementalDiscoveryOptions {
                    mode: ReconcileMode::Normal,
                    limits,
                    maximum_retained_source_bytes: 4,
                },
                &Cancellation::new(),
                |_| {},
            ),
            Err(DiscoveryError::RetainedSnapshotByteLimit {
                observed: 5,
                maximum: 4
            })
        ));
    }

    #[test]
    fn incomplete_vfs_metadata_is_untrusted_and_forces_hashing() {
        let metadata = incremental_metadata(SnapshotMetadata {
            length: 7,
            modified_ns: Some(11),
            change_token: None,
            volume: Some(1),
            file_index: Some(2),
        });

        assert_eq!(metadata.reliability(), MetadataReliability::Untrusted);
        assert_eq!(metadata.change_token(), None);

        let limits = ReconcileLimits::new(1).expect("fixture limits are valid");
        let file = FileId::from_bytes([1; 20]);
        let path_hash = ContentHash::from_bytes([2; 32]);
        let identity = PlatformFileIdentity::new(1, 2);
        let baseline = MetadataBaseline::new(
            [BaselineFile::new(
                FileDescriptor::new(
                    file,
                    path_hash,
                    FileMetadata::trusted_with_change_token(7, 11, 12, identity),
                ),
                ContentHash::from_bytes([3; 32]),
            )],
            limits,
            &Cancellation::new(),
        )
        .expect("fixture baseline is valid");
        let scan = AuthoritativeScan::new(
            [ScannedFile::new(FileDescriptor::new(
                file, path_hash, metadata,
            ))],
            limits,
            &Cancellation::new(),
        )
        .expect("fixture scan is valid");
        let plan = plan_reconcile(
            &baseline,
            &scan,
            ReconcileMode::Normal,
            limits,
            &Cancellation::new(),
        )
        .expect("untrusted reconcile plans");

        assert_eq!(plan.files_to_hash().collect::<Vec<_>>(), vec![file]);
        assert_eq!(
            plan.decisions().next().expect("one decision").reason(),
            HashDecisionReason::MetadataUntrusted
        );
    }
}
