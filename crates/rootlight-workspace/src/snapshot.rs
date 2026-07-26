//! Immutable workspace snapshots that pin every participating generation.
//!
//! Partial construction retains explicit per-repository failures; omitted
//! repositories never become silent exhaustive negatives.

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use serde::Serialize;

use crate::{
    CrossLinkVersion, RepositoryState, WorkspaceCatalog, WorkspaceId, WorkspaceSnapshotId,
    identity::identity_hash,
};

const SNAPSHOT_SCHEMA_VERSION: u16 = 1;
const HARD_MAX_MEMBERS: usize = 1_024;
const HARD_MAX_FAILURES: usize = 1_024;

/// Snapshot construction capacities no broader than hard process limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotLimits {
    max_members: usize,
    max_failures: usize,
}

impl SnapshotLimits {
    /// Creates a snapshot limit policy.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::InvalidLimits`] when either value is zero or
    /// exceeds its hard process ceiling.
    pub fn new(max_members: usize, max_failures: usize) -> Result<Self, SnapshotError> {
        if max_members == 0
            || max_members > HARD_MAX_MEMBERS
            || max_failures == 0
            || max_failures > HARD_MAX_FAILURES
        {
            return Err(SnapshotError::InvalidLimits);
        }
        Ok(Self {
            max_members,
            max_failures,
        })
    }

    /// Returns the maximum requested and available member count.
    #[must_use]
    pub const fn max_members(self) -> usize {
        self.max_members
    }

    /// Returns the maximum explicit failure count.
    #[must_use]
    pub const fn max_failures(self) -> usize {
        self.max_failures
    }
}

impl Default for SnapshotLimits {
    fn default() -> Self {
        Self {
            max_members: 128,
            max_failures: 128,
        }
    }
}

/// Whether construction requires every requested member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotBuildMode {
    /// Any unavailable member rejects the complete snapshot.
    Strict,
    /// Available members form a snapshot and every omission remains explicit.
    AllowPartial,
}

/// One exact repository-generation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotMember {
    repository: RepositoryId,
    generation: GenerationId,
}

impl SnapshotMember {
    /// Creates one exact immutable member.
    #[must_use]
    pub const fn new(repository: RepositoryId, generation: GenerationId) -> Self {
        Self {
            repository,
            generation,
        }
    }

    /// Returns the repository identity.
    #[must_use]
    pub const fn repository(self) -> RepositoryId {
        self.repository
    }

    /// Returns the pinned generation identity.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }
}

/// Source-free reason one requested member was omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SnapshotFailureKind {
    /// Repository is not registered.
    MissingRepository,
    /// Source access is not authorized.
    Unauthorized,
    /// Repository is temporarily unavailable.
    Unavailable,
    /// Repository integrity failed.
    Corrupt,
    /// Repository registration was deleted.
    Deleted,
    /// Exact immutable generation is no longer retained.
    ReclaimedGeneration,
    /// Repository is reindexing and the requested generation is unavailable.
    Reindexing,
}

/// Explicit failure for one requested repository-generation pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotFailure {
    member: SnapshotMember,
    kind: SnapshotFailureKind,
}

impl SnapshotFailure {
    /// Returns the requested member.
    #[must_use]
    pub const fn member(self) -> SnapshotMember {
        self.member
    }

    /// Returns the source-free failure class.
    #[must_use]
    pub const fn kind(self) -> SnapshotFailureKind {
        self.kind
    }
}

/// Declarative request for one immutable workspace view.
#[derive(Debug, Clone)]
pub struct WorkspaceSnapshotRequest {
    members: Vec<SnapshotMember>,
    configuration: ContentHash,
    cross_link_version: CrossLinkVersion,
    observed_epoch: u64,
    valid_until_epoch: u64,
}

impl WorkspaceSnapshotRequest {
    /// Creates an empty request bound to exact configuration and link versions.
    #[must_use]
    pub const fn new(
        configuration: ContentHash,
        cross_link_version: CrossLinkVersion,
        observed_epoch: u64,
        valid_until_epoch: u64,
    ) -> Self {
        Self {
            members: Vec::new(),
            configuration,
            cross_link_version,
            observed_epoch,
            valid_until_epoch,
        }
    }

    /// Adds one exact repository-generation pair.
    #[must_use]
    pub fn with_member(mut self, repository: RepositoryId, generation: GenerationId) -> Self {
        self.members
            .push(SnapshotMember::new(repository, generation));
        self
    }

    /// Returns requested members before canonicalization.
    #[must_use]
    pub fn members(&self) -> &[SnapshotMember] {
        &self.members
    }
}

