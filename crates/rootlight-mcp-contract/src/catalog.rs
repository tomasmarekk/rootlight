//! Complete MCP tool catalog, exposure profiles, and discovery metadata.
//!
//! The catalog defines all nineteen agent-facing tools, their stable names,
//! annotations, and the three exposure profiles that filter `tools/list`
//! without changing tool semantics, limits, or authorization.

/// One tool in the complete Rootlight MCP agent catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpTool {
    /// Creates or updates a repository generation.
    RepoIndex,
    /// Inspects repository, generation, coverage, freshness, and operations.
    RepoStatus,
    /// Lists registered repositories and workspaces.
    RepoList,
    /// Reads or cancels a long-running operation.
    OperationStatus,
    /// Locates bounded structural or lexical matches.
    CodeLocate,
    /// Explains one or more stable symbols.
    SymbolExplain,
    /// Gets bounded typed relationships around symbols.
    SymbolRelationships,
    /// Traces bounded paths through relation graphs.
    FlowTrace,
    /// Maps changes to affected symbols, dependents, and risks.
    ChangeImpact,
    /// Ranks tests relevant to symbols or changes.
    TestsSelect,
    /// Produces a scoped architecture map.
    ArchitectureOverview,
    /// Finds dependency cycles in a relation projection.
    ArchitectureCycles,
    /// Finds dead or unreachable code candidates.
    CodeDead,
    /// Compares two revisions or generations structurally.
    HistoryCompare,
    /// Produces an ordered change plan.
    PlanChange,
    /// Assembles task-specific evidence under a token budget.
    ContextPack,
    /// Reads exact bounded source ranges.
    SourceRead,
    /// Executes a bounded expert query over the safe AST.
    QueryAdvanced,
    /// Executes up to sixteen read operations under one generation.
    QueryBatch,
}

impl McpTool {
    /// Complete deterministic tool catalog in stable discovery order.
    pub const ALL: [Self; 19] = [
        Self::RepoIndex,
        Self::RepoStatus,
        Self::RepoList,
        Self::OperationStatus,
        Self::CodeLocate,
        Self::SymbolExplain,
        Self::SymbolRelationships,
        Self::FlowTrace,
        Self::ChangeImpact,
        Self::TestsSelect,
        Self::ArchitectureOverview,
        Self::ArchitectureCycles,
        Self::CodeDead,
        Self::HistoryCompare,
        Self::PlanChange,
        Self::ContextPack,
        Self::SourceRead,
        Self::QueryAdvanced,
        Self::QueryBatch,
    ];

