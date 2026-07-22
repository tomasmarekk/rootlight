//! Shared execution policy for transport-neutral agent orchestration.
//!
//! Agent planners receive cancellation and budget policy through this module
//! instead of reaching into a JSON-RPC runtime or application-owned state.

use rootlight_mcp_contract::vertical::{ResponseBudget, ResponseProfile};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BudgetResource {
    /// Returned result objects.
    Results,
    /// Estimated output tokens.
    Tokens,
    /// Raw source bytes.
    SourceBytes,
    /// Relationship or traversal facts.
    TraversalFacts,
    /// Plan or traversal depth.
    Depth,
    /// Independently returned paths.
    Paths,
    /// Cooperative elapsed time in milliseconds.
    Time,
}

/// One additive resource charge made by an agent planner or shaper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetCharge {
    /// Returned result objects.
    pub results: u64,
    /// Estimated output tokens.
    pub tokens: u64,
    /// Raw source bytes.
    pub source_bytes: u64,
    /// Relationship or traversal facts.
    pub traversal_facts: u64,
    /// Maximum plan or traversal depth reached by this charge.
    pub depth: u64,
    /// Independently returned paths.
    pub paths: u64,
    /// Cooperative elapsed time in milliseconds.
    pub time_ms: u64,
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

/// Additive shared budget ledger for nested agent orchestration.
///
/// A child operation charges the same ledger as its parent. Limits are copied
/// from the public request once at admission, and every charge is atomic: a
/// rejected charge leaves all counters unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetLedger {
    limits: BudgetLimits,
    consumed: BudgetCharge,
}

impl BudgetLedger {
    /// Creates a ledger from optional caller-provided limits.
    #[must_use]
    pub const fn new(limits: Option<ResponseBudget>) -> Self {
        let limits = match limits {
            Some(limits) => BudgetLimits {
                results: optional_u16(limits.max_results),
                tokens: optional_u16(limits.max_tokens),
                source_bytes: optional_u32(limits.max_source_bytes),
                traversal_facts: optional_u32(limits.max_traversal_facts),
                depth: optional_u8(limits.max_depth),
                paths: optional_u16(limits.max_paths),
                time_ms: optional_u32(limits.timeout_ms),
            },
            None => BudgetLimits::unlimited(),
        };
        Self {
            limits,
            consumed: BudgetCharge {
                results: 0,
                tokens: 0,
                source_bytes: 0,
                traversal_facts: 0,
                depth: 0,
                paths: 0,
                time_ms: 0,
            },
        }
    }

    /// Creates a ledger whose only limit is an agent-specific token ceiling.
    ///
    /// This constructor accepts `u64` because domain operations such as
    /// `context.pack` have dedicated ceilings that differ from the shared
    /// [`ResponseBudget`] schema.
    #[must_use]
    pub const fn with_token_limit(max_tokens: u64) -> Self {
        Self {
            limits: BudgetLimits {
                results: None,
                tokens: Some(max_tokens),
                source_bytes: None,
                traversal_facts: None,
                depth: None,
                paths: None,
                time_ms: None,
            },
            consumed: BudgetCharge {
                results: 0,
                tokens: 0,
                source_bytes: 0,
                traversal_facts: 0,
                depth: 0,
                paths: 0,
                time_ms: 0,
            },
        }
    }

    /// Returns all resources committed to this ledger.
    #[must_use]
    pub const fn consumed(&self) -> BudgetCharge {
        self.consumed
    }

    /// Atomically charges resource usage against the admitted limits.
    ///
    /// Depth is a maximum rather than an additive counter. Every other field
    /// is accumulated with saturation before limits are checked.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionPolicyError::BudgetExceeded`] when the proposed
    /// aggregate would exceed any configured limit.
    pub fn charge(&mut self, charge: BudgetCharge) -> Result<(), ExecutionPolicyError> {
        let proposed = BudgetCharge {
            results: self.consumed.results.saturating_add(charge.results),
            tokens: self.consumed.tokens.saturating_add(charge.tokens),
            source_bytes: self
                .consumed
                .source_bytes
                .saturating_add(charge.source_bytes),
            traversal_facts: self
                .consumed
                .traversal_facts
                .saturating_add(charge.traversal_facts),
            depth: self.consumed.depth.max(charge.depth),
            paths: self.consumed.paths.saturating_add(charge.paths),
            time_ms: self.consumed.time_ms.saturating_add(charge.time_ms),
        };
        self.check(proposed)?;
        self.consumed = proposed;
        Ok(())
    }