/// Deterministic immutable multi-repository snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSnapshot {
    schema_version: u16,
    id: WorkspaceSnapshotId,
    workspace: WorkspaceId,
    configuration: ContentHash,
    cross_link_version: CrossLinkVersion,
    observed_epoch: u64,
    valid_until_epoch: u64,
    requested_members: usize,
    members: Vec<SnapshotMember>,
    failures: Vec<SnapshotFailure>,
}

impl WorkspaceSnapshot {
    /// Resolves exact repository generations against one catalog view.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] for malformed requests, strict-mode failures,
    /// exhausted bounds, complete unavailability, expiry, or cancellation.
    pub fn build(
        catalog: &WorkspaceCatalog,
        mut request: WorkspaceSnapshotRequest,
        mode: SnapshotBuildMode,
        limits: SnapshotLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, SnapshotError> {
        cancellation.check()?;
        if request.members.is_empty() {
            return Err(SnapshotError::EmptyRequest);
        }
        if request.members.len() > limits.max_members {
            return Err(SnapshotError::MemberLimit);
        }
        if request.observed_epoch > request.valid_until_epoch {
            return Err(SnapshotError::Expired);
        }
        request.members.sort();
        if request
            .members
            .windows(2)
            .any(|pair| pair[0].repository == pair[1].repository)
        {
            return Err(SnapshotError::DuplicateRepository);
        }

        let mut members = Vec::with_capacity(request.members.len());
        let mut failures = Vec::new();
        for (index, requested) in request.members.iter().copied().enumerate() {
            if index % 64 == 0 {
                cancellation.check()?;
            }
            match resolve_member(catalog, requested) {
                Ok(member) => members.push(member),
                Err(kind) => {
                    if failures.len() >= limits.max_failures {
                        return Err(SnapshotError::FailureLimit);
                    }
                    failures.push(SnapshotFailure {
                        member: requested,
                        kind,
                    });
                }
            }
        }
        if !failures.is_empty() && mode == SnapshotBuildMode::Strict {
            return Err(SnapshotError::StrictMemberFailure);
        }
        if members.is_empty() {
            return Err(SnapshotError::NoAvailableMembers);
        }
        cancellation.check()?;
        let id = derive_snapshot_id(catalog.id(), &request, &members, &failures);
        Ok(Self {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            id,
            workspace: catalog.id(),
            configuration: request.configuration,
            cross_link_version: request.cross_link_version,
            observed_epoch: request.observed_epoch,
            valid_until_epoch: request.valid_until_epoch,
            requested_members: request.members.len(),
            members,
            failures,
        })
    }

    /// Revalidates retention, authorization, availability, and expiry.
    ///
    /// Advancing an unrelated or participating repository does not invalidate a
    /// retained generation. No live generation pointer is followed.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when workspace identity, expiry, any member,
    /// or cancellation no longer validates.
    pub fn validate(
        &self,
        catalog: &WorkspaceCatalog,
        observed_epoch: u64,
        cancellation: &Cancellation,
    ) -> Result<(), SnapshotError> {
        cancellation.check()?;
        if self.workspace != catalog.id() {
            return Err(SnapshotError::WorkspaceMismatch);
        }
        if observed_epoch > self.valid_until_epoch {
            return Err(SnapshotError::Expired);
        }
        for (index, member) in self.members.iter().copied().enumerate() {
            if index % 64 == 0 {
                cancellation.check()?;
            }
            resolve_member(catalog, member).map_err(|_| SnapshotError::MemberInvalidated)?;
        }
        Ok(())
    }

    /// Returns the snapshot schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }

    /// Returns the deterministic snapshot identity.
    #[must_use]
    pub const fn id(&self) -> WorkspaceSnapshotId {
        self.id
    }

    /// Returns the workspace identity.
    #[must_use]
    pub const fn workspace(&self) -> WorkspaceId {
        self.workspace
    }

    /// Returns the exact configuration identity.
    #[must_use]
    pub const fn configuration(&self) -> ContentHash {
        self.configuration
    }

    /// Returns the exact cross-link configuration identity.
    #[must_use]
    pub const fn cross_link_version(&self) -> CrossLinkVersion {
        self.cross_link_version
    }

    /// Returns the request observation epoch.
    #[must_use]
    pub const fn observed_epoch(&self) -> u64 {
        self.observed_epoch
    }

    /// Returns the inclusive caller-defined validity epoch.
    #[must_use]
    pub const fn valid_until_epoch(&self) -> u64 {
        self.valid_until_epoch
    }

    /// Returns the total number of requested repositories.
    #[must_use]
    pub const fn requested_members(&self) -> usize {
        self.requested_members
    }

    /// Returns available members in repository identity order.
    #[must_use]
    pub fn members(&self) -> &[SnapshotMember] {
        &self.members
    }

    /// Returns explicit omissions in repository identity order.
    #[must_use]
    pub fn failures(&self) -> &[SnapshotFailure] {
        &self.failures
    }

    /// Reports whether every requested repository is present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.members.len() == self.requested_members
    }

