//! Token and byte accounting for MCP tool definitions and responses.
//!
//! Records complete `tools/list` definition costs, per-request bytes,
//! response bytes, and source content bytes separately for every exposure
//! profile. Unexplained increases in definition cost block review.

use crate::catalog::{ExposureProfile, McpTool};
use crate::vertical::VerticalTool;
use serde_json::{Map, Value};

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
/// The measurement serializes the full `{"tools": [...]}` result object an
/// agent receives under the profile, including every tool's input and output
/// schemas and annotations, then counts the serialized UTF-8 bytes. This is
/// the discovery cost paid before any invocation, so it reflects the real
/// wire payload rather than a metadata-only estimate.
///
/// # Panics
///
/// Panics only when a baked-in generated schema fails to parse or the
/// assembled result fails to serialize, both of which are checked invariants
/// of the schema catalog.
#[must_use]
pub fn measure_tool_list(profile: ExposureProfile) -> ToolListAccounting {
    let tools = profile.tools();
    let definitions: Vec<Value> = tools.iter().copied().map(tool_definition_value).collect();
    let mut result = Map::new();
    result.insert("tools".to_owned(), Value::Array(definitions));
    // A value assembled from valid JSON parts always serializes.
    let serialized =
        serde_json::to_vec(&Value::Object(result)).expect("tool list result serializes");
    let definition_bytes = serialized.len();
    ToolListAccounting {
        profile,
        tool_count: tools.len(),
        definition_bytes,
        estimated_tokens: estimate_tokens(definition_bytes),
    }
}

/// Bridges a catalog tool to its schema-bearing vertical counterpart.
///
/// Both enums enumerate the same nineteen tools with identical stable names,
/// so the name lookup is a fixed invariant for any tool drawn from the
/// catalog.
fn vertical_tool_for(tool: McpTool) -> VerticalTool {
    VerticalTool::ALL
        .iter()
        .copied()
        .find(|candidate| candidate.name() == tool.name())
        .expect("every catalog tool has a vertical counterpart")
}

/// Builds one tool definition mirroring the router's serialized wire shape.
///
/// Field names and nesting match the router's `ToolDefinition` so the measured
/// bytes equal the real `tools/list` payload an agent receives. The baked-in
/// generated schemas are parsed into values so their full serialized size is
/// counted, not just the surrounding metadata.
fn tool_definition_value(tool: McpTool) -> Value {
    let vertical = vertical_tool_for(tool);
    // The generated schema artifacts are compiled into the binary and checked
    // by the schema tests, so parsing them here cannot fail.
    let input_schema: Value = serde_json::from_str(vertical.input_schema_json())
        .expect("baked-in input schema is valid json");
    let output_schema: Value = serde_json::from_str(vertical.output_schema_json())
        .expect("baked-in output schema is valid json");

    let mut annotations = Map::new();
    annotations.insert("readOnlyHint".to_owned(), Value::Bool(tool.read_only()));
    annotations.insert(
        "destructiveHint".to_owned(),
        Value::Bool(tool.destructive()),
    );
    annotations.insert("idempotentHint".to_owned(), Value::Bool(tool.idempotent()));
    annotations.insert("openWorldHint".to_owned(), Value::Bool(false));

    let mut execution = Map::new();
    execution.insert(
        "taskSupport".to_owned(),
        Value::String("forbidden".to_owned()),
    );

    let mut definition = Map::new();
    definition.insert("name".to_owned(), Value::String(tool.name().to_owned()));
    definition.insert("title".to_owned(), Value::String(tool.title().to_owned()));
    definition.insert(
        "description".to_owned(),
        Value::String(tool.description().to_owned()),
    );
    definition.insert("inputSchema".to_owned(), input_schema);
    definition.insert("outputSchema".to_owned(), output_schema);
    definition.insert("annotations".to_owned(), Value::Object(annotations));
    definition.insert("execution".to_owned(), Value::Object(execution));

    Value::Object(definition)
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
    use crate::vertical::VerticalTool;
    use serde_json::{Map, Value};

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

        // Independently re-serialize the complete developer tools/list result
        // and require the accounting to report its exact byte length.
        let definitions: Vec<Value> = ExposureProfile::Developer
            .tools()
            .iter()
            .copied()
            .map(serialize_developer_definition)
            .collect();
        let mut result = Map::new();
        result.insert("tools".to_owned(), Value::Array(definitions));
        let serialized = serde_json::to_vec(&Value::Object(result)).expect("result serializes");

        assert_eq!(
            developer.definition_bytes,
            serialized.len(),
            "definition_bytes must equal the real serialized tools/list length"
        );
        // Guards against regressing to the old name+title+description estimate,
        // which ignored schemas and serialized only a few kilobytes.
        assert!(
            developer.definition_bytes > 100_000,
            "developer tools/list measurement looks shallow: {} bytes",
            developer.definition_bytes
        );
        assert_eq!(
            developer.estimated_tokens,
            estimate_tokens(developer.definition_bytes)
        );
    }

    /// Independently rebuilds one developer tool definition from the catalog
    /// metadata and generated schemas, mirroring the MCP wire shape, so the
    /// accounting can be checked against a from-scratch serialization.
    fn serialize_developer_definition(tool: McpTool) -> Value {
        let vertical = VerticalTool::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.name() == tool.name())
            .expect("developer tool has a vertical counterpart");
        let input_schema: Value =
            serde_json::from_str(vertical.input_schema_json()).expect("input schema is valid json");
        let output_schema: Value = serde_json::from_str(vertical.output_schema_json())
            .expect("output schema is valid json");

        let mut annotations = Map::new();
        annotations.insert("readOnlyHint".to_owned(), Value::Bool(tool.read_only()));
        annotations.insert(
            "destructiveHint".to_owned(),
            Value::Bool(tool.destructive()),
        );
        annotations.insert("idempotentHint".to_owned(), Value::Bool(tool.idempotent()));
        annotations.insert("openWorldHint".to_owned(), Value::Bool(false));

        let mut execution = Map::new();
        execution.insert(
            "taskSupport".to_owned(),
            Value::String("forbidden".to_owned()),
        );

        let mut definition = Map::new();
        definition.insert("name".to_owned(), Value::String(tool.name().to_owned()));
        definition.insert("title".to_owned(), Value::String(tool.title().to_owned()));
        definition.insert(
            "description".to_owned(),
            Value::String(tool.description().to_owned()),
        );
        definition.insert("inputSchema".to_owned(), input_schema);
        definition.insert("outputSchema".to_owned(), output_schema);
        definition.insert("annotations".to_owned(), Value::Object(annotations));
        definition.insert("execution".to_owned(), Value::Object(execution));

        Value::Object(definition)
    }
}
