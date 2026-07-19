//! Bounded generation leases, retention, reclamation, and disk admission.
//!
//! Callers own physical storage and monotonic time; this state machine only
//! authorizes actions that preserve active and leased immutable generations.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::{Cancellation, Cancelled};
use rootlight_ids::{GenerationId, OperationId};

/// Hard ceiling for generations tracked by one lifecycle manager.
pub const HARD_MAX_LIFECYCLE_GENERATIONS: u16 = 1024;
/// Hard ceiling for concurrent generation leases.
pub const HARD_MAX_GENERATION_LEASES: u32 = 65_536;
/// Hard ceiling for generations removed by one reclaim commit.
pub const HARD_MAX_RECLAIM_GENERATIONS: u16 = 256;
/// Hard ceiling for aggregate managed generation bytes.
pub const HARD_MAX_LIFECYCLE_BYTES: u64 = 512 * 1024 * 1024 * 1024;

const DEFAULT_MAX_LIFECYCLE_GENERATIONS: u16 = 128;
const DEFAULT_MAX_GENERATION_LEASES: u32 = 4096;
const DEFAULT_MAX_RECLAIM_GENERATIONS: u16 = 32;
const DEFAULT_MAX_LIFECYCLE_BYTES: u64 = 64 * 1024 * 1024 * 1024;

/// Caller-supplied monotonic tick within one durable lease-clock epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeaseTick(u64);

impl LeaseTick {
    /// Creates a monotonic tick.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the caller-owned monotonic value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Strict activation order for one immutable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationSequence(u64);

impl GenerationSequence {
    /// Creates a positive activation sequence.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::InvalidSequence`] when `value` is zero.
    pub const fn new(value: u64) -> Result<Self, LifecycleError> {
        if value == 0 {
            Err(LifecycleError::InvalidSequence)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the monotonic activation sequence.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Immutable physical accounting for one verified generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationFootprint {
    generation: GenerationId,
    sequence: GenerationSequence,
    byte_length: u64,
}

impl GenerationFootprint {
    /// Creates a non-empty generation accounting record.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::InvalidFootprint`] when `byte_length` is zero.
    pub const fn new(
        generation: GenerationId,
        sequence: GenerationSequence,
        byte_length: u64,
    ) -> Result<Self, LifecycleError> {
        if byte_length == 0 {
            return Err(LifecycleError::InvalidFootprint);
        }
        Ok(Self {
            generation,
            sequence,
            byte_length,
        })
    }

    /// Returns the immutable generation identity.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the strict activation order.
    #[must_use]
    pub const fn sequence(self) -> GenerationSequence {
        self.sequence
    }

    /// Returns the complete accounted physical bytes.
    #[must_use]
    pub const fn byte_length(self) -> u64 {
        self.byte_length
    }
}

/// Validated hard ceilings for one lifecycle manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LifecycleLimits {
    max_generations: u16,
    max_leases: u32,
    max_reclaim_generations: u16,
    max_total_bytes: u64,
}

impl LifecycleLimits {
    /// Creates positive limits within the global lifecycle ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::InvalidLimits`] for zero or excessive limits.
    pub const fn new(
        max_generations: u16,
        max_leases: u32,
        max_reclaim_generations: u16,
        max_total_bytes: u64,
    ) -> Result<Self, LifecycleError> {
        if max_generations == 0
            || max_generations > HARD_MAX_LIFECYCLE_GENERATIONS
            || max_leases == 0
            || max_leases > HARD_MAX_GENERATION_LEASES
            || max_reclaim_generations == 0
            || max_reclaim_generations > HARD_MAX_RECLAIM_GENERATIONS
            || max_total_bytes == 0
            || max_total_bytes > HARD_MAX_LIFECYCLE_BYTES
        {
            return Err(LifecycleError::InvalidLimits);
        }
        Ok(Self {
            max_generations,
            max_leases,
            max_reclaim_generations,
            max_total_bytes,
        })
    }

    /// Returns the maximum retained generation count.
    #[must_use]
    pub const fn max_generations(self) -> u16 {
        self.max_generations
    }

    /// Returns the maximum concurrent lease count.
    #[must_use]
    pub const fn max_leases(self) -> u32 {
        self.max_leases
    }

    /// Returns the maximum size of one reclaim transaction.
    #[must_use]
    pub const fn max_reclaim_generations(self) -> u16 {
        self.max_reclaim_generations
    }

    /// Returns the maximum aggregate managed bytes.
    #[must_use]
    pub const fn max_total_bytes(self) -> u64 {
        self.max_total_bytes
    }
}

impl Default for LifecycleLimits {
    fn default() -> Self {
        Self {
            max_generations: DEFAULT_MAX_LIFECYCLE_GENERATIONS,
            max_leases: DEFAULT_MAX_GENERATION_LEASES,
            max_reclaim_generations: DEFAULT_MAX_RECLAIM_GENERATIONS,
            max_total_bytes: DEFAULT_MAX_LIFECYCLE_BYTES,
        }
    }
}

/// Minimum retained generation policy applied after active and leased pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    retain_latest: u16,
}

impl RetentionPolicy {
    /// Creates a policy retaining at least one latest generation.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::InvalidRetention`] when the count is zero or
    /// exceeds the lifecycle generation ceiling.
    pub const fn new(retain_latest: u16) -> Result<Self, LifecycleError> {
        if retain_latest == 0 || retain_latest > HARD_MAX_LIFECYCLE_GENERATIONS {
            return Err(LifecycleError::InvalidRetention);
        }
        Ok(Self { retain_latest })
    }

