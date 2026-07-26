//! Capability-free catalog for repository topology and independent generations.
//!
//! Hosts canonicalize roots and discover Git or package metadata before calling
//! this layer; only opaque identities and bounded declarative facts are stored.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, GenerationId, RepositoryId};
use serde::{Deserialize, Serialize};

use crate::{RepositoryRootIdentity, SharedContentIdentity, WorkspaceId};

const HARD_MAX_REPOSITORIES: usize = 1_024;
const HARD_MAX_ALIASES: usize = 8_192;
const HARD_MAX_PACKAGES: usize = 65_536;
const HARD_MAX_RETAINED_GENERATIONS: usize = 1_024;
const MAX_ALIAS_BYTES: usize = 128;

/// Bounded catalog capacities no broader than process-wide hard ceilings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogLimits {
    max_repositories: usize,
    max_aliases: usize,
    max_packages: usize,
    max_retained_generations_per_repository: usize,
}

impl CatalogLimits {
    /// Creates a catalog limit policy.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidLimits`] when a value is zero or exceeds
    /// the corresponding process-wide hard ceiling.
    pub fn new(
        max_repositories: usize,
        max_aliases: usize,
        max_packages: usize,
        max_retained_generations_per_repository: usize,
    ) -> Result<Self, CatalogError> {
        if max_repositories == 0
            || max_repositories > HARD_MAX_REPOSITORIES
            || max_aliases == 0
            || max_aliases > HARD_MAX_ALIASES
            || max_packages == 0
            || max_packages > HARD_MAX_PACKAGES
            || max_retained_generations_per_repository == 0
            || max_retained_generations_per_repository > HARD_MAX_RETAINED_GENERATIONS
        {
            return Err(CatalogError::InvalidLimits);
        }
        Ok(Self {
            max_repositories,
            max_aliases,
            max_packages,
            max_retained_generations_per_repository,
        })
    }

    /// Returns the maximum registered repository count.
    #[must_use]
    pub const fn max_repositories(self) -> usize {
        self.max_repositories
    }

    /// Returns the maximum global alias count.
    #[must_use]
    pub const fn max_aliases(self) -> usize {
        self.max_aliases
    }

    /// Returns the maximum global package count.
    #[must_use]
    pub const fn max_packages(self) -> usize {
        self.max_packages
    }

    /// Returns the generation-retention ceiling for one repository.
    #[must_use]
    pub const fn max_retained_generations_per_repository(self) -> usize {
        self.max_retained_generations_per_repository
    }
}

impl Default for CatalogLimits {
    fn default() -> Self {
        Self {
            max_repositories: 128,
            max_aliases: 1_024,
            max_packages: 8_192,
            max_retained_generations_per_repository: 128,
        }
    }
}

/// Presentation alias that never participates in repository identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceAlias(String);

impl WorkspaceAlias {
    /// Validates a lowercase ASCII alias.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::InvalidAlias`] for an empty, oversized, or
    /// noncanonical label.
    pub fn new(value: &str) -> Result<Self, CatalogError> {
        if value.is_empty()
            || value.len() > MAX_ALIAS_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            })
        {
            return Err(CatalogError::InvalidAlias);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical alias.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity of one package scope within a registered repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(ContentHash);

impl PackageId {
    /// Creates a package identity from its canonical descriptor hash.
    #[must_use]
    pub const fn from_hash(hash: ContentHash) -> Self {
        Self(hash)
    }

    /// Returns the canonical descriptor hash.
    #[must_use]
    pub const fn as_hash(self) -> ContentHash {
        self.0
    }
}

/// Opaque declarative package metadata attached to one repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageDescriptor {
    id: PackageId,
    scope_identity: ContentHash,
    manifest_identity: ContentHash,
}

impl PackageDescriptor {
    /// Creates a package descriptor from prevalidated immutable identities.
    #[must_use]
    pub const fn new(
        id: PackageId,
        scope_identity: ContentHash,
        manifest_identity: ContentHash,
    ) -> Self {
        Self {
            id,
            scope_identity,
            manifest_identity,
        }
    }

    /// Returns the package identity.
    #[must_use]
    pub const fn id(self) -> PackageId {
        self.id
    }

    /// Returns the opaque root-relative package scope identity.
    #[must_use]
    pub const fn scope_identity(self) -> ContentHash {
        self.scope_identity
    }

    /// Returns the exact declarative manifest identity.
    #[must_use]
    pub const fn manifest_identity(self) -> ContentHash {
        self.manifest_identity
    }
}

/// Ownership relation between one repository root and another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "repository")]
#[non_exhaustive]
pub enum RepositoryTopology {
    /// Independent repository root.
    Standalone,
    /// Root containing independently modeled package scopes.
    Monorepo,
    /// Nested repository owned independently from its enclosing root.
    Nested(RepositoryId),
    /// Worktree sharing immutable object data with a primary checkout.
    Worktree(RepositoryId),
    /// Submodule whose source and generation lifecycle remain independent.
    Submodule(RepositoryId),
}

