//! Versioned typed inputs, scoped fact domains, artifacts, and resource limits.
//!
//! These types make complete dependency fingerprints explicit so reuse cannot
//! silently omit build context, provider versions, or configuration.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    str::FromStr,
};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId};
use serde::Serialize;

use crate::{IncrementalError, ResourceKind};

/// Maximum supported pass identifier byte length.
pub(crate) const MAX_PASS_ID_BYTES: usize = 64;
/// Hard ceiling for typed generation inputs.
pub(crate) const HARD_MAX_INPUTS: usize = 8_000_000;
/// Hard ceiling for reusable artifacts in one parent generation.
pub(crate) const HARD_MAX_ARTIFACTS: usize = 2_000_000;
/// Hard ceiling for declared passes.
pub(crate) const HARD_MAX_PASSES: usize = 16_384;
/// Hard ceiling for scoped dependency nodes.
pub(crate) const HARD_MAX_DEPENDENCY_NODES: usize = 8_000_000;
/// Hard ceiling for typed dependency edges.
pub(crate) const HARD_MAX_DEPENDENCY_EDGES: usize = 32_000_000;
/// Hard ceiling for fixed-point edge visits.
pub(crate) const HARD_MAX_CLOSURE_WORK: usize = 32_000_000;
/// Hard ceiling for source-free trace entries.
pub(crate) const HARD_MAX_TRACE_ENTRIES: usize = 16_000_000;

/// Stable identity of one language or repository analysis unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct AnalysisUnitId(FactId);

impl AnalysisUnitId {
    /// Creates an analysis-unit identity from a domain-separated fact ID.
    #[must_use]
    pub const fn new(id: FactId) -> Self {
        Self(id)
    }

    /// Returns the underlying stable fact identity.
    #[must_use]
    pub const fn as_fact_id(self) -> FactId {
        self.0
    }
}

/// Stable identity of one reusable immutable artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ArtifactId(FactId);

impl ArtifactId {
    /// Creates an artifact identity from a domain-separated fact ID.
    #[must_use]
    pub const fn new(id: FactId) -> Self {
        Self(id)
    }

    /// Returns the underlying stable fact identity.
    #[must_use]
    pub const fn as_fact_id(self) -> FactId {
        self.0
    }
}

/// A bounded stable identifier for one analysis pass or invalidation rule.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PassId(String);

impl PassId {
    /// Parses a source-free lower-case identifier.
    ///
    /// Valid identifiers contain 1 to 64 ASCII lower-case letters, digits,
    /// dots, hyphens, or underscores.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::InvalidPassId`] for any noncanonical value.
    pub fn parse(value: &str) -> Result<Self, IncrementalError> {
        if value.is_empty()
            || value.len() > MAX_PASS_ID_BYTES
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'-' | b'_')
            })
        {
            return Err(IncrementalError::InvalidPassId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the canonical identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PassId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PassId {
    type Err = IncrementalError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// A declared class of base or derived semantic facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FactDomain {
    /// Parsed syntax and syntax diagnostics.
    Syntax,
    /// Exported declarations, signatures, visibility, and imports.
    PublicSurface,
    /// Local implementation bodies and body-dependent occurrences.
    Body,
    /// Import, type, call, hierarchy, and candidate resolution.
    Resolution,
    /// Generation-aligned lexical and optional derived search facts.
    Search,
    /// Bounded derived graph projections and intent-plan aids.
    DerivedGraph,
    /// Test identities and explicit test relationships.
    Tests,
    /// Routes, RPC, messaging, database, and foreign service links.
    Services,
    /// Bounded Git, lineage, ownership, and co-change facts.
    History,
}

impl FactDomain {
    pub(crate) const ALL: [Self; 9] = [
        Self::Syntax,
        Self::PublicSurface,
        Self::Body,
        Self::Resolution,
        Self::Search,
        Self::DerivedGraph,
        Self::Tests,
        Self::Services,
        Self::History,
    ];
}

/// A deterministically ordered set of fact domains.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(transparent)]
pub struct FactDomainSet(BTreeSet<FactDomain>);

impl FactDomainSet {
    /// Creates a canonical domain set from any input order.
    #[must_use]
    pub fn new(domains: impl IntoIterator<Item = FactDomain>) -> Self {
        Self(domains.into_iter().collect())
    }

    /// Returns every currently versioned fact domain.
    #[must_use]
    pub fn all() -> Self {
        Self::new(FactDomain::ALL)
    }

    /// Reports whether the set contains no domains.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Reports whether the set contains a domain.
    #[must_use]
    pub fn contains(&self, domain: FactDomain) -> bool {
        self.0.contains(&domain)
    }

    /// Returns domains in canonical enum order.
    pub fn iter(&self) -> impl Iterator<Item = FactDomain> + '_ {
        self.0.iter().copied()
    }

    /// Reports whether this set overlaps another set.
    #[must_use]
    pub fn intersects(&self, other: &Self) -> bool {
        self.0.iter().any(|domain| other.0.contains(domain))
    }

    pub(crate) fn insert(&mut self, domain: FactDomain) {
        self.0.insert(domain);
    }
}

/// A scoped fact-domain node in the invalidation graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FactNode {
    unit: AnalysisUnitId,
    domain: FactDomain,
}