    /// Returns the minimum newest generation count.
    #[must_use]
    pub const fn retain_latest(self) -> u16 {
        self.retain_latest
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self { retain_latest: 2 }
    }
}

/// One process-safe reader lease over an immutable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GenerationLease {
    lease: OperationId,
    generation: GenerationId,
    deadline: LeaseTick,
}

impl GenerationLease {
    /// Returns the caller-owned unique lease identity.
    #[must_use]
    pub const fn lease(self) -> OperationId {
        self.lease
    }

    /// Returns the pinned generation.
    #[must_use]
    pub const fn generation(self) -> GenerationId {
        self.generation
    }

    /// Returns the monotonic expiry tick.
    #[must_use]
    pub const fn deadline(self) -> LeaseTick {
        self.deadline
    }
}

/// In-memory authorization state for physical generation lifecycle operations.
#[derive(Debug)]
pub struct GenerationLifecycle {
    limits: LifecycleLimits,
    revision: u64,
    active: Option<GenerationId>,
    total_bytes: u64,
    generations: BTreeMap<GenerationId, GenerationFootprint>,
    sequences: BTreeSet<GenerationSequence>,
    leases: BTreeMap<OperationId, GenerationLease>,
    reclaiming: BTreeSet<GenerationId>,
}

impl GenerationLifecycle {
    /// Creates an empty lifecycle state under fixed resource ceilings.
    #[must_use]
    pub fn new(limits: LifecycleLimits) -> Self {
        Self {
            limits,
            revision: 0,
            active: None,
            total_bytes: 0,
            generations: BTreeMap::new(),
            sequences: BTreeSet::new(),
            leases: BTreeMap::new(),
            reclaiming: BTreeSet::new(),
        }
    }

    /// Registers one already verified immutable generation.
    ///
    /// Registration does not activate the generation or make an older
    /// generation reclaimable.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation, duplicate identity or
    /// sequence, count exhaustion, byte overflow, or byte-budget exhaustion.
    pub fn register(
        &mut self,
        footprint: GenerationFootprint,
        cancellation: &Cancellation,
    ) -> Result<(), LifecycleError> {
        cancellation.check()?;
        if self.generations.len() >= usize::from(self.limits.max_generations) {
            return Err(LifecycleError::GenerationLimit);
        }
        if self.generations.contains_key(&footprint.generation) {
            return Err(LifecycleError::DuplicateGeneration);
        }
        if self.sequences.contains(&footprint.sequence) {
            return Err(LifecycleError::DuplicateSequence);
        }
        let total_bytes = self
            .total_bytes
            .checked_add(footprint.byte_length)
            .ok_or(LifecycleError::ByteLimit)?;
        if total_bytes > self.limits.max_total_bytes {
            return Err(LifecycleError::ByteLimit);
        }
        cancellation.check()?;
        self.bump_revision()?;
        self.sequences.insert(footprint.sequence);
        self.generations.insert(footprint.generation, footprint);
        self.total_bytes = total_bytes;
        Ok(())
    }

    /// Marks one registered generation as the sole active generation.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation or an unknown generation.
    pub fn activate(
        &mut self,
        generation: GenerationId,
        cancellation: &Cancellation,
    ) -> Result<(), LifecycleError> {
        cancellation.check()?;
        if !self.generations.contains_key(&generation) {
            return Err(LifecycleError::UnknownGeneration);
        }
        if self.reclaiming.contains(&generation) {
            return Err(LifecycleError::ReclaimInProgress);
        }
        if self.active != Some(generation) {
            self.bump_revision()?;
            self.active = Some(generation);
        }
        Ok(())
    }