/// Declarative registration input for one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryDescriptor {
    repository: RepositoryId,
    root: RepositoryRootIdentity,
    shared_content: SharedContentIdentity,
    topology: RepositoryTopology,
    aliases: Vec<WorkspaceAlias>,
    packages: Vec<PackageDescriptor>,
}

impl RepositoryDescriptor {
    /// Creates a standalone repository descriptor.
    #[must_use]
    pub const fn new(
        repository: RepositoryId,
        root: RepositoryRootIdentity,
        shared_content: SharedContentIdentity,
    ) -> Self {
        Self {
            repository,
            root,
            shared_content,
            topology: RepositoryTopology::Standalone,
            aliases: Vec::new(),
            packages: Vec::new(),
        }
    }

    /// Sets the explicit root topology.
    #[must_use]
    pub const fn with_topology(mut self, topology: RepositoryTopology) -> Self {
        self.topology = topology;
        self
    }

    /// Adds one presentation alias for atomic validation during registration.
    #[must_use]
    pub fn with_alias(mut self, alias: WorkspaceAlias) -> Self {
        self.aliases.push(alias);
        self
    }

    /// Adds one package scope for atomic validation during registration.
    #[must_use]
    pub fn with_package(mut self, package: PackageDescriptor) -> Self {
        self.packages.push(package);
        self
    }
}

/// Availability of one independently managed repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RepositoryState {
    /// The current generation is available for new snapshots.
    Ready,
    /// Publication is in progress; retained generations remain readable.
    Reindexing,
    /// The host cannot currently open this repository.
    Unavailable,
    /// Integrity validation failed for the repository.
    Corrupt,
    /// Registration is tombstoned and cannot be reactivated in place.
    Deleted,
}

/// Registered repository state with independent retained generations.
#[derive(Debug, Clone)]
pub struct CatalogRepository {
    repository: RepositoryId,
    root: RepositoryRootIdentity,
    shared_content: SharedContentIdentity,
    topology: RepositoryTopology,
    aliases: Vec<WorkspaceAlias>,
    packages: Vec<PackageDescriptor>,
    state: RepositoryState,
    authorized: bool,
    current_generation: Option<GenerationId>,
    retained_generations: BTreeSet<GenerationId>,
}

impl CatalogRepository {
    /// Returns the stable repository identity.
    #[must_use]
    pub const fn repository(&self) -> RepositoryId {
        self.repository
    }

    /// Returns the opaque canonical root identity.
    #[must_use]
    pub const fn root(&self) -> RepositoryRootIdentity {
        self.root
    }

    /// Returns the immutable content identity shared by related worktrees.
    #[must_use]
    pub const fn shared_content(&self) -> SharedContentIdentity {
        self.shared_content
    }

    /// Returns the declared topology.
    #[must_use]
    pub const fn topology(&self) -> RepositoryTopology {
        self.topology
    }

    /// Returns aliases in canonical order.
    #[must_use]
    pub fn aliases(&self) -> &[WorkspaceAlias] {
        &self.aliases
    }

    /// Returns package descriptors in identity order.
    #[must_use]
    pub fn packages(&self) -> &[PackageDescriptor] {
        &self.packages
    }

    /// Returns current availability.
    #[must_use]
    pub const fn state(&self) -> RepositoryState {
        self.state
    }

    /// Returns whether source access is authorized for this repository.
    #[must_use]
    pub const fn authorized(&self) -> bool {
        self.authorized
    }

    /// Returns the generation selected for new strict snapshots.
    #[must_use]
    pub const fn current_generation(&self) -> Option<GenerationId> {
        self.current_generation
    }

    /// Reports whether an exact immutable generation remains retained.
    #[must_use]
    pub fn retains(&self, generation: GenerationId) -> bool {
        self.retained_generations.contains(&generation)
    }
}

/// In-memory workspace catalog with no storage or discovery capability.
#[derive(Debug, Clone)]
pub struct WorkspaceCatalog {
    id: WorkspaceId,
    limits: CatalogLimits,
    repositories: BTreeMap<RepositoryId, CatalogRepository>,
    roots: BTreeMap<RepositoryRootIdentity, RepositoryId>,
    aliases: BTreeMap<WorkspaceAlias, RepositoryId>,
    packages: BTreeMap<PackageId, RepositoryId>,
}

impl WorkspaceCatalog {
    /// Creates an empty independently configured catalog.
    #[must_use]
    pub const fn new(id: WorkspaceId, limits: CatalogLimits) -> Self {
        Self {
            id,
            limits,
            repositories: BTreeMap::new(),
            roots: BTreeMap::new(),
            aliases: BTreeMap::new(),
            packages: BTreeMap::new(),
        }
    }