impl FactNode {
    /// Creates a fact node for one analysis unit and domain.
    #[must_use]
    pub const fn new(unit: AnalysisUnitId, domain: FactDomain) -> Self {
        Self { unit, domain }
    }

    /// Returns the owning analysis unit.
    #[must_use]
    pub const fn unit(self) -> AnalysisUnitId {
        self.unit
    }

    /// Returns the fact domain.
    #[must_use]
    pub const fn domain(self) -> FactDomain {
        self.domain
    }
}

/// The closed type family for versioned incremental inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// Actual bytes of one stable file input.
    FileContent,
    /// Canonical path and containment semantics for one file.
    FilePath,
    /// Exported declaration and signature summary.
    PublicSurface,
    /// Local implementation-body summary.
    BodySummary,
    /// Normalized import or export clause set.
    ImportSet,
    /// Build-target identity and membership.
    BuildTarget,
    /// Compiler, macro, and language option context.
    CompilerOptions,
    /// One dependency or lockfile resolution.
    DependencyVersion,
    /// Parser grammar version and configuration.
    GrammarVersion,
    /// Adapter producer binary and policy version.
    AdapterVersion,
    /// Resolver algorithm and calibration version.
    ResolverVersion,
    /// Rootlight analysis configuration revision.
    ConfigurationRevision,
    /// Search schema or tokenizer revision.
    SearchRevision,
    /// One derived plan or projection revision.
    DerivedPlan,
}

impl InputKind {
    fn changed_class(self) -> ChangeClass {
        match self {
            Self::FileContent | Self::PublicSurface | Self::ImportSet => ChangeClass::Surface,
            Self::FilePath => ChangeClass::Move,
            Self::BodySummary => ChangeClass::BodyOnly,
            Self::BuildTarget | Self::CompilerOptions | Self::DependencyVersion => {
                ChangeClass::BuildContext
            }
            Self::GrammarVersion | Self::AdapterVersion | Self::ResolverVersion => {
                ChangeClass::ProviderChange
            }
            Self::ConfigurationRevision | Self::SearchRevision | Self::DerivedPlan => {
                ChangeClass::Configuration
            }
        }
    }
}

/// One typed dependency key whose value is stored separately as a hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(tag = "kind", content = "subject", rename_all = "snake_case")]
pub enum InputKey {
    /// Actual bytes of one file.
    FileContent(FileId),
    /// Canonical path semantics of one file.
    FilePath(FileId),
    /// Exported surface of one analysis unit.
    PublicSurface(AnalysisUnitId),
    /// Body summary of one analysis unit.
    BodySummary(AnalysisUnitId),
    /// Import set of one analysis unit.
    ImportSet(AnalysisUnitId),
    /// Build-target membership identified by a stable fact ID.
    BuildTarget(FactId),
    /// Compiler and macro options identified by a stable fact ID.
    CompilerOptions(FactId),
    /// Dependency identity identified by a stable fact ID.
    DependencyVersion(FactId),
    /// Grammar identity identified by a stable fact ID.
    GrammarVersion(FactId),
    /// Adapter identity identified by a stable fact ID.
    AdapterVersion(FactId),
    /// Global resolver revision for this generation.
    ResolverVersion,
    /// Global analysis-configuration revision for this generation.
    ConfigurationRevision,
    /// Global search revision for this generation.
    SearchRevision,
    /// Derived plan or projection identity.
    DerivedPlan(FactId),
}