    /// Acquires a unique unexpired reader lease.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation, an unknown generation,
    /// duplicate lease, capacity exhaustion, or a deadline not after `now`.
    pub fn acquire_lease(
        &mut self,
        lease: OperationId,
        generation: GenerationId,
        deadline: LeaseTick,
        now: LeaseTick,
        cancellation: &Cancellation,
    ) -> Result<GenerationLease, LifecycleError> {
        cancellation.check()?;
        if !self.generations.contains_key(&generation) {
            return Err(LifecycleError::UnknownGeneration);
        }
        if self.reclaiming.contains(&generation) {
            return Err(LifecycleError::ReclaimInProgress);
        }
        if self.leases.contains_key(&lease) {
            return Err(LifecycleError::DuplicateLease);
        }
        if self.leases.len()
            >= usize::try_from(self.limits.max_leases).map_err(|_| LifecycleError::InvalidLimits)?
        {
            return Err(LifecycleError::LeaseLimit);
        }
        if deadline <= now {
            return Err(LifecycleError::ExpiredLease);
        }
        let record = GenerationLease {
            lease,
            generation,
            deadline,
        };
        cancellation.check()?;
        self.bump_revision()?;
        self.leases.insert(lease, record);
        Ok(record)
    }

    /// Extends one live lease without changing its generation.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation, an unknown or expired
    /// lease, or a deadline that does not strictly advance.
    pub fn renew_lease(
        &mut self,
        lease: OperationId,
        deadline: LeaseTick,
        now: LeaseTick,
        cancellation: &Cancellation,
    ) -> Result<GenerationLease, LifecycleError> {
        cancellation.check()?;
        let current = self
            .leases
            .get(&lease)
            .copied()
            .ok_or(LifecycleError::UnknownLease)?;
        if current.deadline <= now {
            return Err(LifecycleError::ExpiredLease);
        }
        if deadline <= now || deadline <= current.deadline {
            return Err(LifecycleError::DeadlineNotAdvanced);
        }
        let renewed = GenerationLease {
            deadline,
            ..current
        };
        cancellation.check()?;
        self.bump_revision()?;
        self.leases.insert(lease, renewed);
        Ok(renewed)
    }

