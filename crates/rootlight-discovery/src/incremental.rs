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
use rootlight_vfs::{
    EntryKind, MAX_SNAPSHOT_BATCH_BYTES, MAX_SNAPSHOT_BATCH_FILES, RelativePath, RepositoryRoot,
    SnapshotMetadata, SourceSnapshot, VfsError,
};

use crate::{
    DiscoveryError, DiscoveryLimits, DiscoveryManifest, DiscoveryPolicy, MAX_RETAINED_SOURCE_BYTES,
    RetainedSnapshotBudget, ScopedIgnores, child_path, read_directory_with_entry_budget,
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
    complete: bool,
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
    retain_hashed_snapshots: bool,
}

impl IncrementalDiscoveryOptions {
    /// Binds one reconciliation mode to its checked resource limits.
    #[must_use]
    pub const fn new(mode: ReconcileMode, limits: DiscoveryLimits) -> Self {
        Self {
            mode,
            limits,
            maximum_retained_source_bytes: MAX_RETAINED_SOURCE_BYTES,
            retain_hashed_snapshots: true,
        }
    }

    /// Tightens the aggregate source-byte ceiling for this reconciliation.
    ///
    /// Values above the process hard ceiling are clamped, so callers cannot
    /// use this option to relax the bounded-memory contract.
    #[must_use]
    pub const fn with_maximum_retained_source_bytes(mut self, maximum: u64) -> Self {
        self.maximum_retained_source_bytes = if maximum < MAX_RETAINED_SOURCE_BYTES {
            maximum
        } else {
            MAX_RETAINED_SOURCE_BYTES
        };
        self
    }

    /// Avoids retaining hashed source bodies after their fingerprints are recorded.
    ///
    /// Each source remains bounded by the discovery file limit. This option is
    /// intended for callers that will rehydrate exact bytes through a durable or
    /// otherwise staged consumer after the source-free baseline is complete.
    #[must_use]
    pub const fn without_hashed_snapshot_retention(mut self) -> Self {
        self.retain_hashed_snapshots = false;
        self
    }
}

impl IncrementalDiscovery {
    /// Returns the repository whose authoritative handle produced this scan.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Reports whether authoritative traversal covered the complete repository.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.complete
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
        retain_hashed_snapshots,
    } = options;
    let reconcile_limits =
        ReconcileLimits::new(limits.max_entries).map_err(map_incremental_error)?;
    let planning_limits = planning_limits(limits)?;
    let candidate_scan = scan_candidates(
        root,
        policy,
        limits,
        reconcile_limits,
        retain_hashed_snapshots.then_some(maximum_retained_source_bytes),
        cancellation,
    )?;
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
    if retain_hashed_snapshots {
        let mut snapshot_budget = RetainedSnapshotBudget::new(maximum_retained_source_bytes);
        for file in &hashed_files {
            let expected = candidate_scan
                .descriptors
                .get(file)
                .copied()
                .ok_or(DiscoveryError::IncrementalDrift)?;
            snapshot_budget.reserve(expected.metadata().length())?;
        }
    }
    let mut hashes = BTreeMap::new();
    let mut hashed_snapshots = BTreeMap::new();
    let mut files_examined = 0_u64;
    let mut bytes_examined = 0_u64;
    let mut offset = 0;
    while offset < hashed_files.len() {
        cancellation.check()?;
        let mut requests = Vec::new();
        requests
            .try_reserve_exact(MAX_SNAPSHOT_BATCH_FILES.min(hashed_files.len() - offset))
            .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
        let mut batch_bytes = 0_u64;
        let mut end = offset;
        while end < hashed_files.len() && requests.len() < MAX_SNAPSHOT_BATCH_FILES {
            let file = hashed_files[end];
            let path = candidate_scan
                .paths
                .get(&file)
                .ok_or(DiscoveryError::IncrementalDrift)?;
            let expected = candidate_scan
                .descriptors
                .get(&file)
                .copied()
                .ok_or(DiscoveryError::IncrementalDrift)?;
            // The complete scan supplied the aggregate reservation. A smaller
            // capture ceiling keeps a racing file growth outside that reservation.
            let capture_limit = expected.metadata().length().max(1);
            let next_bytes = batch_bytes.checked_add(capture_limit);
            if !requests.is_empty()
                && next_bytes.is_none_or(|bytes| bytes > MAX_SNAPSHOT_BATCH_BYTES)
            {
                break;
            }
            batch_bytes = next_bytes.ok_or(DiscoveryError::RetainedSnapshotByteLimit {
                observed: u64::MAX,
                maximum: MAX_SNAPSHOT_BATCH_BYTES,
            })?;
            requests.push((path.clone(), capture_limit));
            end += 1;
        }
        let snapshots = root.snapshot_batch_with_cancellation(&requests, cancellation)?;
        if snapshots.len() != end - offset {
            return Err(DiscoveryError::IncrementalDrift);
        }
        for (file, snapshot) in hashed_files[offset..end].iter().zip(snapshots) {
            let snapshot = snapshot?;
            let expected = candidate_scan
                .descriptors
                .get(file)
                .copied()
                .ok_or(DiscoveryError::IncrementalDrift)?;
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
            if retain_hashed_snapshots && hashed_snapshots.insert(*file, snapshot).is_some() {
                return Err(DiscoveryError::IncrementalDrift);
            }
        }
        offset = end;
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
        complete: candidate_scan.complete,
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
        || (manifest.coverage.complete && !observed.is_complete())
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
        complete: observed.is_complete() && manifest.coverage.complete,
        baseline,
        changes,
        file_changes,
        hashed_files,
        hashed_snapshots,
    })
}

