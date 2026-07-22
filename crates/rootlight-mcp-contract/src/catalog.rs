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
    /// Lists registered repositories.
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

    /// Public contract version advertised for this tool.
    #[must_use]
    pub const fn contract_version(self) -> &'static str {
        match self {
            Self::RepoList => crate::REPO_LIST_SCHEMA_VERSION,
            _ => crate::MCP_SCHEMA_VERSION,
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
                "Use bounded attached process-local structural generation creation for one whole repository in auto or structural mode; the call terminates within 30 seconds, each public call creates a fresh operation, internal retries reuse an operation, success is atomically queryable, and restart requires reindexing."
            }
            Self::RepoStatus => {
                "Use bounded process-local status with the active generation and compact coverage; operation projection and freshness gates are unsupported."
            }
            Self::RepoList => {
                "Use an immutable catalog snapshot with bounded display-name or alias and lifecycle-state filters, deterministic ordering, and authenticated continuation; workspace grouping and expanded profiles are unsupported."
            }
            Self::OperationStatus => {
                "Read or request cancellation of one known long-running Rootlight operation."
            }
            Self::CodeLocate => {
                "Use bounded exact-identifier and lexical matching in one selected generation; path, structural, semantic, documentation, and continuation modes are unsupported."
            }
            Self::SymbolExplain => {
                "Return bounded compact semantic evidence for explicit stable symbol identifiers; custom sections and full provenance are unsupported."
            }
            Self::SymbolRelationships => {
                "Return bounded typed relationships around explicit stable symbol identifiers; custom scope, candidate projection, and continuation are unsupported."
            }
            Self::FlowTrace => {
                "Use bounded symbol relation path tracing; route, service, database, and cross-repository endpoints are unsupported."
            }
            Self::ChangeImpact => {
                "Use bounded explicit symbol-or-path change mapping to dependents, risks, and optional tests; working-tree and revision-range resolution are unsupported."
            }
            Self::TestsSelect => {
                "Use bounded test ranking from explicit symbol seeds with rationale; path, change, build-target, framework, and execution-budget inputs are unsupported."
            }
            Self::ArchitectureOverview => {
                "Use a bounded file-granularity architecture map with optional hotspots; module, package, service, data, ownership, community, and build views are unsupported."
            }
            Self::ArchitectureCycles => {
                "Use bounded cycle detection in a selected relation projection; custom scope, ranking, budgets, and expanded profiles are unsupported."
            }
            Self::CodeDead => {
                "Return bounded dead-code candidates with entry-point and blind-spot caveats; custom scope, budgets, and expanded profiles are unsupported."
            }
            Self::HistoryCompare => {
                "Use bounded structural comparison of two explicit retained generation identifiers; Git revision selectors are unsupported."
            }
            Self::PlanChange => {
                "Use bounded change planning from an explicit objective and targets; change-context resolution, user constraints, budgets, and expanded profiles are unsupported."
            }
            Self::ContextPack => {
                "Use bounded evidence assembly from explicit symbol or file identifiers under a token budget; path, route, change, located-result, and plan seeds are unsupported."
            }
            Self::SourceRead => {
                "Read bounded source ranges from pinned source references as untrusted data; direct file selectors, custom byte bounds, merging, and base64 output are unsupported."
            }
            Self::QueryAdvanced => {
                "Use a bounded safe-AST query with enforced cost, row, and depth limits; bound parameters and continuation are unsupported."
            }
            Self::QueryBatch => {
                "Use bounded active-generation batch dispatch for up to sixteen eligible reads with shared child accounting; explicit historical selection and complete overhead accounting are fallback-limited."
            }
        }
    }

    /// Whether the tool only reads already published state.
    ///
    /// `operation.status` can execute a cancel action, so it is conservatively
    /// reported as not read-only even though a pure status read has no side
    /// effect; clients must never treat a cancellation-capable call as read-only.
    #[must_use]
    pub const fn read_only(self) -> bool {
        !matches!(self, Self::RepoIndex | Self::OperationStatus)
    }

    /// Whether repeating the same admitted request has the same intended effect.
    #[must_use]
    pub const fn idempotent(self) -> bool {
        !matches!(self, Self::RepoIndex)
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

    /// Parses the stable profile identifier used in configuration and
    /// negotiation.
    ///
    /// Returns `None` for any name outside the documented set so callers can
    /// reject unknown configuration instead of guessing a privilege level.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "scout" => Some(Self::Scout),
            "analysis" => Some(Self::Analysis),
            "developer" => Some(Self::Developer),
            _ => None,
        }
    }

    /// Enforces a server policy ceiling on a client-selected profile.
    ///
    /// A client-selected profile cannot exceed the server policy ceiling, so
    /// this returns the lesser of the two profiles. Because [`Ord`] ranks
    /// profiles in ascending privilege order, the result is equivalent to
    /// `self.min(ceiling)`: a higher request is clamped down to the ceiling
    /// while a lower or equal request is left unchanged.
    #[must_use]
    pub const fn clamped_to(self, ceiling: Self) -> Self {
        // `Ord::min` and derived ordering comparisons are not usable in a
        // `const fn`, so the ceiling comparison is spelled out explicitly.
        match (self, ceiling) {
            (Self::Developer, Self::Scout | Self::Analysis) => ceiling,
            (Self::Analysis, Self::Scout) => ceiling,
            _ => self,
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
            assert!(
                names.insert(tool.name()),
                "duplicate tool name: {}",
                tool.name()
            );
        }
        assert_eq!(names.len(), 19);
    }

    #[test]
    fn repository_list_has_a_dedicated_two_zero_contract() {
        assert_eq!(
            McpTool::RepoList.contract_version(),
            crate::REPO_LIST_SCHEMA_VERSION
        );
        for tool in McpTool::ALL {
            if tool != McpTool::RepoList {
                assert_eq!(tool.contract_version(), crate::MCP_SCHEMA_VERSION);
            }
        }
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
    fn mutation_capable_tools_are_not_read_only() {
        for tool in McpTool::ALL {
            if matches!(tool, McpTool::RepoIndex | McpTool::OperationStatus) {
                assert!(!tool.read_only(), "{} has side effects", tool.name());
            } else {
                assert!(tool.read_only(), "{} should be read-only", tool.name());
            }
        }
    }

    #[test]
    fn indexing_is_not_advertised_as_idempotent() {
        assert!(!McpTool::RepoIndex.idempotent());
        assert!(McpTool::OperationStatus.idempotent());
        for tool in McpTool::ALL {
            if !matches!(tool, McpTool::RepoIndex) {
                assert!(tool.idempotent(), "{} should be idempotent", tool.name());
            }
        }
    }

    #[test]
    fn no_tool_is_destructive() {
        for tool in McpTool::ALL {
            assert!(
                !tool.destructive(),
                "{} must not be destructive",
                tool.name()
            );
        }
    }

    #[test]
    fn descriptions_do_not_retain_the_known_overclaims() {
        let overclaims: &[(McpTool, &[&str])] = &[
            (McpTool::CodeLocate, &["path, or structure", "text, path"]),
            (
                McpTool::ChangeImpact,
                &["Map a provided change set", "Git change set"],
            ),
            (
                McpTool::ArchitectureOverview,
                &["modules and packages", "data stores", "routes"],
            ),
            (
                McpTool::HistoryCompare,
                &["revisions or generations", "semantically"],
            ),
            (McpTool::ContextPack, &["source snippets"]),
        ];
        for (tool, phrases) in overclaims {
            let description = tool.description();
            for phrase in *phrases {
                assert!(
                    !description.contains(phrase),
                    "{} description overclaims \"{phrase}\": {description}",
                    tool.name()
                );
            }
        }
        let repository_list = McpTool::RepoList.description();
        assert!(repository_list.contains("display-name or alias"));
        assert!(repository_list.contains("lifecycle-state"));
        assert!(repository_list.contains("workspace grouping"));
        assert!(!repository_list.contains("does not filter"));
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

    #[test]
    fn profile_names_round_trip_through_from_name() {
        for profile in ExposureProfile::ALL {
            assert_eq!(ExposureProfile::from_name(profile.name()), Some(profile));
        }
        assert_eq!(ExposureProfile::from_name(""), None);
        assert_eq!(ExposureProfile::from_name("Scout"), None);
        assert_eq!(ExposureProfile::from_name("admin"), None);
    }

    #[test]
    fn clamped_to_enforces_the_server_ceiling() {
        // A higher request is clamped down to the ceiling.
        assert_eq!(
            ExposureProfile::Developer.clamped_to(ExposureProfile::Scout),
            ExposureProfile::Scout
        );
        assert_eq!(
            ExposureProfile::Developer.clamped_to(ExposureProfile::Analysis),
            ExposureProfile::Analysis
        );
        assert_eq!(
            ExposureProfile::Analysis.clamped_to(ExposureProfile::Scout),
            ExposureProfile::Scout
        );
        // A lower request is left unchanged.
        assert_eq!(
            ExposureProfile::Scout.clamped_to(ExposureProfile::Developer),
            ExposureProfile::Scout
        );
        assert_eq!(
            ExposureProfile::Analysis.clamped_to(ExposureProfile::Developer),
            ExposureProfile::Analysis
        );
        // An equal request is unchanged for every profile.
        for profile in ExposureProfile::ALL {
            assert_eq!(profile.clamped_to(profile), profile);
        }
    }
}