    /// Releases one reader lease idempotently.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::Cancelled`] when cancellation wins before the
    /// state transition.
    pub fn release_lease(
        &mut self,
        lease: OperationId,
        cancellation: &Cancellation,
    ) -> Result<bool, LifecycleError> {
        cancellation.check()?;
        if self.leases.contains_key(&lease) {
            self.bump_revision()?;
            self.leases.remove(&lease);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Removes leases whose deadline is at or before `now`.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation or revision overflow.
    pub fn reap_expired(
        &mut self,
        now: LeaseTick,
        cancellation: &Cancellation,
    ) -> Result<Vec<OperationId>, LifecycleError> {
        cancellation.check()?;
        let mut expired = Vec::new();
        for (lease, record) in &self.leases {
            cancellation.check()?;
            if record.deadline <= now {
                expired.push(*lease);
            }
        }
        if !expired.is_empty() {
            self.bump_revision()?;
            for lease in &expired {
                self.leases.remove(lease);
            }
        }
        Ok(expired)
    }

    /// Plans a bounded reclaim without mutating lifecycle state.
    ///
    /// Active, live-leased, and newest policy-retained generations are always
    /// excluded. Expired leases do not protect generations.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation or accounting overflow.
    pub fn plan_reclaim(
        &self,
        policy: RetentionPolicy,
        now: LeaseTick,
        cancellation: &Cancellation,
    ) -> Result<ReclaimPlan, LifecycleError> {
        cancellation.check()?;
        let mut protected = BTreeSet::new();
        if let Some(active) = self.active {
            protected.insert(active);
        }
        for lease in self.leases.values() {
            cancellation.check()?;
            if lease.deadline > now {
                protected.insert(lease.generation);
            }
        }

        let mut newest = self.generations.values().copied().collect::<Vec<_>>();
        newest.sort_unstable_by_key(|record| std::cmp::Reverse(record.sequence));
        for record in newest.iter().take(usize::from(policy.retain_latest)) {
            cancellation.check()?;
            protected.insert(record.generation);
        }

        let mut eligible = self
            .generations
            .values()
            .copied()
            .filter(|record| {
                !protected.contains(&record.generation)
                    && !self.reclaiming.contains(&record.generation)
            })
            .collect::<Vec<_>>();
        eligible.sort_unstable_by_key(|record| record.sequence);
        let maximum = usize::from(self.limits.max_reclaim_generations);
        let truncated = eligible.len() > maximum;
        eligible.truncate(maximum);
        let mut reclaim_bytes = 0_u64;
        for record in &eligible {
            cancellation.check()?;
            reclaim_bytes = reclaim_bytes
                .checked_add(record.byte_length)
                .ok_or(LifecycleError::ByteLimit)?;
        }
        Ok(ReclaimPlan {
            revision: self.revision,
            observed_at: now,
            generations: eligible,
            reclaim_bytes,
            truncated,
        })
    }

    /// Locks one unchanged reclaim plan against activation and new leases.
    ///
    /// The caller may perform physical deletion only after this transition.
    /// It must then call [`Self::finish_reclaim`] or [`Self::abort_reclaim`].
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation, a stale plan, protected or
    /// missing candidates, or accounting inconsistency.
    pub fn begin_reclaim(
        &mut self,
        plan: ReclaimPlan,
        cancellation: &Cancellation,
    ) -> Result<ReclaimTransaction, LifecycleError> {
        cancellation.check()?;
        if plan.revision != self.revision {
            return Err(LifecycleError::StaleReclaimPlan);
        }
        let live_generations = self
            .leases
            .values()
            .filter(|lease| lease.deadline > plan.observed_at)
            .map(|lease| lease.generation)
            .collect::<BTreeSet<_>>();
        for record in &plan.generations {
            cancellation.check()?;
            if self.active == Some(record.generation)
                || live_generations.contains(&record.generation)
                || self.reclaiming.contains(&record.generation)
                || self.generations.get(&record.generation) != Some(record)
            {
                return Err(LifecycleError::ProtectedGeneration);
            }
        }
        if !plan.generations.is_empty() {
            self.bump_revision()?;
            for record in &plan.generations {
                self.reclaiming.insert(record.generation);
            }
        }
        Ok(ReclaimTransaction {
            generations: plan.generations,
            reclaim_bytes: plan.reclaim_bytes,
        })
    }

    /// Commits exact successful deletions and unlocks every remaining candidate.
    ///
    /// `deleted_generations` permits bounded partial progress when a later
    /// physical deletion fails. Every identity must belong to the transaction.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation, transaction drift, or
    /// inconsistent byte accounting.
    pub fn finish_reclaim(
        &mut self,
        transaction: ReclaimTransaction,
        deleted_generations: Vec<GenerationId>,
        cancellation: &Cancellation,
    ) -> Result<ReclaimReceipt, LifecycleError> {
        cancellation.check()?;
        if transaction.generations.iter().any(|record| {
            !self.reclaiming.contains(&record.generation)
                || self.generations.get(&record.generation) != Some(record)
        }) {
            return Err(LifecycleError::InvalidReclaimTransaction);
        }
        let deleted_count = deleted_generations.len();
        if deleted_count > transaction.generations.len() {
            return Err(LifecycleError::InvalidReclaimTransaction);
        }
        let deleted = deleted_generations.into_iter().collect::<BTreeSet<_>>();
        if deleted.len() != deleted_count
            || deleted.iter().any(|generation| {
                !transaction
                    .generations
                    .iter()
                    .any(|record| record.generation == *generation)
            })
        {
            return Err(LifecycleError::InvalidReclaimTransaction);
        }
        let removed = transaction
            .generations
            .iter()
            .copied()
            .filter(|record| deleted.contains(&record.generation))
            .collect::<Vec<_>>();
        let mut reclaimed_bytes = 0_u64;
        for record in &removed {
            cancellation.check()?;
            reclaimed_bytes = reclaimed_bytes
                .checked_add(record.byte_length)
                .ok_or(LifecycleError::Accounting)?;
        }
        let total_bytes = self
            .total_bytes
            .checked_sub(reclaimed_bytes)
            .ok_or(LifecycleError::Accounting)?;
        if !transaction.generations.is_empty() {
            self.bump_revision()?;
            for record in &transaction.generations {
                self.reclaiming.remove(&record.generation);
                if deleted.contains(&record.generation) {
                    self.generations.remove(&record.generation);
                    self.sequences.remove(&record.sequence);
                }
            }
            self.total_bytes = total_bytes;
        }
        Ok(ReclaimReceipt {
            generations: removed,
            reclaimed_bytes,
            remaining_bytes: self.total_bytes,
        })
    }

    /// Unlocks a reclaim transaction after physical deletion is abandoned.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError`] for cancellation or transaction drift.
    pub fn abort_reclaim(
        &mut self,
        transaction: ReclaimTransaction,
        cancellation: &Cancellation,
    ) -> Result<(), LifecycleError> {
        cancellation.check()?;
        self.finish_reclaim(transaction, Vec::new(), cancellation)
            .map(|_| ())
    }

    /// Returns the sole active generation.
    #[must_use]
    pub const fn active(&self) -> Option<GenerationId> {
        self.active
    }

    /// Returns aggregate accounted generation bytes.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// Returns the number of registered generations.
    #[must_use]
    pub fn generation_count(&self) -> usize {
        self.generations.len()
    }

    /// Returns the number of retained leases, including expired records not yet reaped.
    #[must_use]
    pub fn lease_count(&self) -> usize {
        self.leases.len()
    }

    fn bump_revision(&mut self) -> Result<(), LifecycleError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(LifecycleError::RevisionOverflow)?;
        Ok(())
    }
}

/// Immutable bounded authorization for physical generation deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimPlan {
    revision: u64,
    observed_at: LeaseTick,
    generations: Vec<GenerationFootprint>,
    reclaim_bytes: u64,
    truncated: bool,
}

/// Exclusive authorization for physical deletion of fixed generations.
#[derive(Debug, PartialEq, Eq)]
pub struct ReclaimTransaction {
    generations: Vec<GenerationFootprint>,
    reclaim_bytes: u64,
}

impl ReclaimTransaction {
    /// Returns locked deletion candidates in oldest-first activation order.
    #[must_use]
    pub fn generations(&self) -> &[GenerationFootprint] {
        &self.generations
    }

