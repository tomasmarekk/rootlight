//! Safe advanced query AST validation and cost estimation.
//!
//! The advanced query interface accepts only a typed, allow-listed AST.
//! SQL, Cypher strings, shell fragments, arbitrary regex, arbitrary code,
//! and unbounded recursion are structurally impossible because the AST
//! grammar does not represent them. Every query is statically cost-bounded
//! before execution.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_mcp_contract::context::{QueryAstNode, QueryPredicate, QueryValue};

/// Maximum AST depth accepted by the validator.
pub const MAX_AST_DEPTH: usize = 5;

/// Maximum rows a single advanced query may return.
pub const MAX_ADVANCED_ROWS: usize = 1_000;

/// Maximum traversal facts a single advanced query may examine.
pub const MAX_ADVANCED_TRAVERSAL: usize = 100_000;

/// Maximum estimated cost units before a query is rejected.
pub const MAX_ESTIMATED_COST: u64 = 1_000_000;

/// Maximum encoded size of the advanced-query parameter map.
pub const MAX_PARAMETER_BYTES: usize = 64 * 1024;

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
    /// A referenced parameter was not supplied.
    #[error("query AST references a missing parameter")]
    MissingParameter,
    /// A supplied parameter is not referenced by the AST.
    #[error("query parameters contain an unexpected value")]
    UnexpectedParameter,
    /// A parameter name or value is not a valid scalar binding.
    #[error("query parameter is invalid")]
    InvalidParameter,
    /// The encoded parameter map exceeds the hard byte ceiling.
    #[error("query parameters exceed the maximum encoded size")]
    ParameterSizeExceeded,
}

/// Binds typed value parameters into a cloned AST before daemon execution.
///
/// Parameter references can occur only where [`QueryValue`] is accepted by the
/// grammar, so bindings cannot introduce operators, field names, or executable
/// query text.
///
/// # Errors
///
/// Returns [`AdvancedQueryError`] for invalid names, missing or extra values,
/// nested references, or an encoded parameter map above
/// [`MAX_PARAMETER_BYTES`].
pub fn bind_query_parameters(
    query: &QueryAstNode,
    parameters: Option<&BTreeMap<String, QueryValue>>,
) -> Result<QueryAstNode, AdvancedQueryError> {
    let parameters = parameters.cloned().unwrap_or_default();
    let encoded_size = serde_json::to_vec(&parameters)
        .map_err(|_| AdvancedQueryError::InvalidParameter)?
        .len();
    if encoded_size > MAX_PARAMETER_BYTES {
        return Err(AdvancedQueryError::ParameterSizeExceeded);
    }
    if parameters.iter().any(|(name, value)| {
        !valid_parameter_name(name) || matches!(value, QueryValue::Parameter { .. })
    }) {
        return Err(AdvancedQueryError::InvalidParameter);
    }

    let mut bound = query.clone();
    let mut referenced = BTreeSet::new();
    bind_node(&mut bound, &parameters, &mut referenced)?;
    if parameters.keys().any(|name| !referenced.contains(name)) {
        return Err(AdvancedQueryError::UnexpectedParameter);
    }
    Ok(bound)
}

fn valid_parameter_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 {
        return false;
    }
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn bind_node(
    node: &mut QueryAstNode,
    parameters: &BTreeMap<String, QueryValue>,
    referenced: &mut BTreeSet<String>,
) -> Result<(), AdvancedQueryError> {
    match node {
        QueryAstNode::Scan { filter, .. } => {
            if let Some(predicate) = filter {
                bind_predicate(predicate, parameters, referenced)?;
            }
        }
        QueryAstNode::Filter { input, predicate } => {
            bind_node(input, parameters, referenced)?;
            bind_predicate(predicate, parameters, referenced)?;
        }
        QueryAstNode::Project { input, .. }
        | QueryAstNode::Aggregate { input, .. }
        | QueryAstNode::Sort { input, .. }
        | QueryAstNode::Limit { input, .. } => bind_node(input, parameters, referenced)?,
        QueryAstNode::Join { left, right, .. } => {
            bind_node(left, parameters, referenced)?;
            bind_node(right, parameters, referenced)?;
        }
        QueryAstNode::Traverse { .. } => {}
    }
    Ok(())
}

