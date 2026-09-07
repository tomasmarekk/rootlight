//! Reproducible release measurements of the public YAML scalar constructor.
//! Input construction is outside timing; successful and budget-rejected inputs
//! are separate cases, and elapsed time is evidence rather than a test assertion.

use std::{hint::black_box, time::Instant};

use rootlight_adapter_sdk::YamlDocumentContext;
use rootlight_ir::IrLimits;

fn main() {
    let maximum = IrLimits::default().max_string_bytes;
    let context = YamlDocumentContext::new(None, &[], maximum).unwrap();
    for (label, prefix, digit) in [("hex", "0x", 'f'), ("octal", "0o", '7')] {
        for digits in [16, 256, 1024, 4096, 16_384, 24_576, 32_766] {
            let source = format!("{prefix}{}", digit.to_string().repeat(digits));
            let expected = context.flow_scalar(&source, None);
            let iterations = if digits <= 256 { 100 } else { 3 };
            let mut samples = Vec::new();
            for _ in 0..5 {
                let started = Instant::now();
                for _ in 0..iterations {
                    black_box(context.flow_scalar(black_box(&source), None));
                }
                samples.push(started.elapsed().as_nanos() / iterations);
            }
            samples.sort_unstable();
            println!(
                "{}",
                serde_json::json!({
                    "schema": "rootlight.yaml-radix-benchmark/1", "radix": label,
                    "digits": digits, "maximum_bytes": maximum,
                    "accepted": expected.is_some(),
                    "output_bytes": expected.as_ref().map_or(0, |value| value.name().len()),
                    "samples_ns": samples, "median_ns": samples[2], "iterations": iterations,
                })
            );
        }
    }
}