impl InputKey {
    /// Returns the closed input kind without its scoped subject.
    #[must_use]
    pub const fn kind(self) -> InputKind {
        match self {
            Self::FileContent(_) => InputKind::FileContent,
            Self::FilePath(_) => InputKind::FilePath,
            Self::PublicSurface(_) => InputKind::PublicSurface,
            Self::BodySummary(_) => InputKind::BodySummary,
            Self::ImportSet(_) => InputKind::ImportSet,
            Self::BuildTarget(_) => InputKind::BuildTarget,
            Self::CompilerOptions(_) => InputKind::CompilerOptions,
            Self::DependencyVersion(_) => InputKind::DependencyVersion,
            Self::GrammarVersion(_) => InputKind::GrammarVersion,
            Self::AdapterVersion(_) => InputKind::AdapterVersion,
            Self::ResolverVersion => InputKind::ResolverVersion,
            Self::ConfigurationRevision => InputKind::ConfigurationRevision,
            Self::SearchRevision => InputKind::SearchRevision,
            Self::DerivedPlan(_) => InputKind::DerivedPlan,
        }
    }
}

/// One typed input and its complete canonical value digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputFingerprint {
    key: InputKey,
    value: ContentHash,
}

impl InputFingerprint {
    /// Creates one input fingerprint.
    #[must_use]
    pub const fn new(key: InputKey, value: ContentHash) -> Self {
        Self { key, value }
    }

    /// Returns the typed input key.
    #[must_use]
    pub const fn key(self) -> InputKey {
        self.key
    }

    /// Returns the canonical value digest.
    #[must_use]
    pub const fn value(self) -> ContentHash {
        self.value
    }
}

/// Resource limits for dependency declarations and graph construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GraphLimits {
    pub(crate) max_passes: usize,
    pub(crate) max_nodes: usize,
    pub(crate) max_edges: usize,
}

impl GraphLimits {
    /// Creates validated dependency graph limits.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::InvalidLimit`] for zero values or values
    /// above a hard safety ceiling.
    pub fn new(
        max_passes: usize,
        max_nodes: usize,
        max_edges: usize,
    ) -> Result<Self, IncrementalError> {
        validate_limit(ResourceKind::Passes, max_passes, HARD_MAX_PASSES)?;
        validate_limit(
            ResourceKind::DependencyNodes,
            max_nodes,
            HARD_MAX_DEPENDENCY_NODES,
        )?;
        validate_limit(
            ResourceKind::DependencyEdges,
            max_edges,
            HARD_MAX_DEPENDENCY_EDGES,
        )?;
        Ok(Self {
            max_passes,
            max_nodes,
            max_edges,
        })
    }
}

impl Default for GraphLimits {
    fn default() -> Self {
        Self {
            max_passes: 1_024,
            max_nodes: 1_000_000,
            max_edges: 4_000_000,
        }
    }
}

/// Resource limits for input comparison, artifact reuse, closure, and traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanningLimits {
    pub(crate) max_inputs: usize,
    pub(crate) max_artifacts: usize,
    pub(crate) max_closure_work: usize,
    pub(crate) max_trace_entries: usize,
}

impl PlanningLimits {
    /// Creates validated planning limits.
    ///
    /// # Errors
    ///
    /// Returns [`IncrementalError::InvalidLimit`] for zero values or values
    /// above a hard safety ceiling.
    pub fn new(
        max_inputs: usize,
        max_artifacts: usize,
        max_closure_work: usize,
        max_trace_entries: usize,
    ) -> Result<Self, IncrementalError> {
        validate_limit(ResourceKind::Inputs, max_inputs, HARD_MAX_INPUTS)?;
        validate_limit(ResourceKind::Artifacts, max_artifacts, HARD_MAX_ARTIFACTS)?;
        validate_limit(
            ResourceKind::ClosureWork,
            max_closure_work,
            HARD_MAX_CLOSURE_WORK,
        )?;
        validate_limit(
            ResourceKind::TraceEntries,
            max_trace_entries,
            HARD_MAX_TRACE_ENTRIES,
        )?;
        Ok(Self {
            max_inputs,
            max_artifacts,
            max_closure_work,
            max_trace_entries,
        })
    }
}

impl Default for PlanningLimits {
    fn default() -> Self {
        Self {
            max_inputs: 1_000_000,
            max_artifacts: 100_000,
            max_closure_work: 4_000_000,
            max_trace_entries: 2_000_000,
        }
    }
}