fn bind_predicate(
    predicate: &mut QueryPredicate,
    parameters: &BTreeMap<String, QueryValue>,
    referenced: &mut BTreeSet<String>,
) -> Result<(), AdvancedQueryError> {
    match predicate {
        QueryPredicate::Equals { field, value } | QueryPredicate::NotEquals { field, value } => {
            bind_value(field, value, parameters, referenced)
        }
        QueryPredicate::In { field, values } => {
            for value in values {
                bind_value(field, value, parameters, referenced)?;
            }
            Ok(())
        }
        QueryPredicate::And { predicates } | QueryPredicate::Or { predicates } => {
            for predicate in predicates {
                bind_predicate(predicate, parameters, referenced)?;
            }
            Ok(())
        }
    }
}

fn bind_value(
    field: &str,
    value: &mut QueryValue,
    parameters: &BTreeMap<String, QueryValue>,
    referenced: &mut BTreeSet<String>,
) -> Result<(), AdvancedQueryError> {
    let QueryValue::Parameter { name } = value else {
        return Ok(());
    };
    if !valid_parameter_name(name) {
        return Err(AdvancedQueryError::InvalidParameter);
    }
    let bound = parameters
        .get(name)
        .ok_or(AdvancedQueryError::MissingParameter)?
        .clone();
    if !parameter_type_matches(field, &bound) {
        return Err(AdvancedQueryError::TypeMismatch);
    }
    referenced.insert(name.clone());
    *value = bound;
    Ok(())
}