    /// Returns exact bytes locked for deletion.
    #[must_use]
    pub const fn reclaim_bytes(&self) -> u64 {
        self.reclaim_bytes
    }
}

impl ReclaimPlan {
    /// Returns deletion candidates in oldest-first activation order.
    #[must_use]
    pub fn generations(&self) -> &[GenerationFootprint] {
        &self.generations
    }

    /// Returns exact bytes represented by the deletion candidates.
    #[must_use]
    pub const fn reclaim_bytes(&self) -> u64 {
        self.reclaim_bytes
    }

    /// Returns whether more eligible candidates remain after this plan.
    #[must_use]
    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

/// Accounting result after physical deletion and reclaim commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReclaimReceipt {
    generations: Vec<GenerationFootprint>,
    reclaimed_bytes: u64,
    remaining_bytes: u64,
}

impl ReclaimReceipt {
    /// Returns the removed generation records.
    #[must_use]
    pub fn generations(&self) -> &[GenerationFootprint] {
        &self.generations
    }

    /// Returns exact reclaimed bytes.
    #[must_use]
    pub const fn reclaimed_bytes(&self) -> u64 {
        self.reclaimed_bytes
    }

    /// Returns lifecycle bytes remaining after commit.
    #[must_use]
    pub const fn remaining_bytes(&self) -> u64 {
        self.remaining_bytes
    }
}

/// Side-by-side disk estimate required before expensive generation work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicationSpaceEstimate {
    final_bytes: u64,
    temporary_bytes: u64,
    reserve_bytes: u64,
    required_bytes: u64,
}

impl PublicationSpaceEstimate {
    /// Creates a checked disk estimate.
    ///
    /// `final_bytes` must be non-zero. Temporary and reserve bytes may be zero,
    /// while their sum must remain within the lifecycle hard byte ceiling.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::InvalidSpaceEstimate`] for a zero final size,
    /// overflow, or an estimate above [`HARD_MAX_LIFECYCLE_BYTES`].
    pub fn new(
        final_bytes: u64,
        temporary_bytes: u64,
        reserve_bytes: u64,
    ) -> Result<Self, LifecycleError> {
        if final_bytes == 0 {
            return Err(LifecycleError::InvalidSpaceEstimate);
        }
        let Some(required_bytes) = final_bytes
            .checked_add(temporary_bytes)
            .and_then(|value| value.checked_add(reserve_bytes))
        else {
            return Err(LifecycleError::InvalidSpaceEstimate);
        };
        if required_bytes > HARD_MAX_LIFECYCLE_BYTES {
            return Err(LifecycleError::InvalidSpaceEstimate);
        }
        Ok(Self {
            final_bytes,
            temporary_bytes,
            reserve_bytes,
            required_bytes,
        })
    }

