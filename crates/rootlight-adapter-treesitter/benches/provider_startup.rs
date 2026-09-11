//! Measures first-provider and repeated-provider construction in a fresh process.
//! Both include teardown; configuration construction is outside timing, and
//! durations are evidence rather than correctness thresholds or indexing timings.

use std::{hint::black_box, time::Instant};

use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, RuntimeConfigError, TreeSitterProvider,
};

fn measure(config: &RuntimeConfig, iterations: u32) -> Result<u128, RuntimeConfigError> {
    let started = Instant::now();
    for _ in 0..iterations {
        let provider = black_box(TreeSitterProvider::new(black_box(config.clone()))?);
        black_box(provider.stats());
        drop(provider);
    }
    Ok(started.elapsed().as_nanos() / u128::from(iterations))
}

fn main() -> Result<(), RuntimeConfigError> {
    let config = RuntimeConfig::new(
        4096,
        1024,
        64,
        8,
        8,
        1,
        2 * 1024 * 1024,
        ParserSettings::new(256)?,
    )?;
    let first = measure(&config, 1)?;
    println!(
        "{}",
        serde_json::json!({
            "schema": "rootlight.provider-startup-benchmark/1",
            "scenario": "first_provider",
            "iterations": 1,
            "samples_ns": [first],
            "median_ns": first,
            "debug_assertions": cfg!(debug_assertions),
        })
    );

    let mut samples = Vec::with_capacity(5);
    for _ in 0..5 {
        samples.push(measure(&config, 8)?);
    }
    samples.sort_unstable();
    println!(
        "{}",
        serde_json::json!({
            "schema": "rootlight.provider-startup-benchmark/1",
            "scenario": "repeated_provider",
            "iterations": 8,
            "samples_ns": samples,
            "median_ns": samples[2],
            "debug_assertions": cfg!(debug_assertions),
        })
    );
    Ok(())
}
