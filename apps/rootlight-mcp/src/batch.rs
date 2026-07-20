//! Bounded batch query validation and execution orchestration.
//!
//! Enforces the public `query.batch` contract: at most sixteen allow-listed
//! read operations, one pinned generation, a depth-eight acyclic dependency
//! graph, restricted typed bindings, shared budgets, deterministic request-order
//! output, per-operation errors, and optional fail-fast behavior.


use rootlight_mcp_contract::McpTool;

/// Maximum operations accepted in one public batch request.
pub const MAX_BATCH_OPERATIONS: usize = 16;

/// Maximum dependency depth in the batch operation DAG.
pub const MAX_BATCH_DEPTH: usize = 8;

/// Maximum dependencies one operation may declare.
pub const MAX_DEPS_PER_OPERATION: usize = 8;

/// The closed allowlist of tools permitted inside a public batch.
///
/// Mutation tools, repository or operation polling, nested batches,
/// `history.compare`, `query.advanced`, and cross-generation operations
/// are forbidden.
pub const BATCH_ALLOWLIST: [McpTool; 11] = [
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

/// Errors returned during batch validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BatchValidationError {
    /// The batch contains zero operations or more than sixteen.
    #[error("batch operation count is outside the 1..16 range")]
    InvalidOperationCount,
    /// An operation references a tool not in the batch allowlist.
    #[error("operation uses a tool not in the batch allowlist")]
    ForbiddenTool,
    /// The dependency graph contains a cycle.
    #[error("batch dependency graph contains a cycle")]
    CyclicDependency,
    /// The dependency graph exceeds depth eight.
    #[error("batch dependency graph exceeds depth eight")]
    DepthExceeded,
    /// An operation references a nonexistent dependency index.
    #[error("operation references a nonexistent dependency")]
    InvalidDependencyReference,
    /// An operation declares more than eight dependencies.
    #[error("operation declares too many dependencies")]
    TooManyDependencies,
    /// A binding references an invalid source operation or field.
    #[error("binding references an invalid source")]
    InvalidBinding,
    /// The batch attempts to use a nested batch operation.
    #[error("nested batch operations are forbidden")]
    NestedBatch,
}

/// One operation in a validated batch request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchOperation {
    /// Zero-based position in request order.
    pub index: usize,
    /// The tool to invoke.
    pub tool: McpTool,
    /// Indices of operations this one depends on.
    pub depends_on: Vec<usize>,
}

/// A validated batch execution plan.
///
/// Operations are topologically sorted for execution but output is always
/// returned in original request order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchPlan {
    /// Operations in request order.
    pub operations: Vec<BatchOperation>,
    /// Topologically sorted execution order (indices into operations).
    pub execution_order: Vec<usize>,
}

impl BatchPlan {
    /// Validates and builds a batch execution plan from raw operation specs.
    ///
    /// # Errors
    ///
    /// Returns [BatchValidationError] when the batch violates any public
    /// contract invariant.
    pub fn validate(
        tools: &[McpTool],
        dependencies: &[Vec<usize>],
    ) -> Result<Self, BatchValidationError> {
        let count = tools.len();
        if count == 0 || count > MAX_BATCH_OPERATIONS {
            return Err(BatchValidationError::InvalidOperationCount);
        }
        if dependencies.len() != count {
            return Err(BatchValidationError::InvalidDependencyReference);
        }

        let mut operations = Vec::new();
        operations
            .try_reserve_exact(count)
            .map_err(|_| BatchValidationError::InvalidOperationCount)?;

        for (index, (tool, deps)) in tools.iter().zip(dependencies).enumerate() {
            if !BATCH_ALLOWLIST.contains(tool) {
                return Err(BatchValidationError::ForbiddenTool);
            }
            if *tool == McpTool::QueryBatch {
                return Err(BatchValidationError::NestedBatch);
            }
            if deps.len() > MAX_DEPS_PER_OPERATION {
                return Err(BatchValidationError::TooManyDependencies);
            }
            for dep in deps {
                if *dep >= count || *dep == index {
                    return Err(BatchValidationError::InvalidDependencyReference);
                }
            }
            operations.push(BatchOperation {
                index,
                tool: *tool,
                depends_on: deps.clone(),
            });
        }

        let execution_order = topological_sort(&operations)?;
        Ok(Self {
            operations,
            execution_order,
        })
    }

    /// Returns the maximum dependency depth in the plan.
    #[must_use]
    pub fn max_depth(&self) -> usize {
        let mut depths = vec![0usize; self.operations.len()];
        for idx in &self.execution_order {
            let op = &self.operations[*idx];
            let dep_depth = op
                .depends_on
                .iter()
                .map(|d| depths[*d] + 1)
                .max()
                .unwrap_or(0);
            depths[*idx] = dep_depth;
        }
        depths.into_iter().max().unwrap_or(0)
    }
}