    /// Requires sufficient free bytes before generation work begins.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::DiskPressure`] when `available_bytes` is below
    /// the checked side-by-side requirement.
    pub const fn admit(self, available_bytes: u64) -> Result<DiskReservation, LifecycleError> {
        if available_bytes < self.required_bytes {
            return Err(LifecycleError::DiskPressure {
                required: self.required_bytes,
                available: available_bytes,
            });
        }
        Ok(DiskReservation {
            final_bytes: self.final_bytes,
            temporary_bytes: self.temporary_bytes,
            reserve_bytes: self.reserve_bytes,
            remaining_bytes: available_bytes - self.required_bytes,
        })
    }

    /// Returns the checked total free-space requirement.
    #[must_use]
    pub const fn required_bytes(self) -> u64 {
        self.required_bytes
    }
}

/// Successful disk-pressure admission for one side-by-side generation build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskReservation {
    final_bytes: u64,
    temporary_bytes: u64,
    reserve_bytes: u64,
    remaining_bytes: u64,
}

impl DiskReservation {
    /// Returns bytes expected in the completed generation.
    #[must_use]
    pub const fn final_bytes(self) -> u64 {
        self.final_bytes
    }

    /// Returns peak temporary bytes included in admission.
    #[must_use]
    pub const fn temporary_bytes(self) -> u64 {
        self.temporary_bytes
    }

    /// Returns the protected free-space margin.
    #[must_use]
    pub const fn reserve_bytes(self) -> u64 {
        self.reserve_bytes
    }

    /// Returns available bytes remaining beyond the full estimate.
    #[must_use]
    pub const fn remaining_bytes(self) -> u64 {
        self.remaining_bytes
    }
}

