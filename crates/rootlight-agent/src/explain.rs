//! Source-free plan construction for explain mode.
//!
//! Explain mode returns the bounded plan (operators, applied limits, estimated
//! cost) without executing retrieval, so a client can audit what would run
//! before spending work. Plan construction is deterministic for a normalized
//! request and never reads repository source.

use rootlight_mcp_contract::context::PlanExplanation;

/// Estimated cost units per planned match for `code.locate`.
const LOCATE_COST_PER_RESULT: u64 = 8;

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
}