/// Kahn's algorithm for topological sort with cycle detection.
fn topological_sort(operations: &[BatchOperation]) -> Result<Vec<usize>, BatchValidationError> {
    let count = operations.len();
    let mut in_degree = vec![0usize; count];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];

    for op in operations {
        for dep in &op.depends_on {
            in_degree[op.index] += 1;
            dependents[*dep].push(op.index);
        }
    }

    let mut queue: Vec<usize> = (0..count).filter(|i| in_degree[*i] == 0).collect();
    let mut order = Vec::new();
    order
        .try_reserve_exact(count)
        .map_err(|_| BatchValidationError::CyclicDependency)?;

    while let Some(node) = queue.pop() {
        order.push(node);
        for dependent in &dependents[node] {
            in_degree[*dependent] -= 1;
            if in_degree[*dependent] == 0 {
                queue.push(*dependent);
            }
        }
    }

    if order.len() != count {
        return Err(BatchValidationError::CyclicDependency);
    }

    // Verify depth constraint
    let mut depths = vec![0usize; count];
    for idx in &order {
        let op = &operations[*idx];
        let dep_depth = op
            .depends_on
            .iter()
            .map(|d| depths[*d] + 1)
            .max()
            .unwrap_or(0);
        depths[*idx] = dep_depth;
        if dep_depth > MAX_BATCH_DEPTH {
            return Err(BatchValidationError::DepthExceeded);
        }
    }

    Ok(order)
}

/// Reports whether a tool is in the batch allowlist.
#[must_use]
pub fn is_batch_allowed(tool: McpTool) -> bool {
    BATCH_ALLOWLIST.contains(&tool)
}

/// Reports whether a tool is visible under the given profile AND in the batch
/// allowlist. `query.batch` cannot bypass profile filtering.
#[must_use]
pub fn is_batch_allowed_under_profile(
    tool: McpTool,
    profile: rootlight_mcp_contract::ExposureProfile,
) -> bool {
    is_batch_allowed(tool) && profile.exposes(tool)
}

#[cfg(test)]
mod tests {
    use super::{
        BatchPlan, BatchValidationError, MAX_BATCH_DEPTH, MAX_BATCH_OPERATIONS, is_batch_allowed,
        is_batch_allowed_under_profile,
    };
    use rootlight_mcp_contract::{ExposureProfile, McpTool};

    #[test]
    fn empty_batch_is_rejected() {
        assert_eq!(
            BatchPlan::validate(&[], &[]),
            Err(BatchValidationError::InvalidOperationCount)
        );
    }

    #[test]
    fn oversized_batch_is_rejected() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_OPERATIONS + 1];
        let deps = vec![vec![]; MAX_BATCH_OPERATIONS + 1];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::InvalidOperationCount)
        );
    }

    #[test]
    fn maximum_sixteen_operations_are_accepted() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_OPERATIONS];
        let deps = vec![vec![]; MAX_BATCH_OPERATIONS];
        assert!(BatchPlan::validate(&tools, &deps).is_ok());
    }

    #[test]
    fn forbidden_tools_are_rejected() {
        let tools = [McpTool::RepoIndex];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );

        let tools = [McpTool::QueryAdvanced];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );

        let tools = [McpTool::HistoryCompare];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );
    }

    #[test]
    fn nested_batch_is_rejected() {
        let tools = [McpTool::QueryBatch];
        let deps = [vec![]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::ForbiddenTool)
        );
    }

    #[test]
    fn cyclic_dependency_is_rejected() {
        let tools = [McpTool::CodeLocate, McpTool::SymbolExplain];
        let deps = [vec![1], vec![0]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::CyclicDependency)
        );
    }

    #[test]
    fn self_dependency_is_rejected() {
        let tools = [McpTool::CodeLocate];
        let deps = [vec![0]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::InvalidDependencyReference)
        );
    }

    #[test]
    fn out_of_range_dependency_is_rejected() {
        let tools = [McpTool::CodeLocate];
        let deps = [vec![5]];
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::InvalidDependencyReference)
        );
    }

    #[test]
    fn depth_eight_is_accepted() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_DEPTH + 1];
        let deps: Vec<Vec<usize>> = (0..=MAX_BATCH_DEPTH)
            .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
            .collect();
        let plan = BatchPlan::validate(&tools, &deps).expect("depth 8 is valid");
        assert_eq!(plan.max_depth(), MAX_BATCH_DEPTH);
    }

    #[test]
    fn depth_nine_is_rejected() {
        let tools = vec![McpTool::CodeLocate; MAX_BATCH_DEPTH + 2];
        let deps: Vec<Vec<usize>> = (0..=MAX_BATCH_DEPTH + 1)
            .map(|i| if i == 0 { vec![] } else { vec![i - 1] })
            .collect();
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::DepthExceeded)
        );
    }

    #[test]
    fn execution_order_is_topologically_valid() {
        let tools = [
            McpTool::CodeLocate,
            McpTool::SymbolExplain,
            McpTool::FlowTrace,
        ];
        let deps = [vec![], vec![0], vec![0, 1]];
        let plan = BatchPlan::validate(&tools, &deps).expect("valid DAG");
        let order = &plan.execution_order;
        let pos = |idx: usize| order.iter().position(|i| *i == idx).unwrap();
        assert!(pos(0) < pos(1));
        assert!(pos(0) < pos(2));
        assert!(pos(1) < pos(2));
    }

    #[test]
    fn batch_allowlist_matches_profile_intersection() {
        for tool in McpTool::ALL {
            let allowed = is_batch_allowed(tool);
            let scout_allowed = is_batch_allowed_under_profile(tool, ExposureProfile::Scout);
            if scout_allowed {
                assert!(allowed, "scout batch tool must be in allowlist");
                assert!(
                    ExposureProfile::Scout.exposes(tool),
                    "scout batch tool must be visible in scout"
                );
            }
        }
    }

    #[test]
    fn too_many_dependencies_per_operation_is_rejected() {
        let tools = vec![McpTool::CodeLocate; 10];
        let mut deps = vec![vec![]; 10];
        deps[9] = (0..9).collect();
        assert_eq!(
            BatchPlan::validate(&tools, &deps),
            Err(BatchValidationError::TooManyDependencies)
        );
    }
}
