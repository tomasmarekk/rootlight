//! Canonical capability registry for the nineteen public MCP tools.
//!
//! One authoritative, machine-readable entry per tool records the contract
//! version, batch eligibility, explain support, handler availability, and the
//! honest runtime disposition. The registry is the single source of truth that
//! the catalog, the batch allowlist, profile membership, and the GATE-3
//! execution matrix are validated against, so the schema, router, executor, and
//! fixtures cannot drift independently. It describes what the runtime can
//! currently do; it is not itself proof that behavior passes acceptance.

use crate::MCP_SCHEMA_VERSION;
use crate::catalog::McpTool;

/// How a tool capability is currently satisfied at runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// Fully implemented and accepted with process evidence.
    Implemented,
    /// Accepted by the schema but rejected before execution with a stable error.
    UnsupportedStableError,
    /// Available only within a documented bounded fallback.
    FallbackLimited,
    /// Not available; blocked pending evidence or design.
    Blocked,
}

/// One tool's canonical capability entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolCapability {
    /// The tool this entry describes.
    pub tool: McpTool,
    /// Contract schema version this entry is written against.
    pub contract_version: &'static str,
    /// Whether the tool may appear inside a public `query.batch`.
    pub batch_eligible: bool,
    /// Whether the tool exposes a source-free explain plan.
    pub explain_supported: bool,
    /// Whether a process-level handler currently exists.
    pub handler_available: bool,
    /// Honest runtime disposition.
    pub status: CapabilityStatus,
    /// Source-free, concise fallback description safe for public discovery.
    pub fallback_summary: &'static str,
}

/// The closed set of tools permitted inside a public `query.batch`.
///
/// This is the single source of truth for batch eligibility. The batch
/// validator aliases this list rather than maintaining a parallel one, so
/// eligibility cannot drift between the registry and the runtime. Mutation
/// tools, repository or operation polling, nested batches, `history.compare`,
/// `query.advanced`, and cross-generation operations are excluded.
pub const BATCH_ELIGIBLE: [McpTool; 11] = [
    McpTool::CodeLocate,
    McpTool::SymbolExplain,
    McpTool::SymbolRelationships,
    McpTool::FlowTrace,
    McpTool::ChangeImpact,
    McpTool::TestsSelect,
    McpTool::ArchitectureOverview,
    McpTool::ArchitectureCycles,
    McpTool::CodeDead,
    McpTool::ContextPack,
    McpTool::SourceRead,
];

/// Reports whether a tool is permitted inside a public batch.
#[must_use]
pub const fn is_batch_eligible(tool: McpTool) -> bool {
    let mut index = 0;
    while index < BATCH_ELIGIBLE.len() {
        if tool_as_u8(BATCH_ELIGIBLE[index]) == tool_as_u8(tool) {
            return true;
        }
        index += 1;
    }
    false
}

/// The canonical capability registry, one entry per tool in catalog order.
pub const CAPABILITIES: [ToolCapability; 19] = build_capabilities();

const fn build_capabilities() -> [ToolCapability; 19] {
    let mut entries = [ToolCapability {
        tool: McpTool::RepoIndex,
        contract_version: MCP_SCHEMA_VERSION,
        batch_eligible: false,
        explain_supported: false,
        handler_available: false,
        status: CapabilityStatus::Blocked,
        fallback_summary: "",
    }; 19];
    let mut index = 0;
    while index < McpTool::ALL.len() {
        let tool = McpTool::ALL[index];
        entries[index] = ToolCapability {
            tool,
            contract_version: MCP_SCHEMA_VERSION,
            batch_eligible: is_batch_eligible(tool),
            explain_supported: false,
            handler_available: true,
            status: tool_status(tool),
            fallback_summary: tool_fallback_summary(tool),
        };
        index += 1;
    }
    entries
}