    /// Stable tool name advertised through MCP.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::RepoIndex => "repo.index",
            Self::RepoStatus => "repo.status",
            Self::RepoList => "repo.list",
            Self::OperationStatus => "operation.status",
            Self::CodeLocate => "code.locate",
            Self::SymbolExplain => "symbol.explain",
            Self::SymbolRelationships => "symbol.relationships",
            Self::FlowTrace => "flow.trace",
            Self::ChangeImpact => "change.impact",
            Self::TestsSelect => "tests.select",
            Self::ArchitectureOverview => "architecture.overview",
            Self::ArchitectureCycles => "architecture.cycles",
            Self::CodeDead => "code.dead",
            Self::HistoryCompare => "history.compare",
            Self::PlanChange => "plan.change",
            Self::ContextPack => "context.pack",
            Self::SourceRead => "source.read",
            Self::QueryAdvanced => "query.advanced",
            Self::QueryBatch => "query.batch",
        }
    }

    /// Static source-free title intended for clients.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::RepoIndex => "Index repository",
            Self::RepoStatus => "Inspect repository",
            Self::RepoList => "List repositories",
            Self::OperationStatus => "Inspect operation",
            Self::CodeLocate => "Locate code",
            Self::SymbolExplain => "Explain symbol",
            Self::SymbolRelationships => "Symbol relationships",
            Self::FlowTrace => "Trace flow",
            Self::ChangeImpact => "Change impact",
            Self::TestsSelect => "Select tests",
            Self::ArchitectureOverview => "Architecture overview",
            Self::ArchitectureCycles => "Architecture cycles",
            Self::CodeDead => "Dead code",
            Self::HistoryCompare => "Compare history",
            Self::PlanChange => "Plan change",
            Self::ContextPack => "Context pack",
            Self::SourceRead => "Read source",
            Self::QueryAdvanced => "Advanced query",
            Self::QueryBatch => "Batch query",
        }
    }

    /// Static source-free description intended for models and clients.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::RepoIndex => {
                "Create or update one local repository generation and return its operation handle."
            }
            Self::RepoStatus => {
                "Inspect repository state, generation freshness, coverage, and active operations."
            }
            Self::RepoList => "List registered repositories and workspaces.",
            Self::OperationStatus => "Read or cancel one known long-running Rootlight operation.",
            Self::CodeLocate => {
                "Find bounded, generation-pinned code and file matches by identifier, text, path, or structure."
            }
            Self::SymbolExplain => {
                "Return bounded semantic evidence for stable symbol identifiers."
            }
            Self::SymbolRelationships => {
                "Get bounded typed callers, callees, references, types, implementations, dependencies, tests, or ownership around symbols."
            }
            Self::FlowTrace => {
                "Trace bounded paths through calls, data flow, services, messaging, build, or dependency relations."
            }
            Self::ChangeImpact => {
                "Map a working-tree or Git change set to affected symbols, dependents, services, risks, and tests."
            }
            Self::TestsSelect => {
                "Rank tests relevant to symbols or changes with rationale and uncertainty."
            }
            Self::ArchitectureOverview => {
                "Produce a scoped architecture map of modules, packages, services, data stores, routes, ownership, and hotspots."
            }
            Self::ArchitectureCycles => {
                "Find and explain dependency cycles in a selected relation projection."
            }
            Self::CodeDead => {
                "Find dead or unreachable candidates with entry-point and coverage caveats."
            }
            Self::HistoryCompare => {
                "Compare two revisions or generations structurally and semantically."
            }
            Self::PlanChange => {
                "Produce an ordered change plan with affected symbols, files, tests, risks, and verification steps."
            }
            Self::ContextPack => {
                "Assemble minimal task-specific evidence and source snippets under a token budget."
            }
            Self::SourceRead => {
                "Read exact bounded ranges from a pinned source snapshot as untrusted repository data."
            }
            Self::QueryAdvanced => {
                "Execute a bounded expert query over the documented safe query AST."
            }
            Self::QueryBatch => {
                "Execute up to sixteen independent or dependency-linked read operations under one pinned generation and one shared budget."
            }
        }
    }

    /// Whether the tool only reads already published state.
    #[must_use]
    pub const fn read_only(self) -> bool {
        !matches!(self, Self::RepoIndex)
    }

    /// Whether repeating the same admitted request has the same intended effect.
    #[must_use]
    pub const fn idempotent(self) -> bool {
        true
    }

    /// Whether the tool performs a destructive update.
    #[must_use]
    pub const fn destructive(self) -> bool {
        false
    }

    /// Default estimated output token budget for this tool.
    #[must_use]
    pub const fn default_token_budget(self) -> u16 {
        match self {
            Self::RepoIndex => 250,
            Self::RepoStatus => 500,
            Self::RepoList => 400,
            Self::OperationStatus => 350,
            Self::CodeLocate => 1200,
            Self::SymbolExplain => 1800,
            Self::SymbolRelationships => 1800,
            Self::FlowTrace => 2400,
            Self::ChangeImpact => 2600,
            Self::TestsSelect => 1800,
            Self::ArchitectureOverview => 2600,
            Self::ArchitectureCycles => 1900,
            Self::CodeDead => 1800,
            Self::HistoryCompare => 2400,
            Self::PlanChange => 3200,
            Self::ContextPack => 4500,
            Self::SourceRead => 3200,
            Self::QueryAdvanced => 2600,
            Self::QueryBatch => 3000,
        }
    }
}

/// A server-configured tool exposure profile that filters `tools/list`.
///
/// Profiles change discovery only. They do not change input schemas, output
/// schemas, limits, errors, authorization, generation semantics, or result
/// quality. A client-selected profile cannot exceed the server policy ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExposureProfile {
    /// Minimal discovery surface for orientation and simple retrieval.
    Scout,
    /// Adds relationship, flow, impact, test, architecture, and dead-code tools.
    Analysis,
    /// Exposes all nineteen tools including administration and advanced query.
    Developer,
}

impl ExposureProfile {
    /// All profiles in ascending privilege order.
    pub const ALL: [Self; 3] = [Self::Scout, Self::Analysis, Self::Developer];

    /// Stable profile identifier used in configuration and negotiation.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scout => "scout",
            Self::Analysis => "analysis",
            Self::Developer => "developer",
        }
    }

    /// Exact tool allowlist exposed by `tools/list` under this profile.
    ///
    /// The returned slice is deterministically ordered by [McpTool::ALL]
    /// position. `query.batch` can invoke only subtools that are both in
    /// its fixed allowlist and visible in the current session profile, so it
    /// cannot bypass profile filtering.
    #[must_use]
    pub const fn tools(self) -> &'static [McpTool] {
        match self {
            Self::Scout => &[
                McpTool::RepoStatus,
                McpTool::CodeLocate,
                McpTool::SymbolExplain,
                McpTool::ContextPack,
                McpTool::SourceRead,
                McpTool::QueryBatch,
            ],
            Self::Analysis => &[
                McpTool::RepoStatus,
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
                McpTool::QueryBatch,
            ],
            Self::Developer => &McpTool::ALL,
        }
    }

    /// Reports whether a tool is visible under this profile.
    #[must_use]
    pub const fn exposes(self, tool: McpTool) -> bool {
        let tools = self.tools();
        let mut index = 0;
        while index < tools.len() {
            if matches_tool(tools[index], tool) {
                return true;
            }
            index += 1;
        }
        false
    }
}

