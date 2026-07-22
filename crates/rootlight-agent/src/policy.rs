//! Shared execution policy for transport-neutral agent orchestration.
//!
//! This module admits every request under total server-owned ceilings and
//! provides transactional accounting for standalone and nested agent work.

use rootlight_mcp_contract::vertical::{ResponseBudget, ResponseProfile};
use rootlight_query::QueryBudget;

/// Read-only cancellation signal supplied by an application adapter.
///
/// The agent boundary deliberately owns cancellation checkpoints while the
/// concrete signal remains selectable by the composing application.
pub trait CancellationSignal {
    /// Reports whether the caller has requested cancellation.
    fn is_cancelled(&self) -> bool;
}

/// Cancellation signal for deterministic work that cannot be cancelled.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NeverCancelled;

impl CancellationSignal for NeverCancelled {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// Resource dimensions tracked by the shared agent budget ledger.
///
/// The declaration order is the stable precedence used when one atomic
/// operation would exceed multiple limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetResource {
    /// Logical rows inspected or materialized.
    Rows,
    /// Returned result objects.
    Results,
    /// Deterministic conservative output-token estimate.
    Tokens,
    /// Tokens counted by an actual tokenizer when one is available.
    ActualTokens,
    /// Raw source bytes.
    SourceBytes,
    /// Relationship or traversal facts.
    TraversalFacts,
    /// Plan or traversal depth.
    Depth,
    /// Independently returned paths.
    Paths,
    /// Exact serialized response bytes.
    JsonBytes,
    /// Variable-sized bytes owned by retained response data.
    MemoryBytes,
    /// Maximum cooperative wall time observed in milliseconds.
    Time,
}

/// One resource charge made by an agent planner or shaper.
///
/// Rows, results, both token counters, source bytes, traversal facts, paths,
/// response bytes, and owned memory are additive. Depth and wall time are
/// high-water marks. `tokens` remains the deterministic estimate used for
/// admission; `actual_tokens` records tokenizer output only when available.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetCharge {
    /// Logical rows inspected or materialized.
    pub rows: u64,
    /// Returned result objects.
    pub results: u64,
    /// Deterministic conservative output-token estimate.
    pub tokens: u64,
    /// Actual tokenizer tokens measured for this charge, or zero when unavailable.
    pub actual_tokens: u64,
    /// Raw source bytes.
    pub source_bytes: u64,
    /// Relationship or traversal facts.
    pub traversal_facts: u64,
    /// Maximum plan or traversal depth reached by this charge.
    pub depth: u64,
    /// Independently returned paths.
    pub paths: u64,
    /// Exact bytes in the serialized response representation.
    pub json_bytes: u64,
    /// Variable-sized bytes owned by retained response data.
    pub memory_bytes: u64,
    /// Cooperative elapsed time in milliseconds.
    pub time_ms: u64,
}

/// Total hard ceilings applied to one ledger or child allocation.
///
/// Every dimension is present. Missing public request fields therefore select
/// an admitted default rather than disabling enforcement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetLimits {
    maximums: BudgetCharge,
}

impl BudgetLimits {
    /// Creates limits from a complete set of resource maximums.
    #[must_use]
    pub const fn from_maximums(maximums: BudgetCharge) -> Self {
        Self { maximums }
    }

    /// Returns the common server ceiling used by compatibility constructors.
    ///
    /// Tool entry points should further reduce this ceiling with
    /// [`BudgetLimits::constrained_by`] before beginning work.
    #[must_use]
    pub fn server_ceiling() -> Self {
        let query = QueryBudget::new();
        let time_ms = u64::try_from(query.max_duration().as_millis()).unwrap_or(u64::MAX);
        Self::from_maximums(BudgetCharge {
            rows: query.max_rows(),
            results: query.max_results(),
            tokens: query.max_tokens(),
            // A supported byte-level tokenizer cannot emit more tokens than
            // the exact serialized UTF-8 bytes from which those tokens are measured.
            actual_tokens: query.max_json_bytes(),
            source_bytes: query.max_source_bytes(),
            traversal_facts: query.max_edges(),
            depth: 16,
            paths: query.max_results(),
            json_bytes: query.max_json_bytes(),
            memory_bytes: query.max_memory_bytes(),
            time_ms,
        })
    }

    /// Returns the complete maximum vector.
    #[must_use]
    pub const fn maximums(self) -> BudgetCharge {
        self.maximums
    }

    /// Returns the maximum for one resource.
    #[must_use]
    pub const fn limit(self, resource: BudgetResource) -> u64 {
        match resource {
            BudgetResource::Rows => self.maximums.rows,
            BudgetResource::Results => self.maximums.results,
            BudgetResource::Tokens => self.maximums.tokens,
            BudgetResource::ActualTokens => self.maximums.actual_tokens,
            BudgetResource::SourceBytes => self.maximums.source_bytes,
            BudgetResource::TraversalFacts => self.maximums.traversal_facts,
            BudgetResource::Depth => self.maximums.depth,
            BudgetResource::Paths => self.maximums.paths,
            BudgetResource::JsonBytes => self.maximums.json_bytes,
            BudgetResource::MemoryBytes => self.maximums.memory_bytes,
            BudgetResource::Time => self.maximums.time_ms,
        }
    }

    /// Returns limits with the deterministic estimated-token ceiling reduced.
    ///
    /// Actual tokenizer usage is an independent measured dimension bounded by
    /// serialized bytes; an estimated-token request cannot constrain it safely.
    #[must_use]
    pub const fn with_tokens(mut self, maximum: u64) -> Self {
        self.maximums.tokens = min_u64(self.maximums.tokens, maximum);
        self
    }

    /// Reduces every resource to the lower ceiling from two policies.
    #[must_use]
    pub const fn constrained_by(self, other: Self) -> Self {
        Self::from_maximums(minimum_charge(self.maximums, other.maximums))
    }

