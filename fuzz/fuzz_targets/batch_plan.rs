#![no_main]

use libfuzzer_sys::fuzz_target;
use rootlight_agent::batch::StaticBatchPlan;
use rootlight_mcp_contract::{ExposureProfile, context::QueryBatchInput};

const MAX_FUZZ_INPUT_BYTES: usize = 64 * 1024;

fuzz_target!(|bytes: &[u8]| {
    if bytes.len() > MAX_FUZZ_INPUT_BYTES {
        return;
    }
    let Ok(input) = serde_json::from_slice::<QueryBatchInput>(bytes) else {
        return;
    };

    let first = StaticBatchPlan::build(input.clone(), ExposureProfile::Developer);
    let second = StaticBatchPlan::build(input, ExposureProfile::Developer);
    match (first, second) {
        (Ok(first), Ok(second)) => {
            assert_eq!(first.canonical_digest(), second.canonical_digest());
            assert_eq!(first.operations(), second.operations());
        }
        (Err(first), Err(second)) => {
            assert_eq!(
                std::mem::discriminant(&first),
                std::mem::discriminant(&second)
            );
        }
        _ => panic!("identical batch inputs produced different admission outcomes"),
    }
});