/// Invalid lifecycle input or stopped lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LifecycleError {
    /// Cooperative cancellation stopped the operation.
    #[error(transparent)]
    Cancelled(#[from] Cancelled),
    /// One lifecycle resource limit was zero or above its hard ceiling.
    #[error("generation lifecycle limits are invalid")]
    InvalidLimits,
    /// Generation activation sequence must be positive.
    #[error("generation activation sequence is invalid")]
    InvalidSequence,
    /// Generation byte accounting must be positive.
    #[error("generation footprint is invalid")]
    InvalidFootprint,
    /// Retention must preserve at least one bounded generation.
    #[error("generation retention policy is invalid")]
    InvalidRetention,
    /// A generation identity was already registered.
    #[error("generation is already registered")]
    DuplicateGeneration,
    /// A strict activation sequence was reused.
    #[error("generation activation sequence is duplicated")]
    DuplicateSequence,
    /// The configured generation count is exhausted.
    #[error("generation lifecycle count limit is exhausted")]
    GenerationLimit,
    /// Aggregate byte accounting overflowed or exhausted its budget.
    #[error("generation lifecycle byte limit is exhausted")]
    ByteLimit,
    /// The requested generation is not registered.
    #[error("generation lifecycle does not contain the requested generation")]
    UnknownGeneration,
    /// A lease identity was already active.
    #[error("generation lease identity is duplicated")]
    DuplicateLease,
    /// The configured lease count is exhausted.
    #[error("generation lease limit is exhausted")]
    LeaseLimit,
    /// The requested lease does not exist.
    #[error("generation lease does not exist")]
    UnknownLease,
    /// A lease deadline is already expired.
    #[error("generation lease deadline has expired")]
    ExpiredLease,
    /// A renewed deadline did not strictly advance.
    #[error("generation lease deadline did not advance")]
    DeadlineNotAdvanced,
    /// A concurrent lifecycle mutation invalidated a reclaim authorization.
    #[error("generation reclaim plan is stale")]
    StaleReclaimPlan,
    /// A reclaim candidate became active, leased, or inconsistent.
    #[error("generation reclaim candidate is protected")]
    ProtectedGeneration,
    /// A generation is locked by an active reclaim transaction.
    #[error("generation reclaim is already in progress")]
    ReclaimInProgress,
    /// A reclaim transaction no longer matches the locked generation set.
    #[error("generation reclaim transaction is invalid")]
    InvalidReclaimTransaction,
    /// Internal byte accounting became inconsistent.
    #[error("generation lifecycle accounting is inconsistent")]
    Accounting,
    /// The internal mutation revision exhausted its integer range.
    #[error("generation lifecycle revision is exhausted")]
    RevisionOverflow,
    /// A disk estimate was empty, overflowed, or exceeded its hard ceiling.
    #[error("generation publication space estimate is invalid")]
    InvalidSpaceEstimate,
    /// Free space cannot preserve side-by-side publication and reserve margin.
    #[error("generation publication requires {required} bytes but only {available} are available")]
    DiskPressure {
        /// Checked bytes required before work begins.
        required: u64,
        /// Caller-observed free bytes.
        available: u64,
    },
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;

    use super::*;

    fn generation(byte: u8) -> GenerationId {
        GenerationId::from_bytes([byte; 20])
    }

    fn operation(byte: u8) -> OperationId {
        OperationId::from_bytes([byte; 16])
    }

    fn footprint(byte: u8, sequence: u64, bytes: u64) -> GenerationFootprint {
        GenerationFootprint::new(
            generation(byte),
            GenerationSequence::new(sequence).expect("fixture sequence is positive"),
            bytes,
        )
        .expect("fixture footprint is non-empty")
    }

    fn populated() -> GenerationLifecycle {
        let mut lifecycle = GenerationLifecycle::new(LifecycleLimits::default());
        let cancellation = Cancellation::new();
        for record in [
            footprint(1, 1, 10),
            footprint(2, 2, 20),
            footprint(3, 3, 30),
            footprint(4, 4, 40),
        ] {
            lifecycle
                .register(record, &cancellation)
                .expect("fixture generation registers");
        }
        lifecycle
            .activate(generation(4), &cancellation)
            .expect("latest fixture activates");
        lifecycle
    }

    #[test]
    fn active_live_leased_and_newest_generations_are_never_reclaimed() {
        let cancellation = Cancellation::new();
        let mut lifecycle = populated();
        lifecycle
            .acquire_lease(
                operation(1),
                generation(1),
                LeaseTick::new(20),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("old generation lease acquires");
        let plan = lifecycle
            .plan_reclaim(
                RetentionPolicy::new(2).expect("fixture policy is valid"),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("reclaim plans");

        assert_eq!(
            plan.generations()
                .iter()
                .map(|record| record.generation())
                .collect::<Vec<_>>(),
            vec![generation(2)]
        );
        assert_eq!(lifecycle.active(), Some(generation(4)));
    }

    #[test]
    fn expired_lease_allows_oldest_first_bounded_reclaim() {
        let cancellation = Cancellation::new();
        let mut lifecycle = populated();
        lifecycle
            .acquire_lease(
                operation(1),
                generation(1),
                LeaseTick::new(10),
                LeaseTick::new(1),
                &cancellation,
            )
            .expect("fixture lease acquires");
        let plan = lifecycle
            .plan_reclaim(
                RetentionPolicy::new(1).expect("fixture policy is valid"),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("reclaim plans");

        assert_eq!(
            plan.generations()
                .iter()
                .map(|record| record.generation())
                .collect::<Vec<_>>(),
            vec![generation(1), generation(2), generation(3)]
        );
        assert_eq!(plan.reclaim_bytes(), 60);
        let transaction = lifecycle
            .begin_reclaim(plan, &cancellation)
            .expect("unchanged reclaim locks");
        assert_eq!(
            lifecycle.acquire_lease(
                operation(2),
                generation(2),
                LeaseTick::new(20),
                LeaseTick::new(10),
                &cancellation,
            ),
            Err(LifecycleError::ReclaimInProgress)
        );
        let receipt = lifecycle
            .finish_reclaim(
                transaction,
                vec![generation(1), generation(2), generation(3)],
                &cancellation,
            )
            .expect("completed physical reclaim commits");
        assert_eq!(receipt.reclaimed_bytes(), 60);
        assert_eq!(receipt.remaining_bytes(), 40);
        assert_eq!(lifecycle.generation_count(), 1);
    }

    #[test]
    fn lease_mutation_invalidates_an_outstanding_reclaim_plan() {
        let cancellation = Cancellation::new();
        let mut lifecycle = populated();
        let plan = lifecycle
            .plan_reclaim(
                RetentionPolicy::new(1).expect("fixture policy is valid"),
                LeaseTick::new(1),
                &cancellation,
            )
            .expect("reclaim plans");
        lifecycle
            .acquire_lease(
                operation(2),
                generation(1),
                LeaseTick::new(20),
                LeaseTick::new(1),
                &cancellation,
            )
            .expect("concurrent lease acquires");

        assert_eq!(
            lifecycle.begin_reclaim(plan, &cancellation),
            Err(LifecycleError::StaleReclaimPlan)
        );
        assert_eq!(lifecycle.generation_count(), 4);
        assert_eq!(lifecycle.total_bytes(), 100);
    }

    #[test]
    fn partial_physical_reclaim_unlocks_undeleted_generations() {
        let cancellation = Cancellation::new();
        let mut lifecycle = populated();
        let plan = lifecycle
            .plan_reclaim(
                RetentionPolicy::new(1).expect("fixture policy is valid"),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("reclaim plans");
        let transaction = lifecycle
            .begin_reclaim(plan, &cancellation)
            .expect("reclaim locks");
        let receipt = lifecycle
            .finish_reclaim(transaction, vec![generation(1)], &cancellation)
            .expect("partial physical result commits");

        assert_eq!(
            receipt
                .generations()
                .iter()
                .map(|record| record.generation())
                .collect::<Vec<_>>(),
            vec![generation(1)]
        );
        assert_eq!(receipt.reclaimed_bytes(), 10);
        assert_eq!(lifecycle.generation_count(), 3);
        lifecycle
            .acquire_lease(
                operation(3),
                generation(2),
                LeaseTick::new(20),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("undeleted candidate is unlocked");
    }

    #[test]
    fn renewal_and_expiry_are_monotonic_and_reap_is_deterministic() {
        let cancellation = Cancellation::new();
        let mut lifecycle = populated();
        lifecycle
            .acquire_lease(
                operation(2),
                generation(2),
                LeaseTick::new(20),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("fixture lease acquires");
        assert_eq!(
            lifecycle.renew_lease(
                operation(2),
                LeaseTick::new(20),
                LeaseTick::new(10),
                &cancellation,
            ),
            Err(LifecycleError::DeadlineNotAdvanced)
        );
        let renewed = lifecycle
            .renew_lease(
                operation(2),
                LeaseTick::new(30),
                LeaseTick::new(10),
                &cancellation,
            )
            .expect("deadline advances");
        assert_eq!(renewed.deadline(), LeaseTick::new(30));
        assert!(
            lifecycle
                .reap_expired(LeaseTick::new(29), &cancellation)
                .expect("early reap succeeds")
                .is_empty()
        );
        assert_eq!(
            lifecycle
                .reap_expired(LeaseTick::new(30), &cancellation)
                .expect("deadline reap succeeds"),
            vec![operation(2)]
        );
    }

    #[test]
    fn registration_limits_fail_before_partial_mutation() {
        let limits = LifecycleLimits::new(1, 1, 1, 10).expect("fixture limits are valid");
        let cancellation = Cancellation::new();
        let mut lifecycle = GenerationLifecycle::new(limits);
        lifecycle
            .register(footprint(1, 1, 10), &cancellation)
            .expect("exact footprint registers");
        assert_eq!(
            lifecycle.register(footprint(2, 2, 1), &cancellation),
            Err(LifecycleError::GenerationLimit)
        );
        assert_eq!(lifecycle.generation_count(), 1);
        assert_eq!(lifecycle.total_bytes(), 10);
    }

    #[test]
    fn disk_preflight_preserves_side_by_side_and_reserve_bytes() {
        let estimate =
            PublicationSpaceEstimate::new(100, 20, 30).expect("fixture estimate is valid");
        assert_eq!(estimate.required_bytes(), 150);
        assert_eq!(
            estimate.admit(149),
            Err(LifecycleError::DiskPressure {
                required: 150,
                available: 149
            })
        );
        let reservation = estimate.admit(200).expect("space is sufficient");
        assert_eq!(reservation.final_bytes(), 100);
        assert_eq!(reservation.temporary_bytes(), 20);
        assert_eq!(reservation.reserve_bytes(), 30);
        assert_eq!(reservation.remaining_bytes(), 50);
    }

    #[test]
    fn cancellation_prevents_lifecycle_mutation() {
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        let mut lifecycle = GenerationLifecycle::new(LifecycleLimits::default());
        assert!(matches!(
            lifecycle.register(footprint(1, 1, 10), &cancellation),
            Err(LifecycleError::Cancelled(_))
        ));
        assert_eq!(lifecycle.generation_count(), 0);
        assert_eq!(lifecycle.total_bytes(), 0);
    }
}