struct CandidateScan {
    complete: bool,
    scan: AuthoritativeScan,
    paths: BTreeMap<FileId, RelativePath>,
    descriptors: BTreeMap<FileId, FileDescriptor>,
}

fn scan_candidates(
    root: &RepositoryRoot,
    policy: &DiscoveryPolicy,
    limits: DiscoveryLimits,
    reconcile_limits: ReconcileLimits,
    maximum_retained_source_bytes: Option<u64>,
    cancellation: &Cancellation,
) -> Result<CandidateScan, DiscoveryError> {
    let mut queue = VecDeque::from([(None, 0_usize)]);
    let mut scanned = Vec::new();
    let mut paths = BTreeMap::new();
    let mut descriptors = BTreeMap::new();
    let mut scoped_ignores = ScopedIgnores::default();
    let mut visited = 0_usize;
    let mut retained_source_bytes = 0_u64;
    let mut complete = true;

    'directories: while let Some((directory, depth)) = queue.pop_front() {
        cancellation.check()?;
        let bounded = read_directory_with_entry_budget(
            root,
            directory.as_ref(),
            limits.max_entries.saturating_sub(visited),
            cancellation,
        )?;
        let directory_complete = bounded.is_complete();
        let entries = bounded.into_entries();
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
                // VFS enumeration leaves platform identity absent when its
                // no-follow file open fails. Clean discovery excludes the same
                // entry as unreadable, so it must not enter reconcile or hashing.
                EntryKind::File
                    if entry.metadata.length <= limits.max_file_bytes
                        && entry.metadata.volume.is_some()
                        && entry.metadata.file_index.is_some() =>
                {
                    if let Some(maximum_retained_source_bytes) = maximum_retained_source_bytes {
                        let Some(observed_source_bytes) =
                            retained_source_bytes.checked_add(entry.metadata.length)
                        else {
                            complete = false;
                            queue.clear();
                            break 'directories;
                        };
                        if observed_source_bytes > maximum_retained_source_bytes {
                            // Preserve the deterministic traversal prefix instead
                            // of selecting later files by their relative size.
                            complete = false;
                            queue.clear();
                            break 'directories;
                        }
                        retained_source_bytes = observed_source_bytes;
                    }
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
        if !directory_complete {
            complete = false;
            queue.clear();
        }
    }
    let scan = AuthoritativeScan::new(scanned, reconcile_limits, cancellation)
        .map_err(map_incremental_error)?;
    Ok(CandidateScan {
        complete,
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
    PlanningLimits::new(max_inputs, 1, 1, 1, max_inputs).map_err(map_incremental_error)
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
    use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};
    use rootlight_ids::derive_repository;
    use rootlight_incremental::{
        BaselineFile, HashDecisionReason, MetadataReliability, ReconcileMode,
    };
    use std::fs;
    use tempfile::tempdir_in;

    #[test]
    fn reconciliation_options_can_only_tighten_the_hard_source_ceiling() {
        let limits =
            DiscoveryLimits::new(2, 4, 1024, 10).expect("test limits are within hard ceilings");

        let tightened = IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits)
            .with_maximum_retained_source_bytes(4);
        assert_eq!(tightened.maximum_retained_source_bytes, 4);

        let attempted_relaxation = IncrementalDiscoveryOptions::new(ReconcileMode::Normal, limits)
            .with_maximum_retained_source_bytes(MAX_RETAINED_SOURCE_BYTES.saturating_add(1));
        assert_eq!(
            attempted_relaxation.maximum_retained_source_bytes,
            MAX_RETAINED_SOURCE_BYTES
        );
    }

    #[test]
    fn incremental_and_clean_discovery_share_the_incomplete_prefix() {
        let current = std::env::current_dir().expect("current directory is available");
        let temporary = tempdir_in(current).expect("local temporary directory is available");
        for name in ["zeta.rs", "alpha.rs", "middle.rs"] {
            fs::write(temporary.path().join(name), b"pub fn item() {}\n")
                .expect("fixture file is written");
        }
        let root = RepositoryRoot::open(
            derive_repository(b"incremental-bounded-prefix").id(),
            temporary.path(),
        )
        .expect("fixture repository opens");
        let config = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::Defaults,
            contents: "version = \"1.0\"",
        }])
        .expect("minimal configuration resolves");
        let policy = DiscoveryPolicy::build(Vec::new(), false).expect("policy builds");
        let limits =
            DiscoveryLimits::new(2, 4, 1024, 10).expect("test limits are within hard ceilings");
        let context = IncrementalDiscoveryContext::new(
            config.hash(),
            FactId::from_bytes([7; 20]),
            content_hash(b"provider"),
        );
        let cancellation = Cancellation::new();

        let observed = discover_incremental(
            &root,
            None,
            context,
            &policy,
            ReconcileMode::Normal,
            limits,
            &cancellation,
        )
        .expect("incremental discovery publishes a partial prefix");
        let manifest = crate::discover(&root, &config, &policy, limits, &cancellation)
            .expect("clean discovery publishes the same partial prefix");
        let correlated = correlate_incremental_manifest(
            &observed,
            None,
            context,
            &manifest,
            limits,
            &cancellation,
        )
        .expect("matching partial scans correlate");

        assert!(!observed.is_complete());
        assert!(!manifest.coverage.complete);
        assert!(!correlated.is_complete());
        assert_eq!(
            manifest
                .inputs
                .iter()
                .map(|input| input.path.as_str())
                .collect::<Vec<_>>(),
            ["alpha.rs", "middle.rs"]
        );
    }

    #[test]
    fn incremental_hashing_truncates_at_the_aggregate_snapshot_budget() {
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
                retain_hashed_snapshots: true,
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

        let partial = discover_incremental_with_progress(
            &root,
            None,
            context,
            &policy,
            IncrementalDiscoveryOptions {
                mode: ReconcileMode::Normal,
                limits,
                maximum_retained_source_bytes: 4,
                retain_hashed_snapshots: true,
            },
            &Cancellation::new(),
            |_| {},
        )
        .expect("aggregate source exhaustion publishes a partial prefix");
        assert!(!partial.is_complete());
        assert_eq!(partial.hashed_snapshots.len(), 1);
        assert_eq!(
            partial
                .hashed_snapshots
                .values()
                .map(|snapshot| snapshot.content().len())
                .sum::<usize>(),
            2
        );

        let mut progress = IncrementalDiscoveryProgress {
            files_examined: 0,
            bytes_examined: 0,
        };
        let streaming = discover_incremental_with_progress(
            &root,
            None,
            context,
            &policy,
            IncrementalDiscoveryOptions {
                mode: ReconcileMode::Normal,
                limits,
                maximum_retained_source_bytes: 4,
                retain_hashed_snapshots: false,
            },
            &Cancellation::new(),
            |observed| progress = observed,
        )
        .expect("source-free hashing crosses the snapshot retention boundary");
        assert!(streaming.is_complete());
        assert_eq!(streaming.hashed_files.len(), 2);
        assert!(streaming.hashed_snapshots.is_empty());
        assert_eq!(progress.files_examined, 2);
        assert_eq!(progress.bytes_examined, 5);
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