    /// Reduces limits by every present field in a public response budget.
    ///
    /// Missing fields preserve the current total ceiling. Evidence level is
    /// representation policy and does not alter resource accounting.
    #[must_use]
    pub fn constrained_by_response_budget(self, requested: Option<&ResponseBudget>) -> Self {
        let Some(requested) = requested else {
            return self;
        };
        let mut maximums = self.maximums;
        if let Some(limit) = requested.max_results {
            maximums.results = maximums.results.min(u64::from(limit));
        }
        if let Some(limit) = requested.max_tokens {
            maximums.tokens = maximums.tokens.min(u64::from(limit));
        }
        if let Some(limit) = requested.max_source_bytes {
            maximums.source_bytes = maximums.source_bytes.min(u64::from(limit));
        }
        if let Some(limit) = requested.max_traversal_facts {
            maximums.traversal_facts = maximums.traversal_facts.min(u64::from(limit));
        }
        if let Some(limit) = requested.max_depth {
            maximums.depth = maximums.depth.min(u64::from(limit));
        }
        if let Some(limit) = requested.max_paths {
            maximums.paths = maximums.paths.min(u64::from(limit));
        }
        if let Some(limit) = requested.timeout_ms {
            maximums.time_ms = maximums.time_ms.min(u64::from(limit));
        }
        Self::from_maximums(maximums)
    }
}

impl Default for BudgetLimits {
    fn default() -> Self {
        Self::server_ceiling()
    }
}

/// Failure returned by agent execution-policy checkpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionPolicyError {
    /// The caller cancelled the operation.
    #[error("agent operation was cancelled")]
    Cancelled,
    /// One shared resource limit would be exceeded.
    #[error("agent resource budget was exceeded: {resource:?}")]
    BudgetExceeded {
        /// Resource that reached its admitted ceiling.
        resource: BudgetResource,
    },
}

/// Immutable accounting view of one budget ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSnapshot {
    limits: BudgetLimits,
    consumed: BudgetCharge,
    reserved: BudgetCharge,
    remaining: BudgetCharge,
    limiting_resource: Option<BudgetResource>,
}

impl BudgetSnapshot {
    /// Returns the effective hard limits.
    #[must_use]
    pub const fn limits(self) -> BudgetLimits {
        self.limits
    }

    /// Returns committed authoritative usage.
    #[must_use]
    pub const fn consumed(self) -> BudgetCharge {
        self.consumed
    }

    /// Returns capacity held for work that has not committed.
    #[must_use]
    pub const fn reserved(self) -> BudgetCharge {
        self.reserved
    }

    /// Returns capacity that can still be allocated.
    ///
    /// Depth and time are high-water dimensions, so their remaining values are
    /// the largest absolute high-water marks another charge may report.
    #[must_use]
    pub const fn remaining(self) -> BudgetCharge {
        self.remaining
    }

    /// Returns the stable resource from the most recent rejected operation.
    #[must_use]
    pub const fn limiting_resource(self) -> Option<BudgetResource> {
        self.limiting_resource
    }
}

/// Shared budget ledger for nested agent orchestration.
///
/// Limits are total and server-bounded. Charges use checked arithmetic, and
/// reservation commit or release is atomic across all resource dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetLedger {
    limits: BudgetLimits,
    consumed: BudgetCharge,
    reserved: BudgetCharge,
    limiting_resource: Option<BudgetResource>,
}

impl BudgetLedger {
    /// Creates a server-bounded ledger from optional caller-provided reductions.
    #[must_use]
    pub fn new(requested: Option<ResponseBudget>) -> Self {
        let limits =
            BudgetLimits::server_ceiling().constrained_by_response_budget(requested.as_ref());
        Self::from_limits(limits)
    }

    /// Creates a ledger from already-admitted total limits.
    #[must_use]
    pub const fn from_limits(limits: BudgetLimits) -> Self {
        Self {
            limits,
            consumed: BudgetCharge {
                rows: 0,
                results: 0,
                tokens: 0,
                actual_tokens: 0,
                source_bytes: 0,
                traversal_facts: 0,
                depth: 0,
                paths: 0,
                json_bytes: 0,
                memory_bytes: 0,
                time_ms: 0,
            },
            reserved: BudgetCharge {
                rows: 0,
                results: 0,
                tokens: 0,
                actual_tokens: 0,
                source_bytes: 0,
                traversal_facts: 0,
                depth: 0,
                paths: 0,
                json_bytes: 0,
                memory_bytes: 0,
                time_ms: 0,
            },
            limiting_resource: None,
        }
    }

    /// Creates a server-bounded ledger with a tool-specific token ceiling.
    ///
    /// This constructor accepts `u64` because domain operations such as
    /// `context.pack` have dedicated ceilings that differ from the shared
    /// [`ResponseBudget`] schema.
    #[must_use]
    pub fn with_token_limit(max_tokens: u64) -> Self {
        Self::from_limits(BudgetLimits::server_ceiling().with_tokens(max_tokens))
    }

    /// Returns the effective total limits.
    #[must_use]
    pub const fn limits(&self) -> BudgetLimits {
        self.limits
    }

    /// Returns all resources committed to this ledger.
    #[must_use]
    pub const fn consumed(&self) -> BudgetCharge {
        self.consumed
    }

    /// Returns capacity currently available to new work.
    ///
    /// Depth and time are high-water dimensions, so another charge may report
    /// any absolute value up to the returned value.
    ///
    /// # Panics
    ///
    /// Panics only if internal ledger counters violate their admission invariant.
    #[must_use]
    pub fn remaining(&self) -> BudgetCharge {
        let admitted = combined_usage(self.consumed, self.reserved)
            .expect("admitted ledger counters cannot overflow");
        remaining_capacity(self.limits.maximums, admitted)
    }

