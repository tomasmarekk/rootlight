//! Safe advanced query AST validation and cost estimation.
//!
//! The advanced query interface accepts only a typed, allow-listed AST.
//! SQL, Cypher strings, shell fragments, arbitrary regex, arbitrary code,
//! and unbounded recursion are structurally impossible because the AST
//! grammar does not represent them. Every query is statically cost-bounded
//! before execution.

/// Maximum AST depth accepted by the validator.
pub const MAX_AST_DEPTH: usize = 5;

/// Maximum rows a single advanced query may return.
pub const MAX_ADVANCED_ROWS: usize = 1_000;

/// Maximum traversal facts a single advanced query may examine.
pub const MAX_ADVANCED_TRAVERSAL: usize = 100_000;

/// Maximum estimated cost units before a query is rejected.
pub const MAX_ESTIMATED_COST: u64 = 1_000_000;

/// Errors returned during advanced query validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdvancedQueryError {
    /// The AST exceeds the maximum nesting depth.
    #[error("query AST exceeds the maximum depth")]
    DepthExceeded,
    /// The AST contains an operator not in the allowlist.
    #[error("query AST contains a forbidden operator")]
    ForbiddenOperator,
    /// The requested row limit exceeds the hard ceiling.
    #[error("requested row limit exceeds the hard ceiling")]
    RowLimitExceeded,
    /// The requested traversal limit exceeds the hard ceiling.
    #[error("requested traversal limit exceeds the hard ceiling")]
    TraversalLimitExceeded,
    /// The static cost estimate exceeds the maximum.
    #[error("static cost estimate exceeds the maximum")]
    CostExceeded,
    /// The AST is structurally malformed.
    #[error("query AST is structurally malformed")]
    Malformed,
    /// A type mismatch was detected during static checking.
    #[error("query AST has a type mismatch")]
    TypeMismatch,
}

/// Allow-listed query operators.
///
/// Only these operators can appear in a valid advanced query AST.
/// The grammar structurally excludes SQL, Cypher, shell, arbitrary
/// regex, arbitrary code, and unbounded recursion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryOperator {
    /// Full symbol or file scan with optional kind filter.
    Scan,
    /// Predicate-based row filtering.
    Filter,
    /// Column selection and renaming.
    Project,
    /// Inner join on typed equality.
    Join,
    /// Count, sum, min, max aggregation.
    Aggregate,
    /// Bounded graph traversal along typed edges.
    Traverse,
    /// Deterministic ordering by typed keys.
    Sort,
    /// Row count limitation.
    Limit,
}

impl QueryOperator {
    /// All allow-listed operators.
    pub const ALL: [Self; 8] = [
        Self::Scan,
        Self::Filter,
        Self::Project,
        Self::Join,
        Self::Aggregate,
        Self::Traverse,
        Self::Sort,
        Self::Limit,
    ];

    /// Base cost weight for static estimation.
    #[must_use]
    pub const fn base_cost(self) -> u64 {
        match self {
            Self::Scan => 100,
            Self::Filter => 10,
            Self::Project => 5,
            Self::Join => 500,
            Self::Aggregate => 50,
            Self::Traverse => 200,
            Self::Sort => 20,
            Self::Limit => 1,
        }
    }
}

/// A validated advanced query plan with static cost estimate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvancedQueryPlan {
    /// Operators in execution order (innermost first).
    pub operators: Vec<QueryOperator>,
    /// Maximum rows requested.
    pub max_rows: usize,
    /// Maximum traversal facts requested.
    pub max_traversal: usize,
    /// Static cost estimate.
    pub estimated_cost: u64,
    /// Maximum nesting depth observed.
    pub depth: usize,
}

impl AdvancedQueryPlan {
    /// Validates an advanced query from its operator sequence and limits.
    ///
    /// # Errors
    ///
    /// Returns [AdvancedQueryError] when the query violates any safety
    /// or resource invariant.
    pub fn validate(
        operators: &[QueryOperator],
        max_rows: usize,
        max_traversal: usize,
        depth: usize,
    ) -> Result<Self, AdvancedQueryError> {
        if depth > MAX_AST_DEPTH {
            return Err(AdvancedQueryError::DepthExceeded);
        }
        if operators.is_empty() {
            return Err(AdvancedQueryError::Malformed);
        }
        if max_rows > MAX_ADVANCED_ROWS {
            return Err(AdvancedQueryError::RowLimitExceeded);
        }
        if max_traversal > MAX_ADVANCED_TRAVERSAL {
            return Err(AdvancedQueryError::TraversalLimitExceeded);
        }

        let estimated_cost = operators
            .iter()
            .fold(0u64, |acc, op| acc.saturating_add(op.base_cost()))
            .saturating_mul(u64::try_from(max_rows).unwrap_or(u64::MAX) / 100 + 1);

        if estimated_cost > MAX_ESTIMATED_COST {
            return Err(AdvancedQueryError::CostExceeded);
        }

        Ok(Self {
            operators: operators.to_vec(),
            max_rows,
            max_traversal,
            estimated_cost,
            depth,
        })
    }