    /// Returns the workspace identity.
    #[must_use]
    pub const fn id(&self) -> WorkspaceId {
        self.id
    }

    /// Returns the catalog limits.
    #[must_use]
    pub const fn limits(&self) -> CatalogLimits {
        self.limits
    }

    /// Registers one repository and its topology atomically.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for collisions, missing topology owners, limit
    /// violations, inconsistent worktree content, or cancellation.
    pub fn register(
        &mut self,
        mut descriptor: RepositoryDescriptor,
        cancellation: &Cancellation,
    ) -> Result<(), CatalogError> {
        cancellation.check()?;
        if self.repositories.len() >= self.limits.max_repositories {
            return Err(CatalogError::RepositoryLimit);
        }
        if self.repositories.contains_key(&descriptor.repository) {
            return Err(CatalogError::DuplicateRepository);
        }
        if self.roots.contains_key(&descriptor.root) {
            return Err(CatalogError::RootCollision);
        }
        self.validate_topology(&descriptor)?;
        descriptor.aliases.sort();
        descriptor.packages.sort_by_key(|package| package.id);
        if descriptor.aliases.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(CatalogError::DuplicateAlias);
        }
        if descriptor
            .packages
            .windows(2)
            .any(|pair| pair[0].id == pair[1].id)
        {
            return Err(CatalogError::DuplicatePackage);
        }
        if self
            .aliases
            .keys()
            .any(|alias| descriptor.aliases.binary_search(alias).is_ok())
        {
            return Err(CatalogError::DuplicateAlias);
        }
        if self.packages.keys().any(|package| {
            descriptor
                .packages
                .binary_search_by_key(package, |item| item.id)
                .is_ok()
        }) {
            return Err(CatalogError::DuplicatePackage);
        }
        if self.aliases.len().saturating_add(descriptor.aliases.len()) > self.limits.max_aliases {
            return Err(CatalogError::AliasLimit);
        }
        if self
            .packages
            .len()
            .saturating_add(descriptor.packages.len())
            > self.limits.max_packages
        {
            return Err(CatalogError::PackageLimit);
        }
        cancellation.check()?;
        let repository = descriptor.repository;
        self.roots.insert(descriptor.root, repository);
        for alias in &descriptor.aliases {
            self.aliases.insert(alias.clone(), repository);
        }
        for package in &descriptor.packages {
            self.packages.insert(package.id, repository);
        }
        self.repositories.insert(
            repository,
            CatalogRepository {
                repository,
                root: descriptor.root,
                shared_content: descriptor.shared_content,
                topology: descriptor.topology,
                aliases: descriptor.aliases,
                packages: descriptor.packages,
                state: RepositoryState::Unavailable,
                authorized: true,
                current_generation: None,
                retained_generations: BTreeSet::new(),
            },
        );
        Ok(())
    }

    /// Publishes one generation without mutating any other repository.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for an unknown or deleted repository, a
    /// retention-limit violation, or cancellation.
    pub fn publish_generation(
        &mut self,
        repository: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<(), CatalogError> {
        cancellation.check()?;
        let entry = self
            .repositories
            .get_mut(&repository)
            .ok_or(CatalogError::UnknownRepository)?;
        if entry.state == RepositoryState::Deleted {
            return Err(CatalogError::DeletedRepository);
        }
        if !entry.retained_generations.contains(&generation)
            && entry.retained_generations.len()
                >= self.limits.max_retained_generations_per_repository
        {
            return Err(CatalogError::GenerationLimit);
        }
        entry.retained_generations.insert(generation);
        entry.current_generation = Some(generation);
        entry.state = RepositoryState::Ready;
        Ok(())
    }

    /// Reclaims a noncurrent generation from one repository only.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] when the repository or generation is unknown,
    /// the generation is current, or cancellation wins.
    pub fn reclaim_generation(
        &mut self,
        repository: RepositoryId,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<(), CatalogError> {
        cancellation.check()?;
        let entry = self
            .repositories
            .get_mut(&repository)
            .ok_or(CatalogError::UnknownRepository)?;
        if entry.current_generation == Some(generation) {
            return Err(CatalogError::CurrentGeneration);
        }
        if !entry.retained_generations.remove(&generation) {
            return Err(CatalogError::UnknownGeneration);
        }
        Ok(())
    }

    /// Changes availability without altering retained immutable generations.
    ///
    /// `Ready` must be established through [`Self::publish_generation`], and a
    /// deleted registration is terminal.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError`] for an invalid transition or unknown repository.
    pub fn set_state(
        &mut self,
        repository: RepositoryId,
        state: RepositoryState,
    ) -> Result<(), CatalogError> {
        let entry = self
            .repositories
            .get_mut(&repository)
            .ok_or(CatalogError::UnknownRepository)?;
        if state == RepositoryState::Ready {
            return Err(CatalogError::InvalidStateTransition);
        }
        if entry.state == RepositoryState::Deleted && state != RepositoryState::Deleted {
            return Err(CatalogError::DeletedRepository);
        }
        entry.state = state;
        if state == RepositoryState::Deleted {
            entry.current_generation = None;
            entry.retained_generations.clear();
        }
        Ok(())
    }

    /// Changes source-access authorization without affecting other entries.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::UnknownRepository`] when the identity is absent.
    pub fn set_authorized(
        &mut self,
        repository: RepositoryId,
        authorized: bool,
    ) -> Result<(), CatalogError> {
        let entry = self
            .repositories
            .get_mut(&repository)
            .ok_or(CatalogError::UnknownRepository)?;
        entry.authorized = authorized;
        Ok(())
    }

    /// Finds a repository by stable identity.
    #[must_use]
    pub fn repository(&self, repository: RepositoryId) -> Option<&CatalogRepository> {
        self.repositories.get(&repository)
    }

    /// Resolves a presentation alias without changing repository identity.
    #[must_use]
    pub fn repository_for_alias(&self, alias: &WorkspaceAlias) -> Option<RepositoryId> {
        self.aliases.get(alias).copied()
    }

    /// Returns repositories in stable identity order.
    pub fn repositories(&self) -> impl ExactSizeIterator<Item = &CatalogRepository> {
        self.repositories.values()
    }

    fn validate_topology(&self, descriptor: &RepositoryDescriptor) -> Result<(), CatalogError> {
        let owner = match descriptor.topology {
            RepositoryTopology::Standalone | RepositoryTopology::Monorepo => return Ok(()),
            RepositoryTopology::Nested(owner)
            | RepositoryTopology::Worktree(owner)
            | RepositoryTopology::Submodule(owner) => owner,
        };
        if owner == descriptor.repository {
            return Err(CatalogError::TopologyCycle);
        }
        let owner = self
            .repositories
            .get(&owner)
            .ok_or(CatalogError::MissingTopologyOwner)?;
        if owner.state == RepositoryState::Deleted {
            return Err(CatalogError::MissingTopologyOwner);
        }
        if matches!(descriptor.topology, RepositoryTopology::Worktree(_))
            && owner.shared_content != descriptor.shared_content
        {
            return Err(CatalogError::WorktreeContentMismatch);
        }
        Ok(())
    }
}

/// Invalid catalog input or isolated repository state transition.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// Limit policy is zero or broader than a hard ceiling.
    #[error("workspace catalog limits are invalid")]
    InvalidLimits,
    /// Alias violates the canonical public grammar.
    #[error("workspace alias is invalid")]
    InvalidAlias,
    /// Repository capacity is exhausted.
    #[error("workspace repository limit exceeded")]
    RepositoryLimit,
    /// Alias capacity is exhausted.
    #[error("workspace alias limit exceeded")]
    AliasLimit,
    /// Package capacity is exhausted.
    #[error("workspace package limit exceeded")]
    PackageLimit,
    /// Per-repository generation retention is exhausted.
    #[error("workspace generation retention limit exceeded")]
    GenerationLimit,
    /// Repository identity already exists.
    #[error("workspace repository identity is already registered")]
    DuplicateRepository,
    /// Canonical root identity belongs to another repository.
    #[error("workspace repository root identity collides")]
    RootCollision,
    /// Alias already exists in the catalog or descriptor.
    #[error("workspace alias is duplicated")]
    DuplicateAlias,
    /// Package identity already exists in the catalog or descriptor.
    #[error("workspace package identity is duplicated")]
    DuplicatePackage,
    /// Topology owner is not registered and active.
    #[error("workspace topology owner is unavailable")]
    MissingTopologyOwner,
    /// Repository directly owns itself.
    #[error("workspace topology contains a direct cycle")]
    TopologyCycle,
    /// Worktree does not share its primary checkout's immutable content.
    #[error("workspace worktree content identity differs from its primary")]
    WorktreeContentMismatch,
    /// Repository identity is absent.
    #[error("workspace repository is unknown")]
    UnknownRepository,
    /// Generation is absent from one repository.
    #[error("workspace generation is unknown")]
    UnknownGeneration,
    /// Current generation cannot be reclaimed.
    #[error("workspace current generation cannot be reclaimed")]
    CurrentGeneration,
    /// Deleted registration cannot be reactivated.
    #[error("workspace repository is deleted")]
    DeletedRepository,
    /// Ready state requires an exact published generation.
    #[error("workspace repository state transition is invalid")]
    InvalidStateTransition,
    /// Cooperative cancellation won.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
}
