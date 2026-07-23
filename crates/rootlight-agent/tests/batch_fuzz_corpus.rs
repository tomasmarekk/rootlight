//! Regression coverage for deterministic batch-planner fuzz seeds.

use proptest::prelude::*;
use rootlight_agent::batch::StaticBatchPlan;
use rootlight_mcp_contract::{ExposureProfile, context::QueryBatchInput};
use serde_json::{Value, json};

const SEEDS: &[(&str, &[u8])] = &[
    (
        "valid binding DAG",
        include_bytes!("../../../fuzz/corpus/batch_plan/valid_binding_dag.json"),
    ),
    (
        "cyclic duplicate IDs",
        include_bytes!("../../../fuzz/corpus/batch_plan/cyclic_duplicate_ids.json"),
    ),
    (
        "invalid binding edge",
        include_bytes!("../../../fuzz/corpus/batch_plan/invalid_binding_edge.json"),
    ),
    (
        "nested argument template",
        include_bytes!("../../../fuzz/corpus/batch_plan/nested_argument_template.json"),
    ),
    (
        "operation ID boundary",
        include_bytes!("../../../fuzz/corpus/batch_plan/operation_id_boundary.json"),
    ),
    (
        "typed scalar binding edge",
        include_bytes!("../../../fuzz/corpus/batch_plan/typed_scalar_binding.json"),
    ),
];

fn parse_input(value: Value) -> QueryBatchInput {
    serde_json::from_value(value).expect("generated batch request matches the public wire contract")
}

fn code_locate_request(id: &str, arguments: Value) -> QueryBatchInput {
    parse_input(json!({
        "repository": {
            "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
        },
        "operations": [{
            "id": id,
            "tool": "code.locate",
            "arguments": arguments
        }]
    }))
}

fn nested_template(depth: usize, payload: u64) -> Value {
    (0..depth).fold(json!({"value": payload}), |value, level| {
        if level % 2 == 0 {
            json!({"nested": value})
        } else {
            json!([value])
        }
    })
}

#[test]
fn batch_plan_fuzz_corpus_is_parseable_and_deterministic() {
    for (name, bytes) in SEEDS {
        let input = serde_json::from_slice::<QueryBatchInput>(bytes)
            .unwrap_or_else(|error| panic!("{name} corpus seed must deserialize: {error}"));
        let first = StaticBatchPlan::build(input.clone(), ExposureProfile::Developer);
        let second = StaticBatchPlan::build(input, ExposureProfile::Developer);
        match (first, second) {
            (Ok(first), Ok(second)) => {
                assert_eq!(
                    first.canonical_digest(),
                    second.canonical_digest(),
                    "{name}"
                );
            }
            (Err(first), Err(second)) => {
                assert_eq!(
                    std::mem::discriminant(&first),
                    std::mem::discriminant(&second),
                    "{name}"
                );
            }
            _ => panic!("{name} produced a nondeterministic admission outcome"),
        }
    }
}

#[test]
fn canonical_field_order_and_explicit_defaults_share_one_digest() {
    let omitted = serde_json::from_str::<QueryBatchInput>(
        r#"{
            "repository":{"repository_id":"repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"},
            "operations":[{
                "id":"find",
                "tool":"code.locate",
                "arguments":{"query":"needle","max_results":8,"search_modes":["exact"]}
            }]
        }"#,
    )
    .expect("omitted-default fixture is valid");
    let explicit_reordered = serde_json::from_str::<QueryBatchInput>(
        r#"{
            "explain":false,
            "response_profile":"compact",
            "failure_policy":"continue_independent",
            "operations":[{
                "arguments":{"search_modes":["exact"],"max_results":8,"query":"needle"},
                "tool":"code.locate",
                "id":"find"
            }],
            "repository":{"repository_id":"repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"}
        }"#,
    )
    .expect("explicit-default fixture is valid");

    let omitted = StaticBatchPlan::build(omitted, ExposureProfile::Developer)
        .expect("omitted defaults admit a plan");
    let explicit = StaticBatchPlan::build(explicit_reordered, ExposureProfile::Developer)
        .expect("explicit defaults admit a plan");
    assert_eq!(omitted.canonical_digest(), explicit.canonical_digest());
    assert_eq!(
        omitted.operations()[0].witness_arguments(),
        explicit.operations()[0].witness_arguments()
    );
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        failure_persistence: None,
        .. ProptestConfig::default()
    })]

    #[test]
    fn valid_operation_ids_admit_and_invalid_ids_fail(
        valid in "[A-Za-z0-9_]{1,32}",
        invalid_character in "[A-Za-z0-9_]{0,12}[-./ ][A-Za-z0-9_]{0,12}",
        oversized in "[A-Za-z0-9_]{33,64}",
    ) {
        let valid_plan = StaticBatchPlan::build(
            code_locate_request(&valid, json!({"query": "needle"})),
            ExposureProfile::Developer,
        );
        prop_assert!(valid_plan.is_ok(), "valid operation ID {valid:?} was rejected");

        for invalid in [invalid_character, oversized] {
            let invalid_plan = StaticBatchPlan::build(
                code_locate_request(&invalid, json!({"query": "needle"})),
                ExposureProfile::Developer,
            );
            prop_assert!(invalid_plan.is_err(), "invalid operation ID {invalid:?} was admitted");
        }
    }

    #[test]
    fn typed_binding_indices_obey_the_registered_source_bound(index in 0_u16..=250) {
        let input = parse_input(json!({
            "repository": {
                "repository_id": "repo1_3hhm6hhk3shhmievg6ra3yjlhp2wuv5v"
            },
            "operations": [
                {
                    "id": "find",
                    "tool": "code.locate",
                    "arguments": {"query": "needle"}
                },
                {
                    "id": "trace",
                    "tool": "flow.trace",
                    "depends_on": ["find"],
                    "arguments": {
                        "from": {
                            "symbol_id": {
                                "$from": "find",
                                "source": "symbol_id",
                                "index": index
                            }
                        },
                        "direction": "downstream"
                    }
                }
            ]
        }));
        let plan = StaticBatchPlan::build(input, ExposureProfile::Developer);
        prop_assert_eq!(plan.is_ok(), index < 200);
    }

    #[test]
    fn nested_argument_templates_are_deterministic(
        depth in 0_usize..=16,
        payload in any::<u64>(),
    ) {
        let arguments = json!({
            "query": "needle",
            "scope": {
                "paths": ["src/**"],
                "metadata": nested_template(depth, payload)
            }
        });
        let input = code_locate_request("nested", arguments);
        let first = StaticBatchPlan::build(input.clone(), ExposureProfile::Developer)
            .expect("ordinary nested literals admit a plan");
        let second = StaticBatchPlan::build(input, ExposureProfile::Developer)
            .expect("the same nested literals remain admissible");
        prop_assert_eq!(first.canonical_digest(), second.canonical_digest());
        prop_assert_eq!(
            first.operations()[0].witness_arguments(),
            second.operations()[0].witness_arguments()
        );
    }
}