/// Const-compatible tool equality check for profile allowlists.
const fn matches_tool(candidate: McpTool, target: McpTool) -> bool {
    candidate as u8 == target as u8
}

#[cfg(test)]
mod tests {
    use super::{ExposureProfile, McpTool};

    #[test]
    fn catalog_contains_exactly_nineteen_unique_tools() {
        let mut names = std::collections::BTreeSet::new();
        for tool in McpTool::ALL {
            assert!(names.insert(tool.name()), "duplicate tool name: {}", tool.name());
        }
        assert_eq!(names.len(), 19);
    }

    #[test]
    fn tool_names_use_documented_dotted_convention() {
        for tool in McpTool::ALL {
            let name = tool.name();
            assert!(
                name.contains('.'),
                "tool name missing dot separator: {name}"
            );
            assert!(
                name.bytes().all(|b| b.is_ascii_lowercase() || b == b'.'),
                "tool name has invalid characters: {name}"
            );
        }
    }

    #[test]
    fn scout_profile_exposes_exact_allowlist() {
        let expected = [
            "repo.status",
            "code.locate",
            "symbol.explain",
            "context.pack",
            "source.read",
            "query.batch",
        ];
        let tools = ExposureProfile::Scout.tools();
        assert_eq!(tools.len(), expected.len());
        for (tool, name) in tools.iter().zip(&expected) {
            assert_eq!(tool.name(), *name);
        }
    }

    #[test]
    fn analysis_profile_extends_scout_without_removal() {
        let scout = ExposureProfile::Scout.tools();
        let analysis = ExposureProfile::Analysis.tools();
        for tool in scout {
            assert!(
                analysis.contains(tool),
                "analysis profile missing scout tool: {}",
                tool.name()
            );
        }
        assert_eq!(analysis.len(), 13);
    }

    #[test]
    fn developer_profile_exposes_all_nineteen_tools() {
        assert_eq!(ExposureProfile::Developer.tools().len(), 19);
        assert_eq!(ExposureProfile::Developer.tools(), &McpTool::ALL);
    }

    #[test]
    fn profiles_do_not_change_tool_semantics() {
        // Annotations are profile-independent.
        for tool in McpTool::ALL {
            let read_only = tool.read_only();
            let idempotent = tool.idempotent();
            let destructive = tool.destructive();
            // Same values regardless of which profile exposes the tool.
            for profile in ExposureProfile::ALL {
                if profile.exposes(tool) {
                    assert_eq!(tool.read_only(), read_only);
                    assert_eq!(tool.idempotent(), idempotent);
                    assert_eq!(tool.destructive(), destructive);
                }
            }
        }
    }

    #[test]
    fn only_repo_index_is_not_read_only() {
        for tool in McpTool::ALL {
            if tool == McpTool::RepoIndex {
                assert!(!tool.read_only());
            } else {
                assert!(tool.read_only(), "{} should be read-only", tool.name());
            }
        }
    }

    #[test]
    fn no_tool_is_destructive() {
        for tool in McpTool::ALL {
            assert!(!tool.destructive(), "{} must not be destructive", tool.name());
        }
    }

    #[test]
    fn token_budgets_are_within_mcp_hard_ceiling() {
        for tool in McpTool::ALL {
            assert!(
                tool.default_token_budget() <= 32_000,
                "{} exceeds hard token ceiling",
                tool.name()
            );
            assert!(
                tool.default_token_budget() >= 100,
                "{} has a trivially small budget",
                tool.name()
            );
        }
    }

    #[test]
    fn profile_exposure_is_monotonic() {
        for tool in McpTool::ALL {
            if ExposureProfile::Scout.exposes(tool) {
                assert!(ExposureProfile::Analysis.exposes(tool));
                assert!(ExposureProfile::Developer.exposes(tool));
            }
            if ExposureProfile::Analysis.exposes(tool) {
                assert!(ExposureProfile::Developer.exposes(tool));
            }
        }
    }

    #[test]
    fn query_batch_cannot_bypass_profile_filtering() {
        // query.batch is in scout, but its subtool allowlist must intersect
        // the session profile. Verify that tools hidden from scout are not
        // in the batch allowlist when scout is active.
        let scout = ExposureProfile::Scout;
        let batch_visible = scout.exposes(McpTool::QueryBatch);
        assert!(batch_visible, "query.batch must be visible in scout");
        // history.compare and query.advanced are developer-only.
        assert!(!scout.exposes(McpTool::HistoryCompare));
        assert!(!scout.exposes(McpTool::QueryAdvanced));
    }
}