    /// Returns an immutable accounting snapshot.
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            limits: self.limits,
            consumed: self.consumed,
            reserved: self.reserved,
            remaining: self.remaining(),
            limiting_resource: self.limiting_resource,
        }
    }

    /// Reserves capacity for work before it starts.
    ///
    /// Dropping the returned reservation releases it. Call
    /// [`BudgetReservation::commit`] to replace the reservation with measured
    /// usage.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::BudgetExceeded`] when the reservation
    /// would overflow an authoritative counter or exceed an effective limit.
    pub fn reserve(
        &mut self,
        requested: BudgetCharge,
    ) -> Result<BudgetReservation<'_>, ExecutionPolicyError> {
        let previous_reserved = self.reserved;
        let proposed_reserved = match combined_usage(previous_reserved, requested) {
            Ok(proposed) => proposed,
            Err(resource) => return Err(self.exceeded(resource)),
        };
        let proposed_total = match combined_usage(self.consumed, proposed_reserved) {
            Ok(proposed) => proposed,
            Err(resource) => return Err(self.exceeded(resource)),
        };
        if let Some(resource) = first_exceeded(proposed_total, self.limits.maximums) {
            return Err(self.exceeded(resource));
        }

        self.reserved = proposed_reserved;
        self.limiting_resource = None;
        Ok(BudgetReservation {
            ledger: self,
            requested,
            previous_reserved,
            active: true,
        })
    }

    /// Atomically charges measured usage against the admitted limits.
    ///
    /// This compatibility operation reserves and commits the same amount.
    /// Prefer [`BudgetLedger::reserve`] when planning can conservatively reserve
    /// before authoritative counters become available.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::BudgetExceeded`] when the proposed
    /// aggregate would overflow or exceed any effective limit.
    pub fn charge(&mut self, charge: BudgetCharge) -> Result<(), ExecutionPolicyError> {
        self.reserve(charge)?.commit(charge)
    }

    /// Allocates a child ledger beneath parent, tool, and optional local caps.
    ///
    /// The parent capacity is reserved before the child can begin. Committing
    /// the allocation replaces that reservation with the child's measured
    /// usage; dropping it releases all reserved capacity.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::BudgetExceeded`] if the parent cannot
    /// reserve the derived child allocation.
    pub fn allocate_child(
        &mut self,
        tool_limits: BudgetLimits,
        local_limits: Option<&ResponseBudget>,
    ) -> Result<BudgetAllocation<'_>, ExecutionPolicyError> {
        let parent_remaining = BudgetLimits::from_maximums(self.remaining());
        let effective = parent_remaining
            .constrained_by(tool_limits)
            .constrained_by_response_budget(local_limits);
        let child = Self::from_limits(effective);
        let reservation = self.reserve(effective.maximums())?;
        Ok(BudgetAllocation { reservation, child })
    }

    fn exceeded(&mut self, resource: BudgetResource) -> ExecutionPolicyError {
        self.limiting_resource = Some(resource);
        ExecutionPolicyError::BudgetExceeded { resource }
    }
}

/// Capacity reserved from a ledger for one unit of work.
///
/// A reservation is linear: consuming it commits or releases once, while
/// dropping it releases automatically.
#[derive(Debug)]
pub struct BudgetReservation<'ledger> {
    ledger: &'ledger mut BudgetLedger,
    requested: BudgetCharge,
    previous_reserved: BudgetCharge,
    active: bool,
}

impl BudgetReservation<'_> {
    /// Returns the capacity held by this reservation.
    #[must_use]
    pub const fn reserved(&self) -> BudgetCharge {
        self.requested
    }

    /// Returns the parent ledger snapshot while this reservation is active.
    #[must_use]
    pub fn snapshot(&self) -> BudgetSnapshot {
        self.ledger.snapshot()
    }

    /// Replaces reserved capacity with measured authoritative usage.
    ///
    /// Unused capacity is released. A measurement cannot exceed its
    /// reservation in any dimension.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::BudgetExceeded`] if measured usage
    /// exceeds the reservation or an authoritative addition overflows.
    pub fn commit(mut self, measured: BudgetCharge) -> Result<(), ExecutionPolicyError> {
        if let Some(resource) = first_exceeded(measured, self.requested) {
            return Err(self.ledger.exceeded(resource));
        }
        let proposed_consumed = match combined_usage(self.ledger.consumed, measured) {
            Ok(proposed) => proposed,
            Err(resource) => return Err(self.ledger.exceeded(resource)),
        };
        let proposed_total = match combined_usage(proposed_consumed, self.previous_reserved) {
            Ok(proposed) => proposed,
            Err(resource) => return Err(self.ledger.exceeded(resource)),
        };
        if let Some(resource) = first_exceeded(proposed_total, self.ledger.limits.maximums) {
            return Err(self.ledger.exceeded(resource));
        }

        self.ledger.consumed = proposed_consumed;
        self.ledger.reserved = self.previous_reserved;
        self.ledger.limiting_resource = None;
        self.active = false;
        Ok(())
    }

    /// Releases the reservation without committing usage.
    pub fn release(mut self) {
        self.restore_previous_reservation();
    }

    fn restore_previous_reservation(&mut self) {
        if self.active {
            self.ledger.reserved = self.previous_reserved;
            self.active = false;
        }
    }
}

impl Drop for BudgetReservation<'_> {
    fn drop(&mut self) {
        self.restore_previous_reservation();
    }
}

/// Child budget allocated from a parent ledger.
///
/// The allocation owns an independently enforceable child ledger while
/// retaining the parent reservation that bounds it.
#[derive(Debug)]
pub struct BudgetAllocation<'parent> {
    reservation: BudgetReservation<'parent>,
    child: BudgetLedger,
}

impl BudgetAllocation<'_> {
    /// Returns the child's effective limits.
    #[must_use]
    pub const fn limits(&self) -> BudgetLimits {
        self.child.limits()
    }

    /// Returns the child ledger.
    #[must_use]
    pub const fn ledger(&self) -> &BudgetLedger {
        &self.child
    }

    /// Returns the child ledger for reservations or charges.
    #[must_use]
    pub const fn ledger_mut(&mut self) -> &mut BudgetLedger {
        &mut self.child
    }

    /// Commits measured child usage to the parent and releases unused capacity.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::BudgetExceeded`] if the child accounting
    /// violates its parent reservation.
    pub fn commit(self) -> Result<BudgetSnapshot, ExecutionPolicyError> {
        let Self { reservation, child } = self;
        let snapshot = child.snapshot();
        reservation.commit(snapshot.consumed())?;
        Ok(snapshot)
    }

    /// Releases the parent allocation without committing child usage.
    pub fn release(self) {
        let Self { reservation, .. } = self;
        reservation.release();
    }
}

