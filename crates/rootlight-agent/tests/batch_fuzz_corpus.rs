//! Regression coverage for deterministic batch-planner fuzz seeds.

use rootlight_agent::batch::StaticBatchPlan;
use rootlight_mcp_contract::{ExposureProfile, context::QueryBatchInput};

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
];

#[test]
fn batch_plan_fuzz_corpus_is_parseable_and_deterministic() {
    for (name, bytes) in SEEDS {
        let input = serde_json::from_slice::<QueryBatchInput>(bytes)
            .unwrap_or_else(|error| panic!("{name} corpus seed must deserialize: {error}"));
        let first = StaticBatchPlan::build(input.clone(), ExposureProfile::Developer);
        let second = StaticBatchPlan::build(input, ExposureProfile::Developer);
        match (first, second) {
            (Ok(first), Ok(second)) => {
                assert_eq!(first.canonical_digest(), second.canonical_digest(), "{name}");
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
