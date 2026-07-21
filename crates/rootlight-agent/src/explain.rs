//! Source-free plan construction for explain mode.
//!
//! Explain mode returns the bounded plan (operators, applied limits, estimated
//! cost) without executing retrieval, so a client can audit what would run
//! before spending work. Plan construction is deterministic for a normalized
//! request and never reads repository source.

use rootlight_mcp_contract::context::PlanExplanation;

/// Estimated cost units per planned match for `code.locate`.
const LOCATE_COST_PER_RESULT: u64 = 8;

/// Estimated cost units per planned symbol for `symbol.explain`.
const EXPLAIN_COST_PER_SYMBOL: u64 = 12;

/// Estimated cost units per planned source reference for `source.read`.
const READ_COST_PER_REFERENCE: u64 = 16;

/// Builds the source-free `code.locate` plan for explain mode.
///
/// `exact` selects an index lookup (exact identifier) versus a lexical scan;
/// `max_results` bounds the planned work and drives the cost estimate.
#[must_use]
pub fn code_locate_plan(exact: bool, max_results: u32) -> PlanExplanation {
    let operator = if exact {
        "index_lookup"
    } else {
        "lexical_scan"
    };
    PlanExplanation {
        estimated_cost: u64::from(max_results).saturating_mul(LOCATE_COST_PER_RESULT),
        operators: vec![operator.to_owned()],
        applied_limits: vec![format!("max_results: {max_results}")],
    }
}

/// Builds the source-free `symbol.explain` plan for explain mode.
///
/// `symbol_count` bounds the planned work and drives the cost estimate.
#[must_use]
pub fn symbol_explain_plan(symbol_count: usize) -> PlanExplanation {
    let cost = u64::try_from(symbol_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(EXPLAIN_COST_PER_SYMBOL);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["symbol_lookup".to_owned()],
        applied_limits: vec![format!("symbols: {symbol_count}")],
    }
}

/// Builds the source-free `source.read` plan for explain mode.
///
/// `reference_count` bounds the planned work and drives the cost estimate.
#[must_use]
pub fn source_read_plan(reference_count: usize) -> PlanExplanation {
    let cost = u64::try_from(reference_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(READ_COST_PER_REFERENCE);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["source_read".to_owned()],
        applied_limits: vec![format!("references: {reference_count}")],
    }
}

#[cfg(test)]
mod tests {
    use super::code_locate_plan;

    #[test]
    fn plan_is_deterministic_for_the_same_request() {
        assert_eq!(code_locate_plan(false, 20), code_locate_plan(false, 20));
        assert_eq!(code_locate_plan(true, 20), code_locate_plan(true, 20));
    }

    #[test]
    fn plan_reflects_mode_and_limits() {
        let lexical = code_locate_plan(false, 20);
        assert_eq!(lexical.operators, vec!["lexical_scan".to_owned()]);
        assert_eq!(lexical.applied_limits, vec!["max_results: 20".to_owned()]);
        let exact = code_locate_plan(true, 5);
        assert_eq!(exact.operators, vec!["index_lookup".to_owned()]);
        assert!(exact.estimated_cost < lexical.estimated_cost);
    }

    #[test]
    fn symbol_explain_plan_is_deterministic_and_bounded() {
        use super::symbol_explain_plan;
        assert_eq!(symbol_explain_plan(3), symbol_explain_plan(3));
        let plan = symbol_explain_plan(3);
        assert_eq!(plan.operators, vec!["symbol_lookup".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["symbols: 3".to_owned()]);
    }

    #[test]
    fn source_read_plan_is_deterministic_and_bounded() {
        use super::source_read_plan;
        assert_eq!(source_read_plan(2), source_read_plan(2));
        let plan = source_read_plan(2);
        assert_eq!(plan.operators, vec!["source_read".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["references: 2".to_owned()]);
    }
}
