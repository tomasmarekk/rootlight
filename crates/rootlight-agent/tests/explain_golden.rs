//! Golden plan corpus for the source-free explain planner.
//!
//! Each public read tool's plan is captured for a canonical normalized request
//! so unintended changes to operators, applied limits, estimated cost, or
//! planner version are caught by name. Plans are source-free: they depend only
//! on the normalized request and planner version, never on repository index
//! state, so the same golden plan holds across empty, partial, stale, fresh,
//! small, large, and unsupported capability states.

use rootlight_agent::explain::{
    architecture_cycles_plan, architecture_overview_plan, change_impact_plan, code_dead_plan,
    code_locate_plan, context_pack_plan, finalize_plan, flow_trace_plan, history_compare_plan,
    plan_change_plan, query_batch_plan, repo_list_plan, repo_status_plan, source_read_plan,
    symbol_explain_plan, symbol_relationships_plan, tests_select_plan,
};
use rootlight_mcp_contract::context::PLANNER_VERSION;

/// A pinned generation used to make golden fingerprints reproducible.
const PINNED_GENERATION: &str = "gen-golden-000000000000000000000000";

#[test]
fn golden_code_locate_lexical_and_exact() {
    let lexical = code_locate_plan(false, 20);
    assert_eq!(lexical.operators, vec!["lexical_scan".to_owned()]);
    assert_eq!(lexical.applied_limits, vec!["max_results: 20".to_owned()]);
    assert_eq!(lexical.estimated_cost, 160);
    assert_eq!(lexical.planner_version, PLANNER_VERSION);

    let exact = code_locate_plan(true, 5);
    assert_eq!(exact.operators, vec!["index_lookup".to_owned()]);
    assert_eq!(exact.estimated_cost, 40);
}

#[test]
fn golden_symbol_explain() {
    let plan = symbol_explain_plan(3);
    assert_eq!(plan.operators, vec!["symbol_lookup".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["symbols: 3".to_owned()]);
    assert_eq!(plan.estimated_cost, 36);
    assert_eq!(plan.planner_version, PLANNER_VERSION);
}

#[test]
fn golden_source_read() {
    let plan = source_read_plan(2);
    assert_eq!(plan.operators, vec!["source_read".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["references: 2".to_owned()]);
    assert_eq!(plan.estimated_cost, 32);
}

#[test]
fn golden_symbol_relationships() {
    let plan = symbol_relationships_plan(2, Some(100));
    assert_eq!(plan.operators, vec!["relationship_expansion".to_owned()]);
    assert_eq!(
        plan.applied_limits,
        vec!["seeds: 2".to_owned(), "max_results: 100".to_owned()]
    );
    assert_eq!(plan.estimated_cost, 48);
}

#[test]
fn golden_flow_trace() {
    let plan = flow_trace_plan(Some(3), Some(10));
    assert_eq!(plan.operators, vec!["path_traversal".to_owned()]);
    assert_eq!(
        plan.applied_limits,
        vec!["max_depth: 3".to_owned(), "max_paths: 10".to_owned()]
    );
    assert_eq!(plan.estimated_cost, 96);
}

#[test]
fn golden_change_impact() {
    let plan = change_impact_plan(2);
    assert_eq!(plan.operators, vec!["change_analysis".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["changed_inputs: 2".to_owned()]);
    assert_eq!(plan.estimated_cost, 80);
}

#[test]
fn golden_tests_select() {
    let plan = tests_select_plan(Some(20));
    assert_eq!(plan.operators, vec!["test_selection".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["max_tests: 20".to_owned()]);
    assert_eq!(plan.estimated_cost, 120);
}

#[test]
fn golden_architecture_overview() {
    let plan = architecture_overview_plan(Some(50));
    assert_eq!(plan.operators, vec!["architecture_mapping".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["max_components: 50".to_owned()]);
    assert_eq!(plan.estimated_cost, 1000);
}

#[test]
fn golden_architecture_cycles() {
    let plan = architecture_cycles_plan(Some(25));
    assert_eq!(plan.operators, vec!["cycle_detection".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["max_cycles: 25".to_owned()]);
    assert_eq!(plan.estimated_cost, 700);
}

#[test]
fn golden_code_dead() {
    let plan = code_dead_plan(Some(40));
    assert_eq!(plan.operators, vec!["reachability_analysis".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["max_candidates: 40".to_owned()]);
    assert_eq!(plan.estimated_cost, 720);
}

#[test]
fn golden_history_compare() {
    let plan = history_compare_plan(Some(30));
    assert_eq!(plan.operators, vec!["revision_comparison".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["max_results: 30".to_owned()]);
    assert_eq!(plan.estimated_cost, 660);
}

#[test]
fn golden_plan_change() {
    let plan = plan_change_plan(Some(5), 2);
    assert_eq!(plan.operators, vec!["change_planning".to_owned()]);
    assert_eq!(
        plan.applied_limits,
        vec!["max_steps: 5".to_owned(), "targets: 2".to_owned()]
    );
    assert_eq!(plan.estimated_cost, 280);
}

#[test]
fn golden_repo_status() {
    let plan = repo_status_plan();
    assert_eq!(plan.operators, vec!["status_read".to_owned()]);
    assert!(plan.applied_limits.is_empty());
    assert_eq!(plan.estimated_cost, 4);
}

#[test]
fn golden_query_batch() {
    let plan = query_batch_plan(3);
    assert_eq!(plan.operators, vec!["batch_dispatch".to_owned()]);
    assert_eq!(plan.applied_limits, vec!["operations: 3".to_owned()]);
    assert_eq!(plan.estimated_cost, 300);
}

#[test]
fn golden_repo_list() {
    let plan = repo_list_plan();
    assert_eq!(plan.operators, vec!["repository_listing".to_owned()]);
    assert!(plan.applied_limits.is_empty());
    assert_eq!(plan.estimated_cost, 8);
}

#[test]
fn golden_context_pack() {
    let plan = context_pack_plan(3, 1000);
    assert_eq!(plan.operators, vec!["context_assembly".to_owned()]);
    assert_eq!(
        plan.applied_limits,
        vec!["seeds: 3".to_owned(), "token_budget: 1000".to_owned()]
    );
    assert_eq!(plan.estimated_cost, 1090);
}

#[test]
fn golden_fingerprints_are_stable_for_a_pinned_generation() {
    // A representative plan per tool, finalized against one pinned generation,
    // yields a reproducible fingerprint across repeated construction.
    let plans = vec![
        code_locate_plan(false, 20),
        symbol_explain_plan(3),
        source_read_plan(2),
        symbol_relationships_plan(2, Some(100)),
        flow_trace_plan(Some(3), Some(10)),
        change_impact_plan(2),
        tests_select_plan(Some(20)),
        architecture_overview_plan(Some(50)),
        architecture_cycles_plan(Some(25)),
        code_dead_plan(Some(40)),
        history_compare_plan(Some(30)),
        plan_change_plan(Some(5), 2),
        repo_status_plan(),
        context_pack_plan(3, 1000),
        query_batch_plan(2),
        repo_list_plan(),
    ];
    for plan in plans {
        let first = finalize_plan(plan.clone(), PINNED_GENERATION);
        let second = finalize_plan(plan, PINNED_GENERATION);
        assert!(first.fingerprint.starts_with("plan1_"));
        assert_eq!(first.fingerprint.len(), "plan1_".len() + 32);
        assert_eq!(first.fingerprint, second.fingerprint);
    }
}