const fn tool_status(_tool: McpTool) -> CapabilityStatus {
    // Every public tool currently ships as a bounded process-local first slice:
    // a handler exists and is schema-valid, but full acceptance is blocked
    // pending the remediation work tracked by the capability matrix. No tool is
    // marked Implemented until process evidence accepts it, and none is marked
    // Blocked because every tool has a reachable bounded handler.
    CapabilityStatus::FallbackLimited
}

const fn tool_fallback_summary(tool: McpTool) -> &'static str {
    match tool {
        McpTool::RepoIndex => "bounded generation creation; durable publication inactive",
        McpTool::RepoStatus => "bounded process-local status; freshness and coverage structural",
        McpTool::RepoList => "bounded catalog listing",
        McpTool::OperationStatus => "bounded operation read and cancel",
        McpTool::CodeLocate => "bounded structural and lexical matching",
        McpTool::SymbolExplain => "bounded semantic evidence",
        McpTool::SymbolRelationships => "bounded typed relationships",
        McpTool::FlowTrace => "bounded path tracing",
        McpTool::ChangeImpact => "bounded change mapping",
        McpTool::TestsSelect => "bounded test ranking",
        McpTool::ArchitectureOverview => "bounded architecture map",
        McpTool::ArchitectureCycles => "bounded cycle detection",
        McpTool::CodeDead => "bounded dead-code candidates",
        McpTool::HistoryCompare => "bounded structural comparison",
        McpTool::PlanChange => "bounded change planning",
        McpTool::ContextPack => "bounded evidence assembly under a token budget",
        McpTool::SourceRead => "bounded source ranges as untrusted data",
        McpTool::QueryAdvanced => "bounded safe-AST query",
        McpTool::QueryBatch => "bounded batched reads under one generation and shared budget",
    }
}

/// Const-compatible discriminant comparison for field-less tools.
const fn tool_as_u8(tool: McpTool) -> u8 {
    tool as u8
}

#[cfg(test)]
mod tests {
    use super::{BATCH_ELIGIBLE, CAPABILITIES, CapabilityStatus, McpTool, is_batch_eligible};

    #[test]
    fn registry_covers_exactly_the_nineteen_catalog_tools_in_order() {
        assert_eq!(CAPABILITIES.len(), 19);
        for (entry, tool) in CAPABILITIES.iter().zip(McpTool::ALL) {
            assert_eq!(entry.tool, tool, "registry order must match the catalog");
        }
    }

    #[test]
    fn every_entry_targets_the_current_contract_version() {
        for entry in &CAPABILITIES {
            assert_eq!(entry.contract_version, crate::MCP_SCHEMA_VERSION);
        }
    }

    #[test]
    fn batch_eligibility_field_matches_the_single_source() {
        for entry in &CAPABILITIES {
            assert_eq!(
                entry.batch_eligible,
                is_batch_eligible(entry.tool),
                "{} batch flag drifted from the allowlist",
                entry.tool.name()
            );
        }
        assert_eq!(BATCH_ELIGIBLE.len(), 11);
    }

    #[test]
    fn batch_eligible_tools_are_read_only() {
        for tool in BATCH_ELIGIBLE {
            assert!(
                tool.read_only(),
                "{} in batch must be read-only",
                tool.name()
            );
        }
        assert!(!is_batch_eligible(McpTool::RepoIndex));
        assert!(!is_batch_eligible(McpTool::QueryBatch));
        assert!(!is_batch_eligible(McpTool::HistoryCompare));
        assert!(!is_batch_eligible(McpTool::QueryAdvanced));
    }

    #[test]
    fn every_entry_has_a_handler_or_explicit_pre_execution_disposition() {
        for entry in &CAPABILITIES {
            let has_explicit_disposition = matches!(
                entry.status,
                CapabilityStatus::UnsupportedStableError | CapabilityStatus::Blocked
            );
            assert!(
                entry.handler_available || has_explicit_disposition,
                "{} lacks a handler and an explicit disposition",
                entry.tool.name()
            );
            assert!(
                !entry.fallback_summary.is_empty(),
                "{} has an empty fallback summary",
                entry.tool.name()
            );
        }
    }
}
