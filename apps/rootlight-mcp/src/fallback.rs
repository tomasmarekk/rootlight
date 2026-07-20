//! Typed fallback responses for intent tools pending daemon port implementation.
//!
//! Each intent tool returns a valid, schema-conformant empty result wrapped in
//! the standard ReadEnvelope. This allows agents to exercise the full tool
//! surface and receive deterministic, bounded responses rather than opaque
//! capability errors. The fallback clearly signals incomplete coverage through
//! the coverage status and warnings.

use rootlight_ids::{GenerationId, RepositoryId};
use serde_json::{Map, Value};

use crate::ToolExecutionError;

/// Builds a typed fallback ReadEnvelope response for an intent tool.
///
/// The response is schema-valid, deterministic, and clearly signals that the
/// tool's full analysis capability is not yet available through the daemon port.
pub(crate) fn intent_fallback_response(
    repository: RepositoryId,
    generation: GenerationId,
    data: Value,
) -> Result<Map<String, Value>, ToolExecutionError> {
    let envelope = serde_json::json!({
        "schema_version": "1.0",
        "repository": {
            "repository_id": repository.to_string(),
            "display_name": "fallback"
        },
        "generation": {
            "generation_id": generation.to_string(),
            "parent_generation": null,
            "structural_freshness": "current",
            "semantic_freshness": "stale"
        },
        "coverage": {
            "status": "bounded",
            "languages": [],
            "skipped_inputs": 0
        },
        "data": data,
        "truncated": false,
        "next_cursor": null,
        "usage": {
            "rows": 0,
            "edges": 0,
            "source_bytes": 0,
            "json_bytes": 0,
            "estimated_tokens": 0,
            "wall_time_ms": 0,
            "cache_status": "miss",
            "trace_id": "fallback"
        },
        "warnings": [{
            "code": "bounded-fallback",
            "message": "tool returned bounded fallback pending full analysis"
        }],
        "trust": "untrusted_repository_data"
    });

    let Value::Object(output) = envelope else {
        return Err(ToolExecutionError::internal(
            crate::ToolExecutionFailure::Executor,
        ));
    };
    Ok(output)
}

/// Empty data payloads for each intent tool's fallback response.
pub(crate) fn empty_data_for_tool(tool_name: &str) -> Value {
    match tool_name {
        "symbol.relationships" => serde_json::json!({
            "groups": [],
            "unresolved_sites": [],
            "totals": {"exact_edges": 0, "candidate_edges": 0, "truncated": false}
        }),
        "flow.trace" => serde_json::json!({
            "paths": [],
            "frontier": {"reached_depth": 0, "open_nodes": 0, "cycle_detected": false}
        }),
        "change.impact" => serde_json::json!({
            "resolved_changes": [],
            "impact_groups": [],
            "service_impacts": [],
            "test_candidates": [],
            "risk_summary": {"coverage": "unknown", "breaking_surface": 0, "fanout": 0, "dynamic_blind_spots": 0}
        }),
        "tests.select" => serde_json::json!({
            "ranked_tests": [],
            "coverage_strategy": "none",
            "gaps": []
        }),
        "architecture.overview" => serde_json::json!({
            "view": "module",
            "components": [],
            "connections": [],
            "hotspots": [],
            "detail": "summary"
        }),
        "architecture.cycles" => serde_json::json!({
            "projection": "module",
            "sccs": [],
            "cycles": [],
            "break_candidates": []
        }),
        "code.dead" => serde_json::json!({
            "candidates": [],
            "entry_points": {"policy": "conservative", "count": 0},
            "blind_spots": [],
            "suppression_rules": []
        }),
        "history.compare" => serde_json::json!({
            "matched_states": {"base": null, "target": null},
            "changes": [],
            "architecture_deltas": [],
            "breaking_candidates": []
        }),
        "plan.change" => serde_json::json!({
            "objective": "refactor",
            "steps": [],
            "impact_summary": {"affected_symbols": 0, "affected_files": 0, "risk_level": "low"},
            "decisions": []
        }),
        "context.pack" => serde_json::json!({
            "items": [],
            "server_guidance": {"reading_order": [], "caveats": []},
            "omissions": [],
            "token_accounting": {"used": 0, "budget": 0, "remaining": 0}
        }),
        "query.advanced" => serde_json::json!({
            "columns": [],
            "rows": [],
            "plan": null,
            "completeness": "unsupported"
        }),
        "query.batch" => serde_json::json!({
            "batch_status": "ok",
            "generation_id": null,
            "operation_results": []
        }),
        _ => serde_json::json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::empty_data_for_tool;

    #[test]
    fn all_intent_tools_have_empty_data() {
        let tools = [
            "symbol.relationships",
            "flow.trace",
            "change.impact",
            "tests.select",
            "architecture.overview",
            "architecture.cycles",
            "code.dead",
            "history.compare",
            "plan.change",
            "context.pack",
            "query.advanced",
            "query.batch",
        ];
        for tool in tools {
            let data = empty_data_for_tool(tool);
            assert!(data.is_object(), "{tool} must return an object");
        }
    }

    #[test]
    fn unknown_tool_returns_empty_object() {
        let data = empty_data_for_tool("unknown.tool");
        assert_eq!(data, serde_json::json!({}));
    }
}
