//! Side-by-side generation migration planning and activation control.
//!
//! Migration steps are bounded and immutable; the previous generation remains
//! the rollback target until lifecycle retention explicitly reclaims it.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{ContentHash, GenerationId, content_hash};
use serde::{Deserialize, Serialize};

use crate::{GenerationContractVersion, PublicationSpaceEstimate};

/// Hard ceiling for one supported forward migration path.
pub const HARD_MAX_MIGRATION_STEPS: u16 = 64;
/// Hard ceiling for one canonical migration journal.
pub const HARD_MAX_MIGRATION_JOURNAL_BYTES: u16 = 8 * 1024;
/// Current canonical migration journal schema.
pub const MIGRATION_JOURNAL_SCHEMA: &str = "rootlight.generation-migration/1";

const DEFAULT_MAX_MIGRATION_STEPS: u16 = 16;

/// Physical strategy for one forward-compatible format transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MigrationStepKind {
    /// Rebuilds a complete immutable generation beside the source.
    Rebuild,
    /// Rewrites bounded metadata into a new immutable generation.
    MetadataRewrite,
}

/// One reviewed adjacent-minor migration transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationStep {
    from: GenerationContractVersion,
    to: GenerationContractVersion,
    kind: MigrationStepKind,
    implementation_hash: ContentHash,
    output_bytes: u64,
    temporary_bytes: u64,
}

impl MigrationStep {
    /// Creates one same-major, adjacent-minor, side-by-side transition.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::InvalidStep`] unless `to.minor` is exactly one
    /// greater than `from.minor`, the major is unchanged, and output bytes are
    /// non-zero.
    pub fn new(
        from: GenerationContractVersion,
        to: GenerationContractVersion,
        kind: MigrationStepKind,
        implementation_hash: ContentHash,
        output_bytes: u64,
        temporary_bytes: u64,
    ) -> Result<Self, MigrationError> {
        if from.major() != to.major()
            || from
                .minor()
                .checked_add(1)
                .is_none_or(|minor| minor != to.minor())
            || output_bytes == 0
        {
            return Err(MigrationError::InvalidStep);
        }
        PublicationSpaceEstimate::new(output_bytes, temporary_bytes, 0)
            .map_err(|_| MigrationError::InvalidStep)?;
        Ok(Self {
            from,
            to,
            kind,
            implementation_hash,
            output_bytes,
            temporary_bytes,
        })
    }

    /// Returns the source contract version.
    #[must_use]
    pub const fn from(self) -> GenerationContractVersion {
        self.from
    }

    /// Returns the target contract version.
    #[must_use]
    pub const fn to(self) -> GenerationContractVersion {
        self.to
    }

    /// Returns the physical migration strategy.
    #[must_use]
    pub const fn kind(self) -> MigrationStepKind {
        self.kind
    }

    /// Returns the reviewed implementation identity.
    #[must_use]
    pub const fn implementation_hash(self) -> ContentHash {
        self.implementation_hash
    }

    /// Returns estimated completed target bytes.
    #[must_use]
    pub const fn output_bytes(self) -> u64 {
        self.output_bytes
    }

    /// Returns estimated peak temporary bytes.
    #[must_use]
    pub const fn temporary_bytes(self) -> u64 {
        self.temporary_bytes
    }
}

/// Validated migration path-length ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationLimits {
    max_steps: u16,
}

