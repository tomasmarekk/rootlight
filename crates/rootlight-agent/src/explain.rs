//! Source-free plan construction for explain mode.
//!
//! Explain mode returns the bounded plan (operators, applied limits, estimated
//! cost) without executing retrieval, so a client can audit what would run
//! before spending work. Plan construction is deterministic for a normalized
//! request and never reads repository source.

use rootlight_mcp_contract::context::{PLANNER_VERSION, PlanExplanation};

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

/// Estimated cost units per planned component for `architecture.overview`.
const OVERVIEW_COST_PER_COMPONENT: u64 = 20;

/// Estimated cost units per planned cycle for `architecture.cycles`.
const CYCLES_COST_PER_CYCLE: u64 = 28;

/// Estimated cost units per planned dead-code candidate for `code.dead`.
const DEAD_COST_PER_CANDIDATE: u64 = 18;

/// Estimated cost units per planned comparison result for `history.compare`.
const HISTORY_COST_PER_RESULT: u64 = 22;

/// Estimated cost units per planned step for `plan.change`.
const PLAN_COST_PER_STEP: u64 = 50;

/// Estimated cost units per planned target for `plan.change`.
const PLAN_COST_PER_TARGET: u64 = 15;

/// Fixed estimated cost units for the metadata-only `repo.status` plan.
const STATUS_READ_COST: u64 = 4;

/// Estimated cost units per planned seed for `context.pack`.
const CONTEXT_COST_PER_SEED: u64 = 30;

/// Estimated cost units per planned batched operation for `query.batch`.
const BATCH_COST_PER_OPERATION: u64 = 100;