/// Request-scoped response profile, budget, and cancellation policy.
#[derive(Debug)]
pub struct ExecutionContext<C> {
    profile: ResponseProfile,
    budget: BudgetLedger,
    cancellation: C,
}

impl<C> ExecutionContext<C>
where
    C: CancellationSignal,
{
    /// Creates one agent execution context.
    #[must_use]
    pub const fn new(profile: ResponseProfile, budget: BudgetLedger, cancellation: C) -> Self {
        Self {
            profile,
            budget,
            cancellation,
        }
    }

    /// Returns the admitted response profile.
    #[must_use]
    pub const fn profile(&self) -> ResponseProfile {
        self.profile
    }

    /// Returns the shared budget ledger.
    #[must_use]
    pub const fn budget(&self) -> &BudgetLedger {
        &self.budget
    }

    /// Returns the shared budget ledger for one atomic charge.
    #[must_use]
    pub const fn budget_mut(&mut self) -> &mut BudgetLedger {
        &mut self.budget
    }

    /// Stops agent work after the caller requests cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::Cancelled`] when cancellation is set.
    pub fn checkpoint(&self) -> Result<(), ExecutionPolicyError> {
        if self.cancellation.is_cancelled() {
            Err(ExecutionPolicyError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Reports whether the request uses the currently implemented compact profile.
#[must_use]
pub const fn is_compact_profile(profile: Option<ResponseProfile>) -> bool {
    matches!(profile, None | Some(ResponseProfile::Compact))
}

const fn minimum_charge(left: BudgetCharge, right: BudgetCharge) -> BudgetCharge {
    BudgetCharge {
        rows: min_u64(left.rows, right.rows),
        results: min_u64(left.results, right.results),
        tokens: min_u64(left.tokens, right.tokens),
        actual_tokens: min_u64(left.actual_tokens, right.actual_tokens),
        source_bytes: min_u64(left.source_bytes, right.source_bytes),
        traversal_facts: min_u64(left.traversal_facts, right.traversal_facts),
        depth: min_u64(left.depth, right.depth),
        paths: min_u64(left.paths, right.paths),
        json_bytes: min_u64(left.json_bytes, right.json_bytes),
        memory_bytes: min_u64(left.memory_bytes, right.memory_bytes),
        time_ms: min_u64(left.time_ms, right.time_ms),
    }
}

const fn min_u64(left: u64, right: u64) -> u64 {
    if left < right { left } else { right }
}

fn combined_usage(left: BudgetCharge, right: BudgetCharge) -> Result<BudgetCharge, BudgetResource> {
    Ok(BudgetCharge {
        rows: left
            .rows
            .checked_add(right.rows)
            .ok_or(BudgetResource::Rows)?,
        results: left
            .results
            .checked_add(right.results)
            .ok_or(BudgetResource::Results)?,
        tokens: left
            .tokens
            .checked_add(right.tokens)
            .ok_or(BudgetResource::Tokens)?,
        actual_tokens: left
            .actual_tokens
            .checked_add(right.actual_tokens)
            .ok_or(BudgetResource::ActualTokens)?,
        source_bytes: left
            .source_bytes
            .checked_add(right.source_bytes)
            .ok_or(BudgetResource::SourceBytes)?,
        traversal_facts: left
            .traversal_facts
            .checked_add(right.traversal_facts)
            .ok_or(BudgetResource::TraversalFacts)?,
        depth: left.depth.max(right.depth),
        paths: left
            .paths
            .checked_add(right.paths)
            .ok_or(BudgetResource::Paths)?,
        json_bytes: left
            .json_bytes
            .checked_add(right.json_bytes)
            .ok_or(BudgetResource::JsonBytes)?,
        memory_bytes: left
            .memory_bytes
            .checked_add(right.memory_bytes)
            .ok_or(BudgetResource::MemoryBytes)?,
        time_ms: left.time_ms.max(right.time_ms),
    })
}

fn remaining_capacity(limits: BudgetCharge, used: BudgetCharge) -> BudgetCharge {
    BudgetCharge {
        rows: limits
            .rows
            .checked_sub(used.rows)
            .expect("admitted rows never exceed their limit"),
        results: limits
            .results
            .checked_sub(used.results)
            .expect("admitted results never exceed their limit"),
        tokens: limits
            .tokens
            .checked_sub(used.tokens)
            .expect("admitted estimated tokens never exceed their limit"),
        actual_tokens: limits
            .actual_tokens
            .checked_sub(used.actual_tokens)
            .expect("admitted actual tokens never exceed their limit"),
        source_bytes: limits
            .source_bytes
            .checked_sub(used.source_bytes)
            .expect("admitted source bytes never exceed their limit"),
        traversal_facts: limits
            .traversal_facts
            .checked_sub(used.traversal_facts)
            .expect("admitted traversal facts never exceed their limit"),
        depth: limits.depth,
        paths: limits
            .paths
            .checked_sub(used.paths)
            .expect("admitted paths never exceed their limit"),
        json_bytes: limits
            .json_bytes
            .checked_sub(used.json_bytes)
            .expect("admitted JSON bytes never exceed their limit"),
        memory_bytes: limits
            .memory_bytes
            .checked_sub(used.memory_bytes)
            .expect("admitted memory bytes never exceed their limit"),
        time_ms: limits.time_ms,
    }
}

fn first_exceeded(used: BudgetCharge, limits: BudgetCharge) -> Option<BudgetResource> {
    if used.rows > limits.rows {
        Some(BudgetResource::Rows)
    } else if used.results > limits.results {
        Some(BudgetResource::Results)
    } else if used.tokens > limits.tokens {
        Some(BudgetResource::Tokens)
    } else if used.actual_tokens > limits.actual_tokens {
        Some(BudgetResource::ActualTokens)
    } else if used.source_bytes > limits.source_bytes {
        Some(BudgetResource::SourceBytes)
    } else if used.traversal_facts > limits.traversal_facts {
        Some(BudgetResource::TraversalFacts)
    } else if used.depth > limits.depth {
        Some(BudgetResource::Depth)
    } else if used.paths > limits.paths {
        Some(BudgetResource::Paths)
    } else if used.json_bytes > limits.json_bytes {
        Some(BudgetResource::JsonBytes)
    } else if used.memory_bytes > limits.memory_bytes {
        Some(BudgetResource::MemoryBytes)
    } else if used.time_ms > limits.time_ms {
        Some(BudgetResource::Time)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[derive(Debug, Clone, Copy)]
    struct Cancelled;

    impl CancellationSignal for Cancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    fn response_budget(maximums: BudgetCharge) -> ResponseBudget {
        ResponseBudget {
            max_results: u16::try_from(maximums.results).ok(),
            max_tokens: u16::try_from(maximums.tokens).ok(),
            max_source_bytes: u32::try_from(maximums.source_bytes).ok(),
            max_traversal_facts: u32::try_from(maximums.traversal_facts).ok(),
            max_depth: u8::try_from(maximums.depth).ok(),
            max_paths: u16::try_from(maximums.paths).ok(),
            timeout_ms: u32::try_from(maximums.time_ms).ok(),
            evidence_level: None,
        }
    }

    fn charge_for(resource: BudgetResource, value: u64) -> BudgetCharge {
        let mut charge = BudgetCharge::default();
        match resource {
            BudgetResource::Rows => charge.rows = value,
            BudgetResource::Results => charge.results = value,
            BudgetResource::Tokens => charge.tokens = value,
            BudgetResource::ActualTokens => charge.actual_tokens = value,
            BudgetResource::SourceBytes => charge.source_bytes = value,
            BudgetResource::TraversalFacts => charge.traversal_facts = value,
            BudgetResource::Depth => charge.depth = value,
            BudgetResource::Paths => charge.paths = value,
            BudgetResource::JsonBytes => charge.json_bytes = value,
            BudgetResource::MemoryBytes => charge.memory_bytes = value,
            BudgetResource::Time => charge.time_ms = value,
        }
        charge
    }

    #[test]
    fn omitted_budget_uses_total_server_ceiling() {
        let ledger = BudgetLedger::new(None);

        assert_eq!(ledger.limits(), BudgetLimits::server_ceiling());
        assert_eq!(
            ledger.remaining(),
            BudgetLimits::server_ceiling().maximums()
        );
    }

    #[test]
    fn server_ceiling_tracks_authoritative_query_limits() {
        let query = QueryBudget::new();
        let maximums = BudgetLimits::server_ceiling().maximums();

        assert_eq!(maximums.rows, query.max_rows());
        assert_eq!(maximums.results, query.max_results());
        assert_eq!(maximums.tokens, query.max_tokens());
        assert_eq!(maximums.actual_tokens, query.max_json_bytes());
        assert_eq!(maximums.source_bytes, query.max_source_bytes());
        assert_eq!(maximums.traversal_facts, query.max_edges());
        assert_eq!(maximums.paths, query.max_results());
        assert_eq!(maximums.json_bytes, query.max_json_bytes());
        assert_eq!(maximums.memory_bytes, query.max_memory_bytes());
        assert_eq!(
            maximums.time_ms,
            u64::try_from(query.max_duration().as_millis())
                .expect("query default duration is representable in milliseconds")
        );
    }

    #[test]
    fn client_fields_only_reduce_server_limits() {
        let requested = response_budget(BudgetCharge {
            rows: u64::MAX,
            results: 2_000,
            tokens: 100,
            actual_tokens: 100,
            source_bytes: 1_000_000,
            traversal_facts: 50,
            depth: 32,
            paths: 5,
            json_bytes: u64::MAX,
            memory_bytes: u64::MAX,
            time_ms: 60_000,
        });
        let limits = BudgetLedger::new(Some(requested)).limits().maximums();

        assert_eq!(
            limits,
            BudgetCharge {
                rows: 250_000,
                results: 1_000,
                tokens: 100,
                actual_tokens: 1_048_576,
                source_bytes: 65_536,
                traversal_facts: 50,
                depth: 16,
                paths: 5,
                json_bytes: 1_048_576,
                memory_bytes: 16_777_216,
                time_ms: 2_000,
            }
        );
    }

    #[test]
    fn token_limit_reduces_only_estimate_without_raising_server_policy() {
        let reduced = BudgetLedger::with_token_limit(40).limits().maximums();
        assert_eq!(reduced.tokens, 40);
        assert_eq!(
            reduced.actual_tokens,
            BudgetLimits::server_ceiling().maximums().actual_tokens
        );

        let attempted_raise = BudgetLedger::with_token_limit(u64::MAX).limits().maximums();
        assert_eq!(attempted_raise, BudgetLimits::server_ceiling().maximums());
    }

    #[test]
    fn exact_limit_is_accepted_and_one_above_is_atomic() {
        let limits = BudgetLimits::from_maximums(BudgetCharge {
            rows: 1,
            results: 1,
            tokens: 1,
            actual_tokens: 1,
            source_bytes: 1,
            traversal_facts: 1,
            depth: 1,
            paths: 1,
            json_bytes: 1,
            memory_bytes: 1,
            time_ms: 1,
        });
        let mut ledger = BudgetLedger::from_limits(limits);
        ledger
            .charge(limits.maximums())
            .expect("exact aggregate limit is admitted");
        let before = ledger.snapshot();

        assert_eq!(
            ledger.charge(BudgetCharge {
                results: 1,
                ..BudgetCharge::default()
            }),
            Err(ExecutionPolicyError::BudgetExceeded {
                resource: BudgetResource::Results,
            })
        );
        assert_eq!(ledger.consumed(), before.consumed());
        assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
        assert_eq!(
            ledger.snapshot().limiting_resource(),
            Some(BudgetResource::Results)
        );
    }

    #[test]
    fn zero_limit_rejects_first_nonzero_charge() {
        let mut ledger =
            BudgetLedger::from_limits(BudgetLimits::from_maximums(BudgetCharge::default()));

        assert_eq!(
            ledger.charge(BudgetCharge {
                tokens: 1,
                ..BudgetCharge::default()
            }),
            Err(ExecutionPolicyError::BudgetExceeded {
                resource: BudgetResource::Tokens,
            })
        );
        assert_eq!(ledger.consumed(), BudgetCharge::default());
    }

    #[test]
    fn every_dimension_accepts_below_and_exact_but_rejects_one_above() {
        let resources = [
            BudgetResource::Rows,
            BudgetResource::Results,
            BudgetResource::Tokens,
            BudgetResource::ActualTokens,
            BudgetResource::SourceBytes,
            BudgetResource::TraversalFacts,
            BudgetResource::Depth,
            BudgetResource::Paths,
            BudgetResource::JsonBytes,
            BudgetResource::MemoryBytes,
            BudgetResource::Time,
        ];

        for resource in resources {
            let limits = BudgetLimits::from_maximums(charge_for(resource, 2));
            for accepted in [1, 2] {
                BudgetLedger::from_limits(limits)
                    .charge(charge_for(resource, accepted))
                    .expect("value at or below the exact limit is admitted");
            }
            let mut rejected = BudgetLedger::from_limits(limits);
            assert_eq!(
                rejected.charge(charge_for(resource, 3)),
                Err(ExecutionPolicyError::BudgetExceeded { resource })
            );
            assert_eq!(rejected.consumed(), BudgetCharge::default());
        }
    }

    #[test]
    fn constrained_and_remaining_include_measured_output_dimensions() {
        let broad = BudgetLimits::from_maximums(BudgetCharge {
            actual_tokens: 100,
            json_bytes: 200,
            memory_bytes: 300,
            ..BudgetCharge::default()
        });
        let narrow = BudgetLimits::from_maximums(BudgetCharge {
            actual_tokens: 80,
            json_bytes: 180,
            memory_bytes: 250,
            ..BudgetCharge::default()
        });
        let mut ledger = BudgetLedger::from_limits(broad.constrained_by(narrow));
        ledger
            .charge(BudgetCharge {
                actual_tokens: 30,
                json_bytes: 60,
                memory_bytes: 70,
                ..BudgetCharge::default()
            })
            .expect("measured output usage fits");

        assert_eq!(ledger.remaining().actual_tokens, 50);
        assert_eq!(ledger.remaining().json_bytes, 120);
        assert_eq!(ledger.remaining().memory_bytes, 180);
    }

    #[test]
    fn checked_overflow_reports_each_additive_resource_without_mutation() {
        let resources = [
            BudgetResource::Rows,
            BudgetResource::Results,
            BudgetResource::Tokens,
            BudgetResource::ActualTokens,
            BudgetResource::SourceBytes,
            BudgetResource::TraversalFacts,
            BudgetResource::Paths,
            BudgetResource::JsonBytes,
            BudgetResource::MemoryBytes,
        ];

        for resource in resources {
            let limits = BudgetLimits::from_maximums(charge_for(resource, u64::MAX));
            let mut ledger = BudgetLedger::from_limits(limits);
            ledger
                .charge(charge_for(resource, u64::MAX))
                .expect("maximum counter value is representable");
            let before = ledger.consumed();

            assert_eq!(
                ledger.charge(charge_for(resource, 1)),
                Err(ExecutionPolicyError::BudgetExceeded { resource })
            );
            assert_eq!(ledger.consumed(), before);
        }
    }

    #[test]
    fn stable_precedence_selects_the_same_resource_for_multi_limit_failure() {
        let mut ledger =
            BudgetLedger::from_limits(BudgetLimits::from_maximums(BudgetCharge::default()));
        let charge = BudgetCharge {
            rows: 1,
            results: 1,
            tokens: 1,
            actual_tokens: 1,
            source_bytes: 1,
            traversal_facts: 1,
            depth: 1,
            paths: 1,
            json_bytes: 1,
            memory_bytes: 1,
            time_ms: 1,
        };

        for _ in 0..3 {
            assert_eq!(
                ledger.charge(charge),
                Err(ExecutionPolicyError::BudgetExceeded {
                    resource: BudgetResource::Rows,
                })
            );
        }
    }

    #[test]
    fn stable_precedence_orders_new_measured_resources() {
        let limits = BudgetLimits::from_maximums(BudgetCharge::default());
        for (charge, expected) in [
            (
                BudgetCharge {
                    tokens: 1,
                    actual_tokens: 1,
                    ..BudgetCharge::default()
                },
                BudgetResource::Tokens,
            ),
            (
                BudgetCharge {
                    actual_tokens: 1,
                    source_bytes: 1,
                    ..BudgetCharge::default()
                },
                BudgetResource::ActualTokens,
            ),
            (
                BudgetCharge {
                    json_bytes: 1,
                    memory_bytes: 1,
                    time_ms: 1,
                    ..BudgetCharge::default()
                },
                BudgetResource::JsonBytes,
            ),
            (
                BudgetCharge {
                    memory_bytes: 1,
                    time_ms: 1,
                    ..BudgetCharge::default()
                },
                BudgetResource::MemoryBytes,
            ),
        ] {
            let mut ledger = BudgetLedger::from_limits(limits);
            assert_eq!(
                ledger.charge(charge),
                Err(ExecutionPolicyError::BudgetExceeded { resource: expected })
            );
        }
    }

    #[test]
    fn reservation_reduces_remaining_and_release_restores_it() {
        let mut ledger = BudgetLedger::with_token_limit(500);
        let original = ledger.remaining();
        let reservation = ledger
            .reserve(BudgetCharge {
                tokens: 200,
                ..BudgetCharge::default()
            })
            .expect("reservation fits");

        assert_eq!(reservation.snapshot().reserved().tokens, 200);
        assert_eq!(reservation.snapshot().remaining().tokens, 300);
        reservation.release();

        assert_eq!(ledger.remaining(), original);
        assert_eq!(ledger.consumed(), BudgetCharge::default());
    }

    #[test]
    fn reservation_commit_releases_unused_capacity() {
        let mut ledger = BudgetLedger::with_token_limit(500);
        ledger
            .reserve(BudgetCharge {
                tokens: 400,
                ..BudgetCharge::default()
            })
            .expect("reservation fits")
            .commit(BudgetCharge {
                tokens: 125,
                ..BudgetCharge::default()
            })
            .expect("measured usage fits reservation");

        assert_eq!(ledger.consumed().tokens, 125);
        assert_eq!(ledger.remaining().tokens, 375);
        assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
    }

    #[test]
    fn rejected_commit_releases_reservation_and_preserves_consumed_usage() {
        let mut ledger = BudgetLedger::with_token_limit(500);
        ledger
            .charge(BudgetCharge {
                tokens: 100,
                ..BudgetCharge::default()
            })
            .expect("initial usage fits");
        let reservation = ledger
            .reserve(BudgetCharge {
                tokens: 200,
                ..BudgetCharge::default()
            })
            .expect("reservation fits");

        assert_eq!(
            reservation.commit(BudgetCharge {
                tokens: 201,
                ..BudgetCharge::default()
            }),
            Err(ExecutionPolicyError::BudgetExceeded {
                resource: BudgetResource::Tokens,
            })
        );
        assert_eq!(ledger.consumed().tokens, 100);
        assert_eq!(ledger.snapshot().reserved(), BudgetCharge::default());
    }

    #[test]
    fn depth_and_wall_time_are_high_water_marks() {
        let mut ledger = BudgetLedger::new(None);
        ledger
            .charge(BudgetCharge {
                results: 2,
                depth: 4,
                time_ms: 20,
                ..BudgetCharge::default()
            })
            .expect("first charge fits");
        ledger
            .charge(BudgetCharge {
                results: 3,
                depth: 2,
                time_ms: 10,
                ..BudgetCharge::default()
            })
            .expect("second charge fits");

        assert_eq!(ledger.consumed().results, 5);
        assert_eq!(ledger.consumed().depth, 4);
        assert_eq!(ledger.consumed().time_ms, 20);
        assert_eq!(ledger.remaining().depth, 16);
        assert_eq!(ledger.remaining().time_ms, 2_000);
    }

    #[test]
    fn child_allocation_is_bounded_and_commits_only_measured_usage() {
        let parent_limits = BudgetLimits::from_maximums(BudgetCharge {
            rows: 30,
            results: 20,
            tokens: 100,
            actual_tokens: 100,
            source_bytes: 200,
            traversal_facts: 300,
            depth: 8,
            paths: 10,
            json_bytes: 400,
            memory_bytes: 500,
            time_ms: 1_000,
        });
        let tool_limits = BudgetLimits::from_maximums(BudgetCharge {
            rows: 25,
            results: 10,
            tokens: 90,
            actual_tokens: 85,
            source_bytes: 150,
            traversal_facts: 250,
            depth: 6,
            paths: 8,
            json_bytes: 350,
            memory_bytes: 450,
            time_ms: 900,
        });
        let local = response_budget(BudgetCharge {
            rows: u64::MAX,
            results: 9,
            tokens: 80,
            actual_tokens: 80,
            source_bytes: 140,
            traversal_facts: 240,
            depth: 5,
            paths: 7,
            json_bytes: 300,
            memory_bytes: 400,
            time_ms: 800,
        });
        let mut parent = BudgetLedger::from_limits(parent_limits);
        parent
            .charge(BudgetCharge {
                tokens: 25,
                actual_tokens: 20,
                json_bytes: 10,
                memory_bytes: 20,
                ..BudgetCharge::default()
            })
            .expect("parent setup charge fits");

        let mut child = parent
            .allocate_child(tool_limits, Some(&local))
            .expect("bounded child allocation fits");
        assert_eq!(
            child.limits().maximums(),
            BudgetCharge {
                rows: 25,
                results: 9,
                tokens: 75,
                actual_tokens: 80,
                source_bytes: 140,
                traversal_facts: 240,
                depth: 5,
                paths: 7,
                json_bytes: 350,
                memory_bytes: 450,
                time_ms: 800,
            }
        );
        child
            .ledger_mut()
            .charge(BudgetCharge {
                results: 2,
                tokens: 30,
                actual_tokens: 25,
                depth: 3,
                json_bytes: 100,
                memory_bytes: 150,
                time_ms: 100,
                ..BudgetCharge::default()
            })
            .expect("child usage fits");
        let child_snapshot = child.commit().expect("child usage fits parent reservation");

        assert_eq!(child_snapshot.consumed().tokens, 30);
        assert_eq!(parent.consumed().tokens, 55);
        assert_eq!(parent.consumed().actual_tokens, 45);
        assert_eq!(parent.consumed().results, 2);
        assert_eq!(parent.consumed().json_bytes, 110);
        assert_eq!(parent.consumed().memory_bytes, 170);
        assert_eq!(parent.snapshot().reserved(), BudgetCharge::default());
    }

    #[test]
    fn dropped_child_allocation_releases_parent_capacity() {
        let mut parent = BudgetLedger::with_token_limit(100);
        let original = parent.remaining();
        {
            let child = parent
                .allocate_child(BudgetLimits::server_ceiling(), None)
                .expect("child allocation fits");
            assert_eq!(child.ledger().remaining().tokens, 100);
        }

        assert_eq!(parent.remaining(), original);
        assert_eq!(parent.consumed(), BudgetCharge::default());
    }

    #[test]
    fn execution_context_owns_profile_budget_and_cancellation() {
        let context = ExecutionContext::new(
            ResponseProfile::Evidence,
            BudgetLedger::with_token_limit(500),
            Cancelled,
        );

        assert_eq!(context.profile(), ResponseProfile::Evidence);
        assert_eq!(context.checkpoint(), Err(ExecutionPolicyError::Cancelled));
    }

    #[test]
    fn compact_profile_accepts_default_or_explicit_compact_only() {
        assert!(is_compact_profile(None));
        assert!(is_compact_profile(Some(ResponseProfile::Compact)));
        assert!(!is_compact_profile(Some(ResponseProfile::Standard)));
        assert!(!is_compact_profile(Some(ResponseProfile::Evidence)));
    }

    proptest! {
        #[test]
        fn child_limits_never_exceed_parent_tool_or_local_caps(
            parent_results in 0_u16..=1_000,
            parent_tokens in 0_u16..=16_000,
            parent_source in 0_u32..=524_288,
            parent_facts in 0_u32..=100_000,
            parent_depth in 0_u8..=16,
            parent_paths in 0_u16..=1_000,
            parent_time in 0_u32..=30_000,
            tool_results in 0_u16..=1_000,
            tool_tokens in 0_u16..=16_000,
            tool_source in 0_u32..=524_288,
            tool_facts in 0_u32..=100_000,
            tool_depth in 0_u8..=16,
            tool_paths in 0_u16..=1_000,
            tool_time in 0_u32..=30_000,
            local_results in 0_u16..=1_000,
            local_tokens in 0_u16..=16_000,
            local_source in 0_u32..=524_288,
            local_facts in 0_u32..=100_000,
            local_depth in 0_u8..=16,
            local_paths in 0_u16..=1_000,
            local_time in 0_u32..=30_000,
        ) {
            let parent_charge = BudgetCharge {
                rows: u64::from(parent_facts),
                results: u64::from(parent_results),
                tokens: u64::from(parent_tokens),
                actual_tokens: u64::from(parent_tokens),
                source_bytes: u64::from(parent_source),
                traversal_facts: u64::from(parent_facts),
                depth: u64::from(parent_depth),
                paths: u64::from(parent_paths),
                json_bytes: u64::from(parent_source),
                memory_bytes: u64::from(parent_source),
                time_ms: u64::from(parent_time),
            };
            let tool_charge = BudgetCharge {
                rows: u64::from(tool_facts),
                results: u64::from(tool_results),
                tokens: u64::from(tool_tokens),
                actual_tokens: u64::from(tool_tokens),
                source_bytes: u64::from(tool_source),
                traversal_facts: u64::from(tool_facts),
                depth: u64::from(tool_depth),
                paths: u64::from(tool_paths),
                json_bytes: u64::from(tool_source),
                memory_bytes: u64::from(tool_source),
                time_ms: u64::from(tool_time),
            };
            let local_charge = BudgetCharge {
                rows: u64::MAX,
                results: u64::from(local_results),
                tokens: u64::from(local_tokens),
                actual_tokens: u64::MAX,
                source_bytes: u64::from(local_source),
                traversal_facts: u64::from(local_facts),
                depth: u64::from(local_depth),
                paths: u64::from(local_paths),
                json_bytes: u64::MAX,
                memory_bytes: u64::MAX,
                time_ms: u64::from(local_time),
            };
            let mut parent =
                BudgetLedger::from_limits(BudgetLimits::from_maximums(parent_charge));
            parent
                .charge(BudgetCharge {
                    rows: parent_charge.rows / 2,
                    results: parent_charge.results / 2,
                    tokens: parent_charge.tokens / 2,
                    actual_tokens: parent_charge.actual_tokens / 2,
                    source_bytes: parent_charge.source_bytes / 2,
                    traversal_facts: parent_charge.traversal_facts / 2,
                    depth: parent_charge.depth / 2,
                    paths: parent_charge.paths / 2,
                    json_bytes: parent_charge.json_bytes / 2,
                    memory_bytes: parent_charge.memory_bytes / 2,
                    time_ms: parent_charge.time_ms / 2,
                })
                .expect("deterministic parent setup charge is within its limits");
            let parent_remaining = parent.remaining();
            let local = response_budget(local_charge);
            let child = parent
                .allocate_child(BudgetLimits::from_maximums(tool_charge), Some(&local))
                .expect("component-wise minimum always fits the parent");
            let effective = child.limits().maximums();

            prop_assert_eq!(
                effective,
                minimum_charge(minimum_charge(parent_remaining, tool_charge), local_charge)
            );
        }

        #[test]
        fn successful_additive_charges_equal_checked_component_sums(
            first in 0_u32..=1_000_000,
            second in 0_u32..=1_000_000,
        ) {
            let maximum = u64::from(first) + u64::from(second);
            let limits = BudgetLimits::from_maximums(BudgetCharge {
                rows: maximum,
                tokens: maximum,
                actual_tokens: maximum,
                json_bytes: maximum,
                memory_bytes: maximum,
                ..BudgetCharge::default()
            });
            let mut ledger = BudgetLedger::from_limits(limits);
            ledger
                .charge(BudgetCharge {
                    rows: u64::from(first),
                    tokens: u64::from(first),
                    actual_tokens: u64::from(first),
                    json_bytes: u64::from(first),
                    memory_bytes: u64::from(first),
                    ..BudgetCharge::default()
                })
                .expect("first charge is within constructed limit");
            ledger
                .charge(BudgetCharge {
                    rows: u64::from(second),
                    tokens: u64::from(second),
                    actual_tokens: u64::from(second),
                    json_bytes: u64::from(second),
                    memory_bytes: u64::from(second),
                    ..BudgetCharge::default()
                })
                .expect("aggregate charge equals constructed limit");

            prop_assert_eq!(ledger.consumed().rows, maximum);
            prop_assert_eq!(ledger.consumed().tokens, maximum);
            prop_assert_eq!(ledger.consumed().actual_tokens, maximum);
            prop_assert_eq!(ledger.consumed().json_bytes, maximum);
            prop_assert_eq!(ledger.consumed().memory_bytes, maximum);
            prop_assert_eq!(ledger.remaining().rows, 0);
            prop_assert_eq!(ledger.remaining().tokens, 0);
            prop_assert_eq!(ledger.remaining().actual_tokens, 0);
            prop_assert_eq!(ledger.remaining().json_bytes, 0);
            prop_assert_eq!(ledger.remaining().memory_bytes, 0);
        }
    }
}
