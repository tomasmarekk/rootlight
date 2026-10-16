//! Explicit service-equivalent capacities for the read-only admission diagnostic.
//! Service tests compare these complete values with the private production
//! constructors so a diagnostic cannot silently measure a different resource policy.

use rootlight_adapter_sdk::{AnalysisLimits, BatchThresholds, LimitError, StreamLimits};
use rootlight_adapter_treesitter::{ParserSettings, RuntimeConfig, RuntimeConfigError};
use rootlight_ir::IrLimits;

/// Builds the diagnostic's complete service-equivalent analysis profile.
///
/// # Errors
/// Returns a checked limit error for an invalid source capacity.
pub(crate) fn analysis_limits(maximum_source_bytes: usize) -> Result<AnalysisLimits, LimitError> {
    let stream = StreamLimits::new(
        512 * 1024 * 1024,
        512 * 1024 * 1024,
        512 * 1024 * 1024,
        16_384,
        16 * 1024 * 1024,
        128 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024)?,
    )?;
    AnalysisLimits::new(
        maximum_source_bytes,
        usize::MAX,
        4096,
        32,
        512 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
}

/// Builds the diagnostic's complete service-equivalent native parser profile.
///
/// # Errors
/// Returns the native runtime's checked configuration error.
pub(crate) fn parser_config(
    maximum_source_bytes: usize,
) -> Result<RuntimeConfig, RuntimeConfigError> {
    RuntimeConfig::new(
        maximum_source_bytes,
        usize::MAX,
        4096,
        32,
        64,
        1,
        16 * 1024 * 1024,
        ParserSettings::new(4096)?,
    )
}