/// Fixed estimated cost units for the metadata-only `repo.list` plan.
const REPO_LIST_COST: u64 = 8;

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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
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
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `architecture.overview` plan for explain mode.
///
/// `max_components` bounds the planned aggregation and drives the cost estimate.
#[must_use]
pub fn architecture_overview_plan(max_components: Option<u16>) -> PlanExplanation {
    let components = u64::from(max_components.unwrap_or(100));
    let cost = components.saturating_mul(OVERVIEW_COST_PER_COMPONENT);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["architecture_mapping".to_owned()],
        applied_limits: vec![format!("max_components: {components}")],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `architecture.cycles` plan for explain mode.
///
/// `max_cycles` bounds the planned detection and drives the cost estimate.
#[must_use]
pub fn architecture_cycles_plan(max_cycles: Option<u16>) -> PlanExplanation {
    let cycles = u64::from(max_cycles.unwrap_or(50));
    let cost = cycles.saturating_mul(CYCLES_COST_PER_CYCLE);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["cycle_detection".to_owned()],
        applied_limits: vec![format!("max_cycles: {cycles}")],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `code.dead` plan for explain mode.
///
/// `max_candidates` bounds the planned reachability analysis and drives the
/// cost estimate.
#[must_use]
pub fn code_dead_plan(max_candidates: Option<u16>) -> PlanExplanation {
    let candidates = u64::from(max_candidates.unwrap_or(100));
    let cost = candidates.saturating_mul(DEAD_COST_PER_CANDIDATE);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["reachability_analysis".to_owned()],
        applied_limits: vec![format!("max_candidates: {candidates}")],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `history.compare` plan for explain mode.
///
/// `max_results` bounds the planned comparison and drives the cost estimate.
#[must_use]
pub fn history_compare_plan(max_results: Option<u16>) -> PlanExplanation {
    let results = u64::from(max_results.unwrap_or(100));
    let cost = results.saturating_mul(HISTORY_COST_PER_RESULT);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["revision_comparison".to_owned()],
        applied_limits: vec![format!("max_results: {results}")],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `plan.change` plan for explain mode.
///
/// `max_steps` and `target_count` bound the planned change planning and drive
/// the cost estimate.
#[must_use]
pub fn plan_change_plan(max_steps: Option<u8>, target_count: usize) -> PlanExplanation {
    let steps = u64::from(max_steps.unwrap_or(10));
    let targets = u64::try_from(target_count).unwrap_or(u64::MAX);
    let cost = steps
        .saturating_mul(PLAN_COST_PER_STEP)
        .saturating_add(targets.saturating_mul(PLAN_COST_PER_TARGET));
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["change_planning".to_owned()],
        applied_limits: vec![format!("max_steps: {steps}"), format!("targets: {targets}")],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `repo.status` plan for explain mode.
///
/// `repo.status` reads only repository metadata, so the plan is a fixed bounded
/// status read with no traversal.
#[must_use]
pub fn repo_status_plan() -> PlanExplanation {
    PlanExplanation {
        estimated_cost: STATUS_READ_COST,
        operators: vec!["status_read".to_owned()],
        applied_limits: Vec::new(),
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `context.pack` plan for explain mode.
///
/// `seed_count` and `token_budget` bound the planned evidence assembly and
/// drive the cost estimate.
#[must_use]
pub fn context_pack_plan(seed_count: usize, token_budget: u16) -> PlanExplanation {
    let seeds = u64::try_from(seed_count).unwrap_or(u64::MAX);
    let cost = seeds
        .saturating_mul(CONTEXT_COST_PER_SEED)
        .saturating_add(u64::from(token_budget));
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["context_assembly".to_owned()],
        applied_limits: vec![
            format!("seeds: {seeds}"),
            format!("token_budget: {token_budget}"),
        ],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `query.batch` plan for explain mode.
///
/// `operation_count` bounds the planned batched dispatch and drives the cost
/// estimate.
#[must_use]
pub fn query_batch_plan(operation_count: usize) -> PlanExplanation {
    let operations = u64::try_from(operation_count).unwrap_or(u64::MAX);
    let cost = operations.saturating_mul(BATCH_COST_PER_OPERATION);
    PlanExplanation {
        estimated_cost: cost,
        operators: vec!["batch_dispatch".to_owned()],
        applied_limits: vec![format!("operations: {operations}")],
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Builds the source-free `repo.list` plan for explain mode.
///
/// `repo.list` reads only the registered-repository catalog, so the plan is a
/// fixed bounded listing with no source retrieval.
#[must_use]
pub fn repo_list_plan() -> PlanExplanation {
    PlanExplanation {
        estimated_cost: REPO_LIST_COST,
        operators: vec!["repository_listing".to_owned()],
        applied_limits: Vec::new(),
        planner_version: PLANNER_VERSION,
        fingerprint: String::new(),
    }
}

/// Binds a stable physical-plan fingerprint to a plan for a pinned generation.
///
/// The fingerprint is a deterministic BLAKE3 digest over the planner version,
/// pinned generation, estimated cost, ordered operators, and applied limits, so
/// identical normalized requests on the same generation yield the same
/// fingerprint while a different generation or plan never collides. It is never
/// random.
#[must_use]
pub fn finalize_plan(mut plan: PlanExplanation, generation: &str) -> PlanExplanation {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&plan.planner_version.to_le_bytes());
    hasher.update(b"\x00");
    hasher.update(generation.as_bytes());
    hasher.update(b"\x00");
    hasher.update(&plan.estimated_cost.to_le_bytes());
    hasher.update(b"\x00");
    for operator in &plan.operators {
        hasher.update(operator.as_bytes());
        hasher.update(b",");
    }
    hasher.update(b"\x00");
    for limit in &plan.applied_limits {
        hasher.update(limit.as_bytes());
        hasher.update(b",");
    }
    let hex = hasher.finalize().to_hex();
    let short: String = hex.chars().take(32).collect();
    plan.fingerprint = format!("plan1_{short}");
    plan
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

    #[test]
    fn architecture_overview_plan_is_deterministic_and_bounded() {
        use super::architecture_overview_plan;
        assert_eq!(
            architecture_overview_plan(Some(50)),
            architecture_overview_plan(Some(50))
        );
        let plan = architecture_overview_plan(Some(50));
        assert_eq!(plan.operators, vec!["architecture_mapping".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["max_components: 50".to_owned()]);
    }

    #[test]
    fn architecture_cycles_plan_is_deterministic_and_bounded() {
        use super::architecture_cycles_plan;
        assert_eq!(
            architecture_cycles_plan(Some(25)),
            architecture_cycles_plan(Some(25))
        );
        let plan = architecture_cycles_plan(Some(25));
        assert_eq!(plan.operators, vec!["cycle_detection".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["max_cycles: 25".to_owned()]);
    }

    #[test]
    fn code_dead_plan_is_deterministic_and_bounded() {
        use super::code_dead_plan;
        assert_eq!(code_dead_plan(Some(40)), code_dead_plan(Some(40)));
        let plan = code_dead_plan(Some(40));
        assert_eq!(plan.operators, vec!["reachability_analysis".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["max_candidates: 40".to_owned()]);
    }

    #[test]
    fn history_compare_plan_is_deterministic_and_bounded() {
        use super::history_compare_plan;
        assert_eq!(
            history_compare_plan(Some(30)),
            history_compare_plan(Some(30))
        );
        let plan = history_compare_plan(Some(30));
        assert_eq!(plan.operators, vec!["revision_comparison".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["max_results: 30".to_owned()]);
    }

    #[test]
    fn plan_change_plan_is_deterministic_and_bounded() {
        use super::plan_change_plan;
        assert_eq!(plan_change_plan(Some(5), 2), plan_change_plan(Some(5), 2));
        let plan = plan_change_plan(Some(5), 2);
        assert_eq!(plan.operators, vec!["change_planning".to_owned()]);
        assert_eq!(
            plan.applied_limits,
            vec!["max_steps: 5".to_owned(), "targets: 2".to_owned()]
        );
    }

    #[test]
    fn repo_status_plan_is_deterministic_and_bounded() {
        use super::repo_status_plan;
        assert_eq!(repo_status_plan(), repo_status_plan());
        let plan = repo_status_plan();
        assert_eq!(plan.operators, vec!["status_read".to_owned()]);
        assert!(plan.applied_limits.is_empty());
    }

    #[test]
    fn context_pack_plan_is_deterministic_and_bounded() {
        use super::context_pack_plan;
        assert_eq!(context_pack_plan(3, 1000), context_pack_plan(3, 1000));
        let plan = context_pack_plan(3, 1000);
        assert_eq!(plan.operators, vec!["context_assembly".to_owned()]);
        assert_eq!(
            plan.applied_limits,
            vec!["seeds: 3".to_owned(), "token_budget: 1000".to_owned()]
        );
    }

    #[test]
    fn finalize_plan_fingerprint_is_deterministic() {
        use super::{code_locate_plan, finalize_plan};
        let a = finalize_plan(code_locate_plan(false, 20), "gen-1");
        let b = finalize_plan(code_locate_plan(false, 20), "gen-1");
        assert_eq!(a.fingerprint, b.fingerprint);
        assert!(a.fingerprint.starts_with("plan1_"));
        assert_eq!(a.planner_version, b.planner_version);
    }

    #[test]
    fn finalize_plan_fingerprint_binds_generation_and_plan() {
        use super::{code_locate_plan, finalize_plan};
        let base = finalize_plan(code_locate_plan(false, 20), "gen-1");
        let other_generation = finalize_plan(code_locate_plan(false, 20), "gen-2");
        let other_plan = finalize_plan(code_locate_plan(true, 20), "gen-1");
        assert_ne!(base.fingerprint, other_generation.fingerprint);
        assert_ne!(base.fingerprint, other_plan.fingerprint);
    }

    #[test]
    fn query_batch_plan_is_deterministic_and_bounded() {
        use super::query_batch_plan;
        assert_eq!(query_batch_plan(3), query_batch_plan(3));
        let plan = query_batch_plan(3);
        assert_eq!(plan.operators, vec!["batch_dispatch".to_owned()]);
        assert_eq!(plan.applied_limits, vec!["operations: 3".to_owned()]);
        assert_eq!(plan.estimated_cost, 300);
    }

    #[test]
    fn repo_list_plan_is_deterministic_and_bounded() {
        use super::repo_list_plan;
        assert_eq!(repo_list_plan(), repo_list_plan());
        let plan = repo_list_plan();
        assert_eq!(plan.operators, vec!["repository_listing".to_owned()]);
        assert!(plan.applied_limits.is_empty());
    }
}