    fn check(&self, proposed: BudgetCharge) -> Result<(), ExecutionPolicyError> {
        check_limit(
            proposed.results,
            self.limits.results,
            BudgetResource::Results,
        )?;
        check_limit(proposed.tokens, self.limits.tokens, BudgetResource::Tokens)?;
        check_limit(
            proposed.source_bytes,
            self.limits.source_bytes,
            BudgetResource::SourceBytes,
        )?;
        check_limit(
            proposed.traversal_facts,
            self.limits.traversal_facts,
            BudgetResource::TraversalFacts,
        )?;
        check_limit(proposed.depth, self.limits.depth, BudgetResource::Depth)?;
        check_limit(proposed.paths, self.limits.paths, BudgetResource::Paths)?;
        check_limit(proposed.time_ms, self.limits.time_ms, BudgetResource::Time)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BudgetLimits {
    results: Option<u64>,
    tokens: Option<u64>,
    source_bytes: Option<u64>,
    traversal_facts: Option<u64>,
    depth: Option<u64>,
    paths: Option<u64>,
    time_ms: Option<u64>,
}

impl BudgetLimits {
    const fn unlimited() -> Self {
        Self {
            results: None,
            tokens: None,
            source_bytes: None,
            traversal_facts: None,
            depth: None,
            paths: None,
            time_ms: None,
        }
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

fn check_limit(
    used: u64,
    limit: Option<u64>,
    resource: BudgetResource,
) -> Result<(), ExecutionPolicyError> {
    if limit.is_some_and(|limit| used > limit) {
        Err(ExecutionPolicyError::BudgetExceeded { resource })
    } else {
        Ok(())
    }
}

const fn optional_u8(value: Option<u8>) -> Option<u64> {
    match value {
        Some(value) => Some(value as u64),
        None => None,
    }
}

const fn optional_u16(value: Option<u16>) -> Option<u64> {
    match value {
        Some(value) => Some(value as u64),
        None => None,
    }
}

const fn optional_u32(value: Option<u32>) -> Option<u64> {
    match value {
        Some(value) => Some(value as u64),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct Cancelled;

    impl CancellationSignal for Cancelled {
        fn is_cancelled(&self) -> bool {
            true
        }
    }

    #[test]
    fn rejected_charge_does_not_mutate_the_ledger() {
        let mut ledger = BudgetLedger::with_token_limit(500);
        ledger
            .charge(BudgetCharge {
                tokens: 400,
                ..BudgetCharge::default()
            })
            .expect("initial charge fits");

        assert_eq!(
            ledger.charge(BudgetCharge {
                tokens: 101,
                ..BudgetCharge::default()
            }),
            Err(ExecutionPolicyError::BudgetExceeded {
                resource: BudgetResource::Tokens
            })
        );
        assert_eq!(ledger.consumed().tokens, 400);
    }

    #[test]
    fn depth_uses_the_maximum_while_other_resources_accumulate() {
        let mut ledger = BudgetLedger::new(None);
        ledger
            .charge(BudgetCharge {
                results: 2,
                depth: 4,
                ..BudgetCharge::default()
            })
            .expect("unlimited ledger accepts charge");
        ledger
            .charge(BudgetCharge {
                results: 3,
                depth: 2,
                ..BudgetCharge::default()
            })
            .expect("unlimited ledger accepts charge");

        assert_eq!(ledger.consumed().results, 5);
        assert_eq!(ledger.consumed().depth, 4);
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
}
