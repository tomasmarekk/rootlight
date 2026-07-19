//! Capability-confined metadata scans for authoritative incremental reconcile.
//!
//! Watcher events never enter this API. Complete bounded scans decide which
//! files require content hashing and derive canonical typed generation changes.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    path::Path,
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, RepositoryId, content_hash};
use rootlight_incremental::{
    AuthoritativeScan, ChangeSet, FileChange, FileDescriptor, FileMetadata, IncrementalError,
    InputFingerprint, InputKey, InputSnapshot, MetadataBaseline, PlanningLimits,
    PlatformFileIdentity, ReconcileLimits, ReconcileMode, ScannedFile, plan_reconcile,
};
use rootlight_vfs::{EntryKind, RelativePath, RepositoryRoot, SnapshotMetadata};

use crate::{DiscoveryError, DiscoveryLimits, DiscoveryManifest, DiscoveryPolicy, child_path};

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
}

/// Reconciles a complete metadata scan against an optional parent baseline.
///
/// The scan uses only repository-root capabilities and validated relative
/// paths. It applies compiled policy layers but conservatively retains files
/// excluded only by repository-scoped ignore contents. This avoids reading an
/// unchanged ignore file merely to decide whether another unchanged file needs
/// hashing; downstream clean discovery still applies scoped ignore semantics.
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
    let mut hashes = BTreeMap::new();
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
        let snapshot =
            root.snapshot_with_cancellation(path, limits.max_file_bytes, cancellation)?;
        if snapshot.file() != *file
            || incremental_metadata(snapshot.metadata()) != expected.metadata()
        {
            return Err(DiscoveryError::IncrementalDrift);
        }
        hashes.insert(*file, snapshot.content_hash());
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
    })
}

/// Correlates an incremental metadata result with one clean discovery manifest.
///
/// Clean discovery applies repository-scoped ignore files and content
/// classification after the incremental candidate scan. This function makes
/// the clean manifest the generation boundary: only its inputs enter the next
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
        for entry in entries {
            cancellation.check()?;
            visited = visited.saturating_add(1);
            let path = child_path(directory.as_ref(), &entry.name)?;
            let is_directory = entry.kind == EntryKind::Directory;
            let decision = policy.layered_decision(&path, is_directory, None);
            if decision.excluded && !decision.included {
                continue;
            }
            match entry.kind {
                EntryKind::Directory if depth < limits.max_depth => {
                    queue.push_back((Some(path), depth + 1));
                }
                EntryKind::File if entry.metadata.length <= limits.max_file_bytes => {
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
    use rootlight_incremental::{
        BaselineFile, HashDecisionReason, MetadataReliability, ReconcileMode,
    };

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