fn parameter_type_matches(field: &str, value: &QueryValue) -> bool {
    match field {
        "id" | "source" | "target" => matches!(value, QueryValue::Symbol(_)),
        "kind" | "name" | "path" | "relation" => matches!(value, QueryValue::Text(_)),
        _ => true,
    }
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

    /// Stable static display name used in plan explanations.
    ///
    /// Returning a borrowed static str avoids allocating and permanently
    /// leaking one formatted string per operator on every `explain` call,
    /// which would otherwise grow without bound in a long-running server.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Scan => "Scan",
            Self::Filter => "Filter",
            Self::Project => "Project",
            Self::Join => "Join",
            Self::Aggregate => "Aggregate",
            Self::Traverse => "Traverse",
            Self::Sort => "Sort",
            Self::Limit => "Limit",
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
    /// Derives and validates a plan directly from the public typed AST.
    ///
    /// # Errors
    ///
    /// Returns [`AdvancedQueryError`] when AST depth, resource ceilings, or an
    /// optional caller cost ceiling reject the derived plan.
    pub fn from_ast(
        query: &QueryAstNode,
        max_rows: usize,
        max_traversal: usize,
        cost_limit: Option<u64>,
    ) -> Result<Self, AdvancedQueryError> {
        let mut operators = Vec::new();
        let depth = derive_query_operators(query, &mut operators);
        let plan = Self::validate(&operators, max_rows, max_traversal, depth)?;
        if cost_limit.is_some_and(|limit| plan.estimated_cost > limit) {
            return Err(AdvancedQueryError::CostExceeded);
        }
        Ok(plan)
    }

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
        let ops: Vec<&str> = self.operators.iter().map(|op| op.as_str()).collect();
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

fn derive_query_operators(node: &QueryAstNode, operators: &mut Vec<QueryOperator>) -> usize {
    match node {
        QueryAstNode::Scan { .. } => {
            operators.push(QueryOperator::Scan);
            1
        }
        QueryAstNode::Filter { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Filter);
            depth.saturating_add(1)
        }
        QueryAstNode::Project { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Project);
            depth.saturating_add(1)
        }
        QueryAstNode::Join { left, right, .. } => {
            let left_depth = derive_query_operators(left, operators);
            let right_depth = derive_query_operators(right, operators);
            operators.push(QueryOperator::Join);
            left_depth.max(right_depth).saturating_add(1)
        }
        QueryAstNode::Aggregate { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Aggregate);
            depth.saturating_add(1)
        }
        QueryAstNode::Traverse { .. } => {
            operators.push(QueryOperator::Traverse);
            1
        }
        QueryAstNode::Sort { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Sort);
            depth.saturating_add(1)
        }
        QueryAstNode::Limit { input, .. } => {
            let depth = derive_query_operators(input, operators);
            operators.push(QueryOperator::Limit);
            depth.saturating_add(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        AdvancedQueryError, AdvancedQueryPlan, MAX_ADVANCED_ROWS, MAX_ADVANCED_TRAVERSAL,
        MAX_AST_DEPTH, MAX_ESTIMATED_COST, QueryOperator, bind_query_parameters,
    };
    use rootlight_mcp_contract::{
        context::{QueryAstNode, QueryPredicate, QueryValue},
        vertical::EntityKind,
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
    fn typed_ast_derivation_is_owned_by_the_agent_boundary() {
        let query = QueryAstNode::Limit {
            input: Box::new(QueryAstNode::Scan {
                entity: EntityKind::Function,
                filter: None,
            }),
            max_rows: 20,
        };

        let plan = AdvancedQueryPlan::from_ast(&query, 20, MAX_ADVANCED_TRAVERSAL, Some(1_000))
            .expect("bounded typed AST is valid");

        assert_eq!(
            plan.operators,
            vec![QueryOperator::Scan, QueryOperator::Limit]
        );
        assert_eq!(plan.depth, 2);
    }

    #[test]
    fn typed_ast_cost_ceiling_is_enforced_without_the_mcp_binary() {
        let query = QueryAstNode::Scan {
            entity: EntityKind::Function,
            filter: None,
        };

        assert_eq!(
            AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, Some(1)),
            Err(AdvancedQueryError::CostExceeded)
        );
    }

    fn parameter_query(name: &str) -> QueryAstNode {
        QueryAstNode::Scan {
            entity: EntityKind::Function,
            filter: Some(Box::new(QueryPredicate::Equals {
                field: "name".to_owned(),
                value: QueryValue::Parameter {
                    name: name.to_owned(),
                },
            })),
        }
    }

    #[test]
    fn typed_parameter_is_bound_only_in_a_value_position() {
        let parameters = BTreeMap::from([(
            "needle".to_owned(),
            QueryValue::Text("handle_request".to_owned()),
        )]);

        let bound = bind_query_parameters(&parameter_query("needle"), Some(&parameters))
            .expect("referenced typed parameter is valid");
        let encoded = serde_json::to_string(&bound).expect("bound AST serializes");

        assert!(encoded.contains("handle_request"));
        assert!(!encoded.contains("parameter"));
    }

    #[test]
    fn missing_and_extra_parameters_are_rejected() {
        assert_eq!(
            bind_query_parameters(&parameter_query("missing"), None),
            Err(AdvancedQueryError::MissingParameter)
        );
        let extra = BTreeMap::from([("unused".to_owned(), QueryValue::Boolean(true))]);
        assert_eq!(
            bind_query_parameters(
                &QueryAstNode::Scan {
                    entity: EntityKind::Function,
                    filter: None,
                },
                Some(&extra)
            ),
            Err(AdvancedQueryError::UnexpectedParameter)
        );
    }

    #[test]
    fn invalid_names_nested_references_and_type_mismatches_are_rejected() {
        let invalid_name =
            BTreeMap::from([("x;drop".to_owned(), QueryValue::Text("ignored".to_owned()))]);
        assert_eq!(
            bind_query_parameters(&parameter_query("x;drop"), Some(&invalid_name)),
            Err(AdvancedQueryError::InvalidParameter)
        );

        let nested = BTreeMap::from([(
            "needle".to_owned(),
            QueryValue::Parameter {
                name: "other".to_owned(),
            },
        )]);
        assert_eq!(
            bind_query_parameters(&parameter_query("needle"), Some(&nested)),
            Err(AdvancedQueryError::InvalidParameter)
        );

        let wrong_type = BTreeMap::from([("needle".to_owned(), QueryValue::Integer(42))]);
        assert_eq!(
            bind_query_parameters(&parameter_query("needle"), Some(&wrong_type)),
            Err(AdvancedQueryError::TypeMismatch)
        );
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