pub(crate) fn validate_limit(
    resource: ResourceKind,
    value: usize,
    hard_maximum: usize,
) -> Result<(), IncrementalError> {
    if value == 0 || value > hard_maximum {
        return Err(IncrementalError::InvalidLimit {
            resource,
            value,
            hard_maximum,
        });
    }
    Ok(())
}

pub(crate) fn check_count(
    resource: ResourceKind,
    observed: usize,
    limit: usize,
) -> Result<(), IncrementalError> {
    if observed > limit {
        return Err(IncrementalError::ResourceLimit {
            resource,
            observed,
            limit,
        });
    }
    Ok(())
}

/// A canonical complete set of generation input values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSnapshot {
    entries: BTreeMap<InputKey, ContentHash>,
}

impl InputSnapshot {
    /// Canonicalizes and validates generation inputs.
    ///
    /// # Errors
    ///
    /// Returns a duplicate, limit, or cancellation error.
    pub fn new(
        entries: impl IntoIterator<Item = InputFingerprint>,
        limits: PlanningLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical = BTreeMap::new();
        for entry in entries {
            cancellation.check()?;
            if canonical.insert(entry.key(), entry.value()).is_some() {
                return Err(IncrementalError::DuplicateInput { key: entry.key() });
            }
            check_count(ResourceKind::Inputs, canonical.len(), limits.max_inputs)?;
        }
        Ok(Self { entries: canonical })
    }

    /// Returns a value digest for one typed input.
    #[must_use]
    pub fn value(&self, key: InputKey) -> Option<ContentHash> {
        self.entries.get(&key).copied()
    }

    /// Returns the number of typed inputs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether no typed inputs exist.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns inputs in canonical key order.
    pub fn iter(&self) -> impl Iterator<Item = InputFingerprint> + '_ {
        self.entries
            .iter()
            .map(|(key, value)| InputFingerprint::new(*key, *value))
    }

    /// Derives canonical typed transitions to another complete input snapshot.
    ///
    /// # Errors
    ///
    /// Returns a resource-limit or cancellation error.
    pub fn changes_to(
        &self,
        current: &Self,
        limits: PlanningLimits,
        cancellation: &Cancellation,
    ) -> Result<ChangeSet, IncrementalError> {
        let mut keys = BTreeSet::new();
        keys.extend(self.entries.keys().copied());
        keys.extend(current.entries.keys().copied());
        check_count(ResourceKind::Inputs, keys.len(), limits.max_inputs)?;

        let mut changes = Vec::new();
        for key in keys {
            cancellation.check()?;
            let before = self.value(key);
            let after = current.value(key);
            if before == after {
                continue;
            }
            let class = match (before, after) {
                (None, Some(_)) => ChangeClass::Added,
                (Some(_), None) => ChangeClass::Delete,
                (Some(_), Some(_)) => key.kind().changed_class(),
                (None, None) => continue,
            };
            changes.push(InputDelta {
                key,
                before,
                after,
                class,
            });
        }
        Ok(ChangeSet { changes })
    }
}

/// Semantic classification of one stable input transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeClass {
    /// No complete input value changed.
    NoChange,
    /// A stable input was newly added.
    Added,
    /// Only an implementation-body summary changed.
    BodyOnly,
    /// A declaration, import, route, test, or exported surface may have changed.
    Surface,
    /// Build targets, compiler options, or dependency context changed.
    BuildContext,
    /// Canonical path or containment semantics changed.
    Move,
    /// A stable input was removed.
    Delete,
    /// Rootlight analysis or derived-index configuration changed.
    Configuration,
    /// Grammar, adapter, or resolver producer version changed.
    ProviderChange,
}

/// One canonical typed input transition between generation snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InputDelta {
    key: InputKey,
    before: Option<ContentHash>,
    after: Option<ContentHash>,
    class: ChangeClass,
}

impl InputDelta {
    /// Returns the typed changed input.
    #[must_use]
    pub const fn key(self) -> InputKey {
        self.key
    }

    /// Returns the parent value, if the input existed.
    #[must_use]
    pub const fn before(self) -> Option<ContentHash> {
        self.before
    }

    /// Returns the current value, if the input still exists.
    #[must_use]
    pub const fn after(self) -> Option<ContentHash> {
        self.after
    }

    /// Returns the conservative semantic change class.
    #[must_use]
    pub const fn class(self) -> ChangeClass {
        self.class
    }
}

/// Canonical changed inputs derived from complete parent and current snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ChangeSet {
    changes: Vec<InputDelta>,
}

