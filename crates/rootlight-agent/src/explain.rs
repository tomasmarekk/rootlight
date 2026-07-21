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

/// Estimated cost units per planned seed for `symbol.relationships`.
const RELATIONSHIPS_COST_PER_SEED: u64 = 24;

/// Estimated cost units per planned traversal depth for `flow.trace`.
const TRACE_COST_PER_DEPTH: u64 = 32;

/// Estimated cost units per planned changed input for `change.impact`.
const IMPACT_COST_PER_CHANGE: u64 = 40;

/// Estimated cost units per planned selected test for `tests.select`.
const TESTS_COST_PER_TEST: u64 = 6;

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

/// Builds the source-free `symbol.relationships` plan for explain mode.
///
/// `seed_count` and `max_results` bound the planned neighborhood expansion and
/// drive the cost estimate.
#[must_use]
pub fn symbol_relationships_plan(seed_count: usize, max_results: Option<u32>) -> PlanExplanation {
    let cost = u64::try_from(seed_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(RELATIONSHIPS_COST_PER_SEED);
    let mut applied_limits = vec![format!("seeds: {seed_count}")];
    if let Some(max) = max_results {
        applied_limits.push(format!("max_results: {max}"));
    }
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["relationship_expansion".to_owned()],
        applied_limits,
    }
}

/// Builds the source-free `flow.trace` plan for explain mode.
///
/// `max_depth` and `max_paths` bound the planned traversal and drive the cost
/// estimate.
#[must_use]
pub fn flow_trace_plan(max_depth: Option<u8>, max_paths: Option<u16>) -> PlanExplanation {
    let depth = u64::from(max_depth.unwrap_or(8));
    let cost = depth.saturating_mul(TRACE_COST_PER_DEPTH);
    let mut applied_limits = vec![format!("max_depth: {depth}")];
    if let Some(paths) = max_paths {
        applied_limits.push(format!("max_paths: {paths}"));
    }
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["path_traversal".to_owned()],
        applied_limits,
    }
}

/// Builds the source-free `change.impact` plan for explain mode.
///
/// `changed_count` bounds the planned impact analysis and drives the cost
/// estimate.
#[must_use]
pub fn change_impact_plan(changed_count: usize) -> PlanExplanation {
    let cost = u64::try_from(changed_count)
        .unwrap_or(u64::MAX)
        .saturating_mul(IMPACT_COST_PER_CHANGE);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["change_analysis".to_owned()],
        applied_limits: vec![format!("changed_inputs: {changed_count}")],
    }
}

/// Builds the source-free `tests.select` plan for explain mode.
///
/// `max_tests` bounds the planned selection and drives the cost estimate.
#[must_use]
pub fn tests_select_plan(max_tests: Option<u16>) -> PlanExplanation {
    let tests = u64::from(max_tests.unwrap_or(100));
    let cost = tests.saturating_mul(TESTS_COST_PER_TEST);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["test_selection".to_owned()],
        applied_limits: vec![format!("max_tests: {tests}")],
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

    #[test]
    fn symbol_relationships_plan_is_deterministic_and_bounded() {
        use super::symbol_relationships_plan;
        assert_eq!(
            symbol_relationships_plan(2, Some(100)),
            symbol_relationships_plan(2, Some(100))
        );
        let plan = symbol_relationships_plan(2, Some(100));
        assert_eq!(plan.operators, vec!["relationship_expansion".to_owned()]);
        assert_eq!(
            plan.applied_limits,
            vec!["seeds: 2".to_owned(), "max_results: 100".to_owned()]
        );
    }

    #[test]
    fn flow_trace_plan_is_deterministic_and_bounded() {
        use super::flow_trace_plan;
        assert_eq!(
            flow_trace_plan(Some(3), Some(10)),
            flow_trace_plan(Some(3), Some(10))
        );
        let plan = flow_trace_plan(Some(3), Some(10));
        assert_eq!(plan.operators, vec!["path_traversal".to_owned()]);
        assert_eq!(
            plan.applied_limits,
            vec!["max_depth: 3".to_owned(), "max_paths: 10".to_owned()]
        );
    }

    #[test]
    fn change_impact_plan_is_deterministic_and_bounded() {
        use super::change_impact_plan;
        assert_eq!(change_impact_plan(2), change_impact_plan(2));
        let plan = change_impact_plan(2);
        assert_eq!(plan.operators, vec!["change_analysis".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["changed_inputs: 2".to_owned()]);
    }

    #[test]
    fn tests_select_plan_is_deterministic_and_bounded() {
        use super::tests_select_plan;
        assert_eq!(tests_select_plan(Some(20)), tests_select_plan(Some(20)));
        let plan = tests_select_plan(Some(20));
        assert_eq!(plan.operators, vec!["test_selection".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["max_tests: 20".to_owned()]);
    }
}
