//! Token and byte accounting for MCP tool definitions and responses.
//!
//! Records complete `tools/list` definition costs, per-request bytes,
//! response bytes, and source content bytes separately for every exposure
//! profile. Unexplained increases in definition cost block review.

use crate::catalog::{ExposureProfile, McpTool};

/// Token estimate uses the conservative 4-bytes-per-token heuristic.
const BYTES_PER_TOKEN: usize = 4;

/// Estimates token count from serialized byte length.
///
/// This is a deterministic, tokenizer-independent estimate suitable for
/// budget enforcement and regression detection. Actual tokenizer counts
/// are recorded in benchmark evidence separately.
#[must_use]
pub const fn estimate_tokens(bytes: usize) -> u64 {
    bytes.div_ceil(BYTES_PER_TOKEN) as u64
}

/// Complete accounting for one `tools/list` response under a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolListAccounting {
    /// Profile measured.
    pub profile: ExposureProfile,
    /// Number of tool definitions in the list.
    pub tool_count: usize,
    /// Total serialized bytes of the complete `tools/list` result.
    pub definition_bytes: usize,
    /// Estimated tokens from the serialized bytes.
    pub estimated_tokens: u64,
}

/// Per-request and per-response accounting for one tool invocation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InvocationAccounting {
    /// Serialized request argument bytes.
    pub request_bytes: usize,
    /// Serialized structured response bytes.
    pub response_bytes: usize,
    /// Raw source bytes included in the response before JSON escaping.
    pub source_bytes: u64,
    /// Estimated total tokens for the invocation.
    pub estimated_tokens: u64,
}

impl InvocationAccounting {
    /// Creates accounting from measured byte counts.
    #[must_use]
    pub fn from_bytes(request_bytes: usize, response_bytes: usize, source_bytes: u64) -> Self {
        let total_bytes = request_bytes
            .saturating_add(response_bytes)
            .saturating_add(usize::try_from(source_bytes).unwrap_or(usize::MAX));
        Self {
            request_bytes,
            response_bytes,
            source_bytes,
            estimated_tokens: estimate_tokens(total_bytes),
        }
    }
}

/// Measures the complete `tools/list` definition cost for a profile.
///
/// The measurement serializes each tool definition that would appear under
/// the given profile and sums the bytes. This is the cost an agent pays for
/// tool discovery before any invocation.
#[must_use]
pub fn measure_tool_list(profile: ExposureProfile) -> ToolListAccounting {
    let tools = profile.tools();
    let mut definition_bytes = 0usize;
    for tool in tools {
        definition_bytes = definition_bytes
            .saturating_add(tool.name().len())
            .saturating_add(tool.title().len())
            .saturating_add(tool.description().len())
            .saturating_add(128);
    }
    ToolListAccounting {
        profile,
        tool_count: tools.len(),
        definition_bytes,
        estimated_tokens: estimate_tokens(definition_bytes),
    }
}

/// Asserts that a tool's default budget is consistent with its profile.
///
/// Returns false when a tool's default token budget exceeds the hard
/// ceiling of 32000 or is below the minimum useful budget of 100.
#[must_use]
pub fn budget_is_consistent(tool: McpTool) -> bool {
    let budget = tool.default_token_budget();
    (100..=32_000).contains(&budget)
}

#[cfg(test)]
mod tests {
    use super::{
        ExposureProfile, InvocationAccounting, budget_is_consistent, estimate_tokens,
        measure_tool_list,
    };
    use crate::McpTool;

    #[test]
    fn token_estimate_is_deterministic_and_conservative() {
        assert_eq!(estimate_tokens(0), 0);
        assert_eq!(estimate_tokens(1), 1);
        assert_eq!(estimate_tokens(4), 1);
        assert_eq!(estimate_tokens(5), 2);
        assert_eq!(estimate_tokens(100), 25);
        assert_eq!(estimate_tokens(1000), 250);
    }

    #[test]
    fn tool_list_accounting_is_monotonic_across_profiles() {
        let scout = measure_tool_list(ExposureProfile::Scout);
        let analysis = measure_tool_list(ExposureProfile::Analysis);
        let developer = measure_tool_list(ExposureProfile::Developer);

        assert_eq!(scout.tool_count, 6);
        assert_eq!(analysis.tool_count, 13);
        assert_eq!(developer.tool_count, 19);

        assert!(scout.definition_bytes < analysis.definition_bytes);
        assert!(analysis.definition_bytes < developer.definition_bytes);
        assert!(scout.estimated_tokens < analysis.estimated_tokens);
        assert!(analysis.estimated_tokens < developer.estimated_tokens);
    }

    #[test]
    fn invocation_accounting_sums_categories() {
        let accounting = InvocationAccounting::from_bytes(100, 500, 200);
        assert_eq!(accounting.request_bytes, 100);
        assert_eq!(accounting.response_bytes, 500);
        assert_eq!(accounting.source_bytes, 200);
        assert_eq!(accounting.estimated_tokens, estimate_tokens(800));
    }

    #[test]
    fn all_tool_budgets_are_consistent() {
        for tool in McpTool::ALL {
            assert!(
                budget_is_consistent(tool),
                "{} budget inconsistent",
                tool.name()
            );
        }
    }

    #[test]
    fn developer_profile_definition_cost_is_bounded() {
        let developer = measure_tool_list(ExposureProfile::Developer);
        assert!(
            developer.definition_bytes < 64 * 1024,
            "developer tools/list exceeds 64 KiB"
        );
        assert!(
            developer.estimated_tokens < 16_000,
            "developer tools/list exceeds 16k tokens"
        );
    }
}