impl MigrationLimits {
    /// Creates a positive limit within [`HARD_MAX_MIGRATION_STEPS`].
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::InvalidLimits`] for zero or excessive limits.
    pub const fn new(max_steps: u16) -> Result<Self, MigrationError> {
        if max_steps == 0 || max_steps > HARD_MAX_MIGRATION_STEPS {
            return Err(MigrationError::InvalidLimits);
        }
        Ok(Self { max_steps })
    }

    /// Returns the maximum path length.
    #[must_use]
    pub const fn max_steps(self) -> u16 {
        self.max_steps
    }
}

impl Default for MigrationLimits {
    fn default() -> Self {
        Self {
            max_steps: DEFAULT_MAX_MIGRATION_STEPS,
        }
    }
}

/// Closed registry of reviewed transitions leading to one current contract.
#[derive(Debug, Clone)]
pub struct MigrationRegistry {
    current: GenerationContractVersion,
    limits: MigrationLimits,
    steps: BTreeMap<GenerationContractVersion, MigrationStep>,
}

impl MigrationRegistry {
    /// Creates a deterministic transition registry.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, an oversized registry,
    /// invalid step direction, duplicate source or target versions, or a step
    /// beyond the declared current version.
    pub fn new(
        current: GenerationContractVersion,
        steps: Vec<MigrationStep>,
        limits: MigrationLimits,
        cancellation: &Cancellation,
    ) -> Result<Self, MigrationError> {
        cancellation.check()?;
        if steps.len() > usize::from(limits.max_steps) {
            return Err(MigrationError::StepLimit);
        }
        let mut by_source = BTreeMap::new();
        let mut targets = BTreeSet::new();
        for step in steps {
            cancellation.check()?;
            if step.to > current
                || step.from.major() != current.major()
                || !targets.insert(step.to)
                || by_source.insert(step.from, step).is_some()
            {
                return Err(MigrationError::InvalidRegistry);
            }
        }
        Ok(Self {
            current,
            limits,
            steps: by_source,
        })
    }

    /// Plans a dry-run forward path to the current contract.
    ///
    /// No generation state changes. Callers must pass the returned space
    /// estimate through disk preflight before constructing a session.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, incompatible versions,
    /// missing steps, identity reuse, path-length exhaustion, or invalid space
    /// accounting.
    pub fn plan(
        &self,
        source_version: GenerationContractVersion,
        source_generation: GenerationId,
        target_generation: GenerationId,
        reserve_bytes: u64,
        cancellation: &Cancellation,
    ) -> Result<MigrationPlan, MigrationError> {
        cancellation.check()?;
        if source_version.major() != self.current.major() {
            return Err(MigrationError::UnsupportedMajor);
        }
        if source_version > self.current {
            return Err(MigrationError::UnsupportedFutureVersion);
        }
        if source_version == self.current {
            if target_generation != source_generation {
                return Err(MigrationError::InvalidTargetIdentity);
            }
            return Ok(MigrationPlan {
                source_version,
                target_version: self.current,
                source_generation,
                target_generation,
                steps: Vec::new(),
                space: None,
            });
        }
        if target_generation == source_generation {
            return Err(MigrationError::InvalidTargetIdentity);
        }

        let mut version = source_version;
        let mut path = Vec::new();
        let mut peak_temporary_bytes = 0_u64;
        while version != self.current {
            cancellation.check()?;
            if path.len() >= usize::from(self.limits.max_steps) {
                return Err(MigrationError::StepLimit);
            }
            let step = self
                .steps
                .get(&version)
                .copied()
                .ok_or(MigrationError::MissingPath)?;
            peak_temporary_bytes = peak_temporary_bytes.max(step.temporary_bytes);
            version = step.to;
            path.push(step);
        }
        let output_bytes = path
            .last()
            .map(|step| step.output_bytes)
            .ok_or(MigrationError::MissingPath)?;
        let space =
            PublicationSpaceEstimate::new(output_bytes, peak_temporary_bytes, reserve_bytes)
                .map_err(|_| MigrationError::InvalidSpaceEstimate)?;
        Ok(MigrationPlan {
            source_version,
            target_version: self.current,
            source_generation,
            target_generation,
            steps: path,
            space: Some(space),
        })
    }

    /// Returns the target generation contract.
    #[must_use]
    pub const fn current(&self) -> GenerationContractVersion {
        self.current
    }
}

/// Immutable dry-run result for one migration request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    source_version: GenerationContractVersion,
    target_version: GenerationContractVersion,
    source_generation: GenerationId,
    target_generation: GenerationId,
    steps: Vec<MigrationStep>,
    space: Option<PublicationSpaceEstimate>,
}

impl MigrationPlan {
    /// Returns the source format version.
    #[must_use]
    pub const fn source_version(&self) -> GenerationContractVersion {
        self.source_version
    }

    /// Returns the current target format version.
    #[must_use]
    pub const fn target_version(&self) -> GenerationContractVersion {
        self.target_version
    }

    /// Returns the last-known-good source generation.
    #[must_use]
    pub const fn source_generation(&self) -> GenerationId {
        self.source_generation
    }

    /// Returns the side-by-side target generation.
    #[must_use]
    pub const fn target_generation(&self) -> GenerationId {
        self.target_generation
    }

    /// Returns reviewed steps in execution order.
    #[must_use]
    pub fn steps(&self) -> &[MigrationStep] {
        &self.steps
    }

    /// Returns whether no migration is needed.
    #[must_use]
    pub fn is_noop(&self) -> bool {
        self.steps.is_empty()
    }

    /// Returns the required disk preflight estimate for a side-by-side path.
    #[must_use]
    pub const fn space_estimate(&self) -> Option<PublicationSpaceEstimate> {
        self.space
    }

    fn canonical_hash(&self) -> Result<ContentHash, MigrationError> {
        let recipe = MigrationPlanRecipe {
            source_version: VersionWire::from(self.source_version),
            target_version: VersionWire::from(self.target_version),
            source_generation: self.source_generation,
            target_generation: self.target_generation,
            steps: &self.steps,
            required_bytes: self.space.map(PublicationSpaceEstimate::required_bytes),
        };
        serde_json::to_vec(&recipe)
            .map(|encoded| content_hash(&encoded))
            .map_err(|_| MigrationError::Encode)
    }
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct MigrationPlanRecipe<'a> {
    source_version: VersionWire,
    target_version: VersionWire,
    source_generation: GenerationId,
    target_generation: GenerationId,
    steps: &'a [MigrationStep],
    required_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionWire {
    major: u16,
    minor: u16,
}

impl From<GenerationContractVersion> for VersionWire {
    fn from(version: GenerationContractVersion) -> Self {
        Self {
            major: version.major(),
            minor: version.minor(),
        }
    }
}

/// Restart-safe next action derived from canonical migration journal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum MigrationResumeAction {
    /// Build the target generation beside the source.
    BuildTarget,
    /// Verify the completed target manifest and backend.
    VerifyTarget,
    /// Atomically activate the verified target.
    ActivateTarget,
    /// Serve the activated target.
    ServeTarget,
    /// Serve the retained source after rollback.
    ServeSource,
}

/// Idempotent side-by-side migration state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSession {
    plan: MigrationPlan,
    state: MigrationState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum MigrationState {
    Planned,
    Built { manifest_hash: ContentHash },
    Verified { manifest_hash: ContentHash },
    Activated { manifest_hash: ContentHash },
    RolledBack { manifest_hash: ContentHash },
}

impl MigrationSession {
    /// Creates a session after dry-run and disk preflight succeed.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError::NoMigrationRequired`] for a no-op plan.
    pub fn new(plan: MigrationPlan) -> Result<Self, MigrationError> {
        if plan.is_noop() {
            return Err(MigrationError::NoMigrationRequired);
        }
        Ok(Self {
            plan,
            state: MigrationState::Planned,
        })
    }

    /// Restores a session only when a canonical journal binds the exact plan.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, oversized, malformed,
    /// unsupported, non-canonical, or plan-mismatched journal bytes.
    pub fn resume(
        plan: MigrationPlan,
        journal_bytes: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Self, MigrationError> {
        cancellation.check()?;
        if journal_bytes.len() > usize::from(HARD_MAX_MIGRATION_JOURNAL_BYTES) {
            return Err(MigrationError::JournalTooLarge);
        }
        let journal: MigrationJournal =
            serde_json::from_slice(journal_bytes).map_err(|_| MigrationError::InvalidJournal)?;
        if journal.schema != MIGRATION_JOURNAL_SCHEMA {
            return Err(MigrationError::UnsupportedJournalSchema);
        }
        if journal.source_version != VersionWire::from(plan.source_version)
            || journal.target_version != VersionWire::from(plan.target_version)
            || journal.source_generation != plan.source_generation
            || journal.target_generation != plan.target_generation
            || journal.plan_hash != plan.canonical_hash()?
        {
            return Err(MigrationError::PlanMismatch);
        }
        let session = Self {
            plan,
            state: journal.state,
        };
        let canonical = session.canonical_journal(cancellation)?;
        if canonical != journal_bytes {
            return Err(MigrationError::NonCanonicalJournal);
        }
        Ok(session)
    }

    /// Records a fully built target generation idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, target identity drift,
    /// manifest drift, or an invalid state transition.
    pub fn record_built(
        &mut self,
        generation: GenerationId,
        manifest_hash: ContentHash,
        cancellation: &Cancellation,
    ) -> Result<(), MigrationError> {
        cancellation.check()?;
        if generation != self.plan.target_generation {
            return Err(MigrationError::InvalidTargetIdentity);
        }
        match self.state {
            MigrationState::Planned => {
                self.state = MigrationState::Built { manifest_hash };
                Ok(())
            }
            MigrationState::Built {
                manifest_hash: existing,
            }
            | MigrationState::Verified {
                manifest_hash: existing,
            }
            | MigrationState::Activated {
                manifest_hash: existing,
            } if existing == manifest_hash => Ok(()),
            MigrationState::Built { .. }
            | MigrationState::Verified { .. }
            | MigrationState::Activated { .. } => Err(MigrationError::ManifestMismatch),
            MigrationState::RolledBack { .. } => Err(MigrationError::InvalidTransition),
        }
    }

    /// Records independent target verification idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, manifest drift, or an
    /// invalid state transition.
    pub fn record_verified(
        &mut self,
        manifest_hash: ContentHash,
        cancellation: &Cancellation,
    ) -> Result<(), MigrationError> {
        cancellation.check()?;
        match self.state {
            MigrationState::Built {
                manifest_hash: existing,
            } if existing == manifest_hash => {
                self.state = MigrationState::Verified { manifest_hash };
                Ok(())
            }
            MigrationState::Verified {
                manifest_hash: existing,
            }
            | MigrationState::Activated {
                manifest_hash: existing,
            } if existing == manifest_hash => Ok(()),
            MigrationState::Built { .. }
            | MigrationState::Verified { .. }
            | MigrationState::Activated { .. } => Err(MigrationError::ManifestMismatch),
            MigrationState::Planned | MigrationState::RolledBack { .. } => {
                Err(MigrationError::InvalidTransition)
            }
        }
    }

    /// Activates the verified target while retaining the exact source rollback.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, active-source drift, or an
    /// invalid state transition.
    pub fn activate(
        &mut self,
        active_generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<MigrationActivation, MigrationError> {
        cancellation.check()?;
        match self.state {
            MigrationState::Verified { manifest_hash } => {
                if active_generation != self.plan.source_generation {
                    return Err(MigrationError::ActiveGenerationMismatch);
                }
                self.state = MigrationState::Activated { manifest_hash };
                Ok(self.activation(manifest_hash))
            }
            MigrationState::Activated { manifest_hash }
                if active_generation == self.plan.target_generation =>
            {
                Ok(self.activation(manifest_hash))
            }
            MigrationState::Activated { .. } => Err(MigrationError::ActiveGenerationMismatch),
            MigrationState::Planned
            | MigrationState::Built { .. }
            | MigrationState::RolledBack { .. } => Err(MigrationError::InvalidTransition),
        }
    }

    /// Rolls an activated session back to its retained source idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, active-target drift, or an
    /// invalid state transition.
    pub fn rollback(
        &mut self,
        active_generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<GenerationId, MigrationError> {
        cancellation.check()?;
        match self.state {
            MigrationState::Activated { manifest_hash } => {
                if active_generation != self.plan.target_generation {
                    return Err(MigrationError::ActiveGenerationMismatch);
                }
                self.state = MigrationState::RolledBack { manifest_hash };
                Ok(self.plan.source_generation)
            }
            MigrationState::RolledBack { .. }
                if active_generation == self.plan.source_generation =>
            {
                Ok(self.plan.source_generation)
            }
            MigrationState::RolledBack { .. } => Err(MigrationError::ActiveGenerationMismatch),
            MigrationState::Planned
            | MigrationState::Built { .. }
            | MigrationState::Verified { .. } => Err(MigrationError::InvalidTransition),
        }
    }

    /// Returns the deterministic restart action for current durable state.
    #[must_use]
    pub const fn resume_action(&self) -> MigrationResumeAction {
        match self.state {
            MigrationState::Planned => MigrationResumeAction::BuildTarget,
            MigrationState::Built { .. } => MigrationResumeAction::VerifyTarget,
            MigrationState::Verified { .. } => MigrationResumeAction::ActivateTarget,
            MigrationState::Activated { .. } => MigrationResumeAction::ServeTarget,
            MigrationState::RolledBack { .. } => MigrationResumeAction::ServeSource,
        }
    }

    /// Returns the immutable dry-run plan.
    #[must_use]
    pub const fn plan(&self) -> &MigrationPlan {
        &self.plan
    }

    /// Encodes bounded deterministic journal bytes for durable recovery.
    ///
    /// # Errors
    ///
    /// Returns [`MigrationError`] for cancellation, plan hashing or encoding
    /// failure, or output above [`HARD_MAX_MIGRATION_JOURNAL_BYTES`].
    pub fn canonical_journal(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Vec<u8>, MigrationError> {
        cancellation.check()?;
        let journal = MigrationJournal {
            schema: MIGRATION_JOURNAL_SCHEMA.to_owned(),
            source_version: VersionWire::from(self.plan.source_version),
            target_version: VersionWire::from(self.plan.target_version),
            source_generation: self.plan.source_generation,
            target_generation: self.plan.target_generation,
            plan_hash: self.plan.canonical_hash()?,
            state: self.state,
        };
        let encoded = serde_json::to_vec(&journal).map_err(|_| MigrationError::Encode)?;
        cancellation.check()?;
        if encoded.len() > usize::from(HARD_MAX_MIGRATION_JOURNAL_BYTES) {
            return Err(MigrationError::JournalTooLarge);
        }
        Ok(encoded)
    }

    fn activation(&self, manifest_hash: ContentHash) -> MigrationActivation {
        MigrationActivation {
            previous: self.plan.source_generation,
            active: self.plan.target_generation,
            manifest_hash,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MigrationJournal {
    schema: String,
    source_version: VersionWire,
    target_version: VersionWire,
    source_generation: GenerationId,
    target_generation: GenerationId,
    plan_hash: ContentHash,
    state: MigrationState,
}

/// Atomic catalog-pointer transition after verified side-by-side migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MigrationActivation {
    previous: GenerationId,
    active: GenerationId,
    manifest_hash: ContentHash,
}

impl MigrationActivation {
    /// Returns the retained rollback generation.
    #[must_use]
    pub const fn previous(self) -> GenerationId {
        self.previous
    }

    /// Returns the newly active generation.
    #[must_use]
    pub const fn active(self) -> GenerationId {
        self.active
    }

    /// Returns the verified target manifest checksum.
    #[must_use]
    pub const fn manifest_hash(self) -> ContentHash {
        self.manifest_hash
    }
}

/// Invalid migration contract input or stopped migration operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// Cooperative cancellation stopped the operation.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// A migration transition was not same-major and adjacent-minor.
    #[error("generation migration step is invalid")]
    InvalidStep,
    /// A migration path-length limit was zero or above its hard ceiling.
    #[error("generation migration limits are invalid")]
    InvalidLimits,
    /// Registered steps were duplicated, inconsistent, or beyond current.
    #[error("generation migration registry is invalid")]
    InvalidRegistry,
    /// A registry or planned path exceeded its configured step limit.
    #[error("generation migration step limit is exceeded")]
    StepLimit,
    /// The source major version requires an unsupported rebuild or binary.
    #[error("generation migration source major is unsupported")]
    UnsupportedMajor,
    /// The source version is newer than this binary.
    #[error("generation migration source version is newer than this binary")]
    UnsupportedFutureVersion,
    /// No complete reviewed path reaches the current contract.
    #[error("generation migration path is incomplete")]
    MissingPath,
    /// Side-by-side migration reused or changed the expected target identity.
    #[error("generation migration target identity is invalid")]
    InvalidTargetIdentity,
    /// Side-by-side disk accounting was invalid.
    #[error("generation migration space estimate is invalid")]
    InvalidSpaceEstimate,
    /// A session was requested for an already-current generation.
    #[error("generation migration is not required")]
    NoMigrationRequired,
    /// Session state does not permit the requested transition.
    #[error("generation migration state transition is invalid")]
    InvalidTransition,
    /// Repeated evidence named a different target manifest.
    #[error("generation migration manifest evidence differs")]
    ManifestMismatch,
    /// Catalog active generation differed from the required source or target.
    #[error("generation migration active generation differs")]
    ActiveGenerationMismatch,
    /// Migration journal bytes were malformed or contained an unknown field.
    #[error("generation migration journal is invalid")]
    InvalidJournal,
    /// Migration journal schema is not supported by this binary.
    #[error("generation migration journal schema is unsupported")]
    UnsupportedJournalSchema,
    /// Migration journal bytes were valid but not canonical.
    #[error("generation migration journal is not canonical")]
    NonCanonicalJournal,
    /// Migration journal bytes exceeded their hard ceiling.
    #[error("generation migration journal exceeds the hard limit")]
    JournalTooLarge,
    /// Migration journal was created for a different immutable plan.
    #[error("generation migration journal does not match its plan")]
    PlanMismatch,
    /// Canonical migration encoding failed.
    #[error("generation migration encoding failed")]
    Encode,
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::content_hash;

    use super::*;

    fn version(minor: u16) -> GenerationContractVersion {
        GenerationContractVersion::new(1, minor)
    }

    fn generation(byte: u8) -> GenerationId {
        GenerationId::from_bytes([byte; 20])
    }

    fn step(from: u16, to: u16, output: u64, temporary: u64) -> MigrationStep {
        MigrationStep::new(
            version(from),
            version(to),
            MigrationStepKind::Rebuild,
            content_hash(&[u8::try_from(to).expect("fixture minor fits")]),
            output,
            temporary,
        )
        .expect("fixture step is valid")
    }

    fn registry() -> MigrationRegistry {
        MigrationRegistry::new(
            version(2),
            vec![step(0, 1, 100, 40), step(1, 2, 120, 30)],
            MigrationLimits::default(),
            &Cancellation::new(),
        )
        .expect("fixture registry is valid")
    }

    #[test]
    fn planner_builds_a_complete_forward_path_and_peak_space_estimate() {
        let plan = registry()
            .plan(
                version(0),
                generation(1),
                generation(2),
                20,
                &Cancellation::new(),
            )
            .expect("forward path plans");
        assert_eq!(
            plan.steps()
                .iter()
                .map(|step| (step.from(), step.to()))
                .collect::<Vec<_>>(),
            vec![(version(0), version(1)), (version(1), version(2))]
        );
        assert_eq!(
            plan.space_estimate()
                .expect("migration requires space")
                .required_bytes(),
            180
        );
    }

    #[test]
    fn planner_rejects_incompatible_missing_and_reused_targets() {
        let cancellation = Cancellation::new();
        assert_eq!(
            registry().plan(
                GenerationContractVersion::new(2, 0),
                generation(1),
                generation(2),
                0,
                &cancellation,
            ),
            Err(MigrationError::UnsupportedMajor)
        );
        assert_eq!(
            registry().plan(version(0), generation(1), generation(1), 0, &cancellation,),
            Err(MigrationError::InvalidTargetIdentity)
        );
        let incomplete = MigrationRegistry::new(
            version(2),
            vec![step(1, 2, 100, 10)],
            MigrationLimits::default(),
            &cancellation,
        )
        .expect("incomplete registry remains structurally valid");
        assert_eq!(
            incomplete.plan(version(0), generation(1), generation(2), 0, &cancellation,),
            Err(MigrationError::MissingPath)
        );
    }

    #[test]
    fn session_is_idempotent_and_rolls_back_to_the_retained_source() {
        let cancellation = Cancellation::new();
        let plan = registry()
            .plan(version(1), generation(1), generation(2), 20, &cancellation)
            .expect("fixture migration plans");
        let mut session = MigrationSession::new(plan).expect("migration session starts");
        let manifest_hash = content_hash(b"target-manifest");

        assert_eq!(session.resume_action(), MigrationResumeAction::BuildTarget);
        session
            .record_built(generation(2), manifest_hash, &cancellation)
            .expect("target build records");
        session
            .record_built(generation(2), manifest_hash, &cancellation)
            .expect("duplicate build record is idempotent");
        assert_eq!(session.resume_action(), MigrationResumeAction::VerifyTarget);
        session
            .record_verified(manifest_hash, &cancellation)
            .expect("target verification records");
        session
            .record_verified(manifest_hash, &cancellation)
            .expect("duplicate verification is idempotent");
        let activation = session
            .activate(generation(1), &cancellation)
            .expect("verified target activates");
        assert_eq!(activation.previous(), generation(1));
        assert_eq!(activation.active(), generation(2));
        assert_eq!(session.resume_action(), MigrationResumeAction::ServeTarget);
        assert_eq!(
            session
                .activate(generation(2), &cancellation)
                .expect("duplicate activation is idempotent"),
            activation
        );
        assert_eq!(
            session
                .rollback(generation(2), &cancellation)
                .expect("activated target rolls back"),
            generation(1)
        );
        assert_eq!(session.resume_action(), MigrationResumeAction::ServeSource);
        assert_eq!(
            session
                .rollback(generation(1), &cancellation)
                .expect("duplicate rollback is idempotent"),
            generation(1)
        );
    }

    #[test]
    fn manifest_drift_and_invalid_transition_fail_closed() {
        let cancellation = Cancellation::new();
        let plan = registry()
            .plan(version(1), generation(1), generation(2), 0, &cancellation)
            .expect("fixture migration plans");
        let mut session = MigrationSession::new(plan).expect("migration session starts");
        assert_eq!(
            session.record_verified(content_hash(b"early"), &cancellation),
            Err(MigrationError::InvalidTransition)
        );
        session
            .record_built(generation(2), content_hash(b"first"), &cancellation)
            .expect("target build records");
        assert_eq!(
            session.record_built(generation(2), content_hash(b"second"), &cancellation),
            Err(MigrationError::ManifestMismatch)
        );
    }

    #[test]
    fn canonical_journal_resumes_only_the_exact_plan_and_state() {
        let cancellation = Cancellation::new();
        let plan = registry()
            .plan(version(1), generation(1), generation(2), 10, &cancellation)
            .expect("fixture migration plans");
        let mut session = MigrationSession::new(plan.clone()).expect("migration session starts");
        let manifest_hash = content_hash(b"target-manifest");
        session
            .record_built(generation(2), manifest_hash, &cancellation)
            .expect("target build records");
        let journal = session
            .canonical_journal(&cancellation)
            .expect("journal encodes");
        let resumed = MigrationSession::resume(plan.clone(), &journal, &cancellation)
            .expect("exact journal resumes");
        assert_eq!(resumed.resume_action(), MigrationResumeAction::VerifyTarget);
        assert_eq!(
            resumed
                .canonical_journal(&cancellation)
                .expect("resumed journal is stable"),
            journal
        );

        let mut spaced = journal.clone();
        spaced.push(b'\n');
        assert_eq!(
            MigrationSession::resume(plan.clone(), &spaced, &cancellation),
            Err(MigrationError::NonCanonicalJournal)
        );

        let other_plan = registry()
            .plan(version(1), generation(1), generation(3), 10, &cancellation)
            .expect("other migration plans");
        assert_eq!(
            MigrationSession::resume(other_plan, &journal, &cancellation),
            Err(MigrationError::PlanMismatch)
        );
    }

    #[test]
    fn cancellation_wins_before_registry_or_session_work() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            MigrationRegistry::new(
                version(1),
                vec![step(0, 1, 10, 0)],
                MigrationLimits::default(),
                &cancellation,
            ),
            Err(MigrationError::Cancelled(_))
        ));
    }
}