    /// Finds the generation pinned for one participating repository.
    #[must_use]
    pub fn generation_for(&self, repository: RepositoryId) -> Option<GenerationId> {
        self.members
            .binary_search_by_key(&repository, |member| member.repository)
            .ok()
            .and_then(|index| self.members.get(index))
            .map(|member| member.generation)
    }
}

fn resolve_member(
    catalog: &WorkspaceCatalog,
    member: SnapshotMember,
) -> Result<SnapshotMember, SnapshotFailureKind> {
    let Some(repository) = catalog.repository(member.repository) else {
        return Err(SnapshotFailureKind::MissingRepository);
    };
    if !repository.authorized() {
        return Err(SnapshotFailureKind::Unauthorized);
    }
    match repository.state() {
        RepositoryState::Deleted => Err(SnapshotFailureKind::Deleted),
        RepositoryState::Corrupt => Err(SnapshotFailureKind::Corrupt),
        RepositoryState::Unavailable => Err(SnapshotFailureKind::Unavailable),
        RepositoryState::Reindexing if !repository.retains(member.generation) => {
            Err(SnapshotFailureKind::Reindexing)
        }
        RepositoryState::Ready | RepositoryState::Reindexing => {
            if repository.retains(member.generation) {
                Ok(member)
            } else {
                Err(SnapshotFailureKind::ReclaimedGeneration)
            }
        }
    }
}

fn derive_snapshot_id(
    workspace: WorkspaceId,
    request: &WorkspaceSnapshotRequest,
    members: &[SnapshotMember],
    failures: &[SnapshotFailure],
) -> WorkspaceSnapshotId {
    let mut membership = Vec::with_capacity(
        members
            .len()
            .saturating_mul(40)
            .saturating_add(failures.len().saturating_mul(41))
            .saturating_add(24),
    );
    membership.extend_from_slice(&request.observed_epoch.to_be_bytes());
    membership.extend_from_slice(&request.valid_until_epoch.to_be_bytes());
    membership.extend_from_slice(
        &u64::try_from(request.members.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes(),
    );
    for member in members {
        membership.extend_from_slice(member.repository.as_bytes());
        membership.extend_from_slice(member.generation.as_bytes());
    }
    for failure in failures {
        membership.extend_from_slice(failure.member.repository.as_bytes());
        membership.extend_from_slice(failure.member.generation.as_bytes());
        membership.push(failure_kind_code(failure.kind));
    }
    WorkspaceSnapshotId::from_hash(identity_hash(
        b"rootlight/workspace-snapshot/v1",
        &[
            workspace.as_hash().as_bytes(),
            request.configuration.as_bytes(),
            request.cross_link_version.as_hash().as_bytes(),
            &membership,
        ],
    ))
}

const fn failure_kind_code(kind: SnapshotFailureKind) -> u8 {
    match kind {
        SnapshotFailureKind::MissingRepository => 0,
        SnapshotFailureKind::Unauthorized => 1,
        SnapshotFailureKind::Unavailable => 2,
        SnapshotFailureKind::Corrupt => 3,
        SnapshotFailureKind::Deleted => 4,
        SnapshotFailureKind::ReclaimedGeneration => 5,
        SnapshotFailureKind::Reindexing => 6,
    }
}

/// Invalid, unavailable, expired, or cancelled workspace snapshot.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SnapshotError {
    /// Limit policy is zero or broader than a hard ceiling.
    #[error("workspace snapshot limits are invalid")]
    InvalidLimits,
    /// Request contains no repositories.
    #[error("workspace snapshot request is empty")]
    EmptyRequest,
    /// Request contains one repository more than once.
    #[error("workspace snapshot request repeats a repository")]
    DuplicateRepository,
    /// Member bound is exhausted.
    #[error("workspace snapshot member limit exceeded")]
    MemberLimit,
    /// Failure-accounting bound is exhausted.
    #[error("workspace snapshot failure limit exceeded")]
    FailureLimit,
    /// Strict construction observed at least one unavailable member.
    #[error("workspace strict snapshot has an unavailable member")]
    StrictMemberFailure,
    /// Partial construction found no valid repository generation.
    #[error("workspace snapshot has no available member")]
    NoAvailableMembers,
    /// Request or validation epoch is beyond the declared lifetime.
    #[error("workspace snapshot is expired")]
    Expired,
    /// Snapshot belongs to another workspace.
    #[error("workspace snapshot identity does not match the catalog")]
    WorkspaceMismatch,
    /// A previously pinned member no longer validates.
    #[error("workspace snapshot member is no longer valid")]
    MemberInvalidated,
    /// Cooperative cancellation won.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}