impl ChangeSet {
    /// Reports whether the two complete input snapshots are equal.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Returns changed inputs in canonical key order.
    #[must_use]
    pub fn changes(&self) -> &[InputDelta] {
        &self.changes
    }
}

/// One reusable artifact and the complete inputs and outputs that govern reuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactSummary {
    id: ArtifactId,
    outputs: BTreeSet<FactNode>,
    dependencies: BTreeMap<InputKey, ContentHash>,
}

impl ArtifactSummary {
    /// Canonicalizes one artifact's output and dependency contract.
    ///
    /// # Errors
    ///
    /// Returns an empty-part, duplicate, resource-limit, or cancellation error.
    pub fn new(
        id: ArtifactId,
        outputs: impl IntoIterator<Item = FactNode>,
        dependencies: impl IntoIterator<Item = InputFingerprint>,
        limits: PlanningLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical_outputs = BTreeSet::new();
        for node in outputs {
            cancellation.check()?;
            if !canonical_outputs.insert(node) {
                return Err(IncrementalError::DuplicateArtifactOutput { artifact: id, node });
            }
        }
        if canonical_outputs.is_empty() {
            return Err(IncrementalError::EmptyArtifactPart {
                artifact: id,
                part: "outputs",
            });
        }

        let mut canonical_dependencies = BTreeMap::new();
        for dependency in dependencies {
            cancellation.check()?;
            if canonical_dependencies
                .insert(dependency.key(), dependency.value())
                .is_some()
            {
                return Err(IncrementalError::DuplicateArtifactDependency {
                    artifact: id,
                    key: dependency.key(),
                });
            }
            check_count(
                ResourceKind::Inputs,
                canonical_dependencies.len(),
                limits.max_inputs,
            )?;
        }
        if canonical_dependencies.is_empty() {
            return Err(IncrementalError::EmptyArtifactPart {
                artifact: id,
                part: "dependencies",
            });
        }

        Ok(Self {
            id,
            outputs: canonical_outputs,
            dependencies: canonical_dependencies,
        })
    }

    /// Returns the artifact identity.
    #[must_use]
    pub const fn id(&self) -> ArtifactId {
        self.id
    }

    /// Returns produced fact nodes in canonical order.
    pub fn outputs(&self) -> impl Iterator<Item = FactNode> + '_ {
        self.outputs.iter().copied()
    }

    /// Returns complete dependency fingerprints in canonical key order.
    pub fn dependencies(&self) -> impl Iterator<Item = InputFingerprint> + '_ {
        self.dependencies
            .iter()
            .map(|(key, value)| InputFingerprint::new(*key, *value))
    }
}

/// Parent-generation inputs and reusable artifact summaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationSummary {
    inputs: InputSnapshot,
    artifacts: BTreeMap<ArtifactId, ArtifactSummary>,
}

impl GenerationSummary {
    /// Validates a complete parent generation summary.
    ///
    /// Every artifact dependency must match an input value in this summary.
    ///
    /// # Errors
    ///
    /// Returns a duplicate, mismatch, limit, or cancellation error.
    pub fn new(
        inputs: InputSnapshot,
        artifacts: impl IntoIterator<Item = ArtifactSummary>,
        limits: PlanningLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, IncrementalError> {
        let mut canonical_artifacts = BTreeMap::new();
        for artifact in artifacts {
            cancellation.check()?;
            for dependency in artifact.dependencies() {
                if inputs.value(dependency.key()) != Some(dependency.value()) {
                    return Err(IncrementalError::ArtifactDependencyMismatch {
                        artifact: artifact.id(),
                        key: dependency.key(),
                    });
                }
            }
            let id = artifact.id();
            if canonical_artifacts.insert(id, artifact).is_some() {
                return Err(IncrementalError::DuplicateArtifact { artifact: id });
            }
            check_count(
                ResourceKind::Artifacts,
                canonical_artifacts.len(),
                limits.max_artifacts,
            )?;
        }
        Ok(Self {
            inputs,
            artifacts: canonical_artifacts,
        })
    }

    /// Returns the complete parent input snapshot.
    #[must_use]
    pub const fn inputs(&self) -> &InputSnapshot {
        &self.inputs
    }

    /// Returns reusable artifacts in canonical identity order.
    pub fn artifacts(&self) -> impl Iterator<Item = &ArtifactSummary> {
        self.artifacts.values()
    }
}