    /// Returns a human-readable plan explanation for the `explain` flag.
    #[must_use]
    pub fn explain(&self) -> String {
        let ops: Vec<&str> = self
            .operators
            .iter()
            .map(|op| format!("{op:?}").leak() as &str)
            .collect();
        format!(
            "plan: [{}] depth={} rows<={} traversal<={} cost~={}",
            ops.join(" -> "),
            self.depth,
            self.max_rows,
            self.max_traversal,
            self.estimated_cost
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdvancedQueryError, AdvancedQueryPlan, MAX_ADVANCED_ROWS, MAX_ADVANCED_TRAVERSAL,
        MAX_AST_DEPTH, MAX_ESTIMATED_COST, QueryOperator,
    };

    #[test]
    fn simple_scan_with_limit_is_valid() {
        let plan = AdvancedQueryPlan::validate(
            &[QueryOperator::Scan, QueryOperator::Limit],
            100,
            10_000,
            2,
        )
        .expect("simple query is valid");
        assert_eq!(plan.operators.len(), 2);
        assert_eq!(plan.max_rows, 100);
        assert!(plan.estimated_cost <= MAX_ESTIMATED_COST);
    }

    #[test]
    fn empty_operator_list_is_malformed() {
        assert_eq!(
            AdvancedQueryPlan::validate(&[], 100, 10_000, 1),
            Err(AdvancedQueryError::Malformed)
        );
    }

    #[test]
    fn excessive_depth_is_rejected() {
        assert_eq!(
            AdvancedQueryPlan::validate(&[QueryOperator::Scan], 100, 10_000, MAX_AST_DEPTH + 1),
            Err(AdvancedQueryError::DepthExceeded)
        );
    }

    #[test]
    fn maximum_depth_is_accepted() {
        assert!(
            AdvancedQueryPlan::validate(&[QueryOperator::Scan], 100, 10_000, MAX_AST_DEPTH).is_ok()
        );
    }

    #[test]
    fn excessive_row_limit_is_rejected() {
        assert_eq!(
            AdvancedQueryPlan::validate(&[QueryOperator::Scan], MAX_ADVANCED_ROWS + 1, 10_000, 1),
            Err(AdvancedQueryError::RowLimitExceeded)
        );
    }

    #[test]
    fn excessive_traversal_is_rejected() {
        assert_eq!(
            AdvancedQueryPlan::validate(
                &[QueryOperator::Traverse],
                100,
                MAX_ADVANCED_TRAVERSAL + 1,
                1
            ),
            Err(AdvancedQueryError::TraversalLimitExceeded)
        );
    }

    #[test]
    fn all_operators_have_positive_base_cost() {
        for op in QueryOperator::ALL {
            assert!(op.base_cost() > 0, "{op:?} must have positive cost");
        }
    }

    #[test]
    fn explain_produces_readable_output() {
        let plan = AdvancedQueryPlan::validate(
            &[
                QueryOperator::Scan,
                QueryOperator::Filter,
                QueryOperator::Limit,
            ],
            50,
            5_000,
            3,
        )
        .expect("valid plan");
        let explanation = plan.explain();
        assert!(explanation.contains("Scan"));
        assert!(explanation.contains("Filter"));
        assert!(explanation.contains("Limit"));
        assert!(explanation.contains("rows<=50"));
    }

    #[test]
    fn complex_query_with_join_and_traverse_is_valid() {
        let plan = AdvancedQueryPlan::validate(
            &[
                QueryOperator::Scan,
                QueryOperator::Traverse,
                QueryOperator::Join,
                QueryOperator::Aggregate,
                QueryOperator::Sort,
                QueryOperator::Limit,
            ],
            200,
            50_000,
            4,
        )
        .expect("complex query within bounds");
        assert_eq!(plan.operators.len(), 6);
        assert_eq!(plan.depth, 4);
    }
}
