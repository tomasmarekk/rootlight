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

/// Maximum number of operators in one accepted AST.
pub const MAX_AST_NODES: usize = 31;

/// Maximum rows a single advanced query may return.
pub const MAX_ADVANCED_ROWS: usize = 1_000;

/// Maximum traversal facts a single advanced query may examine.
pub const MAX_ADVANCED_TRAVERSAL: usize = 100_000;

/// Maximum estimated cost units before a query is rejected.
pub const MAX_ESTIMATED_COST: u64 = 1_000_000;

/// Maximum encoded size of the advanced-query parameter map.
pub const MAX_PARAMETER_BYTES: usize = 64 * 1024;

const MAX_PREDICATE_DEPTH: usize = 5;
const MAX_PREDICATE_NODES: usize = 256;
const MAX_BOOLEAN_PREDICATES: usize = 16;
const MAX_IN_VALUES: usize = 256;
const MAX_PROJECT_COLUMNS: usize = 64;
const MAX_GROUP_COLUMNS: usize = 16;
const MAX_AGGREGATIONS: usize = 16;
const MAX_SORT_KEYS: usize = 8;
const MAX_FIELD_BYTES: usize = 256;
const MAX_TEXT_BYTES: usize = 4_096;
const MAX_TRAVERSE_DEPTH: u8 = 5;

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
        validate_ast_structure(query)?;
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
        if max_rows == 0 || max_rows > MAX_ADVANCED_ROWS {
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

fn validate_ast_structure(query: &QueryAstNode) -> Result<(), AdvancedQueryError> {
    let mut ast_nodes = 0;
    validate_ast_node(query, 1, &mut ast_nodes)
}

fn validate_ast_node(
    node: &QueryAstNode,
    depth: usize,
    ast_nodes: &mut usize,
) -> Result<(), AdvancedQueryError> {
    if depth > MAX_AST_DEPTH {
        return Err(AdvancedQueryError::DepthExceeded);
    }
    *ast_nodes = ast_nodes.saturating_add(1);
    if *ast_nodes > MAX_AST_NODES {
        return Err(AdvancedQueryError::Malformed);
    }

    match node {
        QueryAstNode::Scan { filter, .. } => {
            if let Some(predicate) = filter {
                validate_predicate_structure(predicate)?;
            }
        }
        QueryAstNode::Filter { input, predicate } => {
            validate_ast_node(input, depth.saturating_add(1), ast_nodes)?;
            validate_predicate_structure(predicate)?;
        }
        QueryAstNode::Project { input, columns } => {
            validate_ast_node(input, depth.saturating_add(1), ast_nodes)?;
            validate_fields(columns, 1, MAX_PROJECT_COLUMNS)?;
        }
        QueryAstNode::Join { left, right, on } => {
            validate_ast_node(left, depth.saturating_add(1), ast_nodes)?;
            validate_ast_node(right, depth.saturating_add(1), ast_nodes)?;
            validate_field(on)?;
        }
        QueryAstNode::Aggregate {
            input,
            group_by,
            aggregations,
        } => {
            validate_ast_node(input, depth.saturating_add(1), ast_nodes)?;
            validate_fields(group_by, 0, MAX_GROUP_COLUMNS)?;
            if aggregations.is_empty() || aggregations.len() > MAX_AGGREGATIONS {
                return Err(AdvancedQueryError::Malformed);
            }
            for aggregation in aggregations {
                match aggregation {
                    rootlight_mcp_contract::context::AggregateFunction::Count => {}
                    rootlight_mcp_contract::context::AggregateFunction::Sum { field }
                    | rootlight_mcp_contract::context::AggregateFunction::Min { field }
                    | rootlight_mcp_contract::context::AggregateFunction::Max { field } => {
                        validate_field(field)?;
                    }
                }
            }
        }
        QueryAstNode::Traverse { max_depth, .. } => {
            if max_depth.is_some_and(|depth| depth == 0 || depth > MAX_TRAVERSE_DEPTH) {
                return Err(AdvancedQueryError::TraversalLimitExceeded);
            }
        }
        QueryAstNode::Sort { input, by } => {
            validate_ast_node(input, depth.saturating_add(1), ast_nodes)?;
            if by.is_empty() || by.len() > MAX_SORT_KEYS {
                return Err(AdvancedQueryError::Malformed);
            }
            for key in by {
                validate_field(&key.field)?;
            }
        }
        QueryAstNode::Limit { input, max_rows } => {
            validate_ast_node(input, depth.saturating_add(1), ast_nodes)?;
            if *max_rows == 0 || usize::from(*max_rows) > MAX_ADVANCED_ROWS {
                return Err(AdvancedQueryError::RowLimitExceeded);
            }
        }
    }
    Ok(())
}

fn validate_predicate_structure(predicate: &QueryPredicate) -> Result<(), AdvancedQueryError> {
    let mut predicate_nodes = 0;
    validate_predicate_node(predicate, 1, &mut predicate_nodes)
}

fn validate_predicate_node(
    predicate: &QueryPredicate,
    depth: usize,
    predicate_nodes: &mut usize,
) -> Result<(), AdvancedQueryError> {
    if depth > MAX_PREDICATE_DEPTH {
        return Err(AdvancedQueryError::DepthExceeded);
    }
    *predicate_nodes = predicate_nodes.saturating_add(1);
    if *predicate_nodes > MAX_PREDICATE_NODES {
        return Err(AdvancedQueryError::Malformed);
    }

    match predicate {
        QueryPredicate::Equals { field, value } | QueryPredicate::NotEquals { field, value } => {
            validate_field(field)?;
            validate_query_value(value)
        }
        QueryPredicate::In { field, values } => {
            validate_field(field)?;
            if values.is_empty() || values.len() > MAX_IN_VALUES {
                return Err(AdvancedQueryError::Malformed);
            }
            for value in values {
                validate_query_value(value)?;
            }
            Ok(())
        }
        QueryPredicate::And { predicates } | QueryPredicate::Or { predicates } => {
            if predicates.is_empty() || predicates.len() > MAX_BOOLEAN_PREDICATES {
                return Err(AdvancedQueryError::Malformed);
            }
            for predicate in predicates {
                validate_predicate_node(predicate, depth.saturating_add(1), predicate_nodes)?;
            }
            Ok(())
        }
    }
}

fn validate_query_value(value: &QueryValue) -> Result<(), AdvancedQueryError> {
    match value {
        QueryValue::Text(value) if value.is_empty() || value.len() > MAX_TEXT_BYTES => {
            Err(AdvancedQueryError::Malformed)
        }
        QueryValue::Parameter { name } if !valid_parameter_name(name) => {
            Err(AdvancedQueryError::InvalidParameter)
        }
        _ => Ok(()),
    }
}

fn validate_fields(
    fields: &[String],
    minimum: usize,
    maximum: usize,
) -> Result<(), AdvancedQueryError> {
    if fields.len() < minimum || fields.len() > maximum {
        return Err(AdvancedQueryError::Malformed);
    }
    for field in fields {
        validate_field(field)?;
    }
    Ok(())
}

fn validate_field(field: &str) -> Result<(), AdvancedQueryError> {
    if field.is_empty() || field.len() > MAX_FIELD_BYTES {
        return Err(AdvancedQueryError::Malformed);
    }
    let mut bytes = field.bytes();
    if !bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        || !bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    {
        return Err(AdvancedQueryError::Malformed);
    }
    Ok(())
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
        MAX_AST_DEPTH, MAX_AST_NODES, MAX_ESTIMATED_COST, MAX_PREDICATE_NODES, QueryOperator,
        bind_query_parameters,
    };
    use proptest::{
        prelude::*,
        test_runner::{RngAlgorithm, RngSeed},
    };
    use rootlight_ids::SymbolId;
    use rootlight_mcp_contract::{
        context::{
            AggregateFunction, QueryAstNode, QueryPredicate, QueryValue, RelationKind, SortKey,
            TraverseDirection,
        },
        vertical::EntityKind,
    };

    const GENERATED_CASES: u32 = 96;
    const GENERATED_SEED: u64 = 202_607_220_040;

    fn entity_kind() -> impl Strategy<Value = EntityKind> {
        prop_oneof![
            Just(EntityKind::File),
            Just(EntityKind::Module),
            Just(EntityKind::Type),
            Just(EntityKind::Function),
            Just(EntityKind::Method),
            Just(EntityKind::Field),
            Just(EntityKind::Constant),
            Just(EntityKind::Variable),
            Just(EntityKind::Configuration),
        ]
    }

    fn field_name() -> impl Strategy<Value = String> {
        prop::sample::select(vec!["id", "kind", "name", "path"]).prop_map(str::to_owned)
    }

    fn scalar_value() -> impl Strategy<Value = QueryValue> {
        prop_oneof![
            "[a-zA-Z0-9_]{1,24}".prop_map(QueryValue::Text),
            any::<i64>().prop_map(QueryValue::Integer),
            any::<bool>().prop_map(QueryValue::Boolean),
        ]
    }

    fn simple_predicate() -> impl Strategy<Value = QueryPredicate> {
        (field_name(), scalar_value())
            .prop_map(|(field, value)| QueryPredicate::Equals { field, value })
    }

    fn valid_ast() -> impl Strategy<Value = QueryAstNode> {
        let leaf = prop_oneof![
            entity_kind().prop_map(|entity| QueryAstNode::Scan {
                entity,
                filter: None,
            }),
            any::<[u8; 20]>().prop_map(|bytes| QueryAstNode::Traverse {
                seed: SymbolId::from_bytes(bytes),
                relation: RelationKind::Calls,
                direction: TraverseDirection::Outbound,
                max_depth: Some(1),
            }),
        ];

        leaf.prop_recursive(
            4,
            u32::try_from(MAX_AST_NODES).expect("AST node limit fits u32"),
            2,
            |inner| {
                prop_oneof![
                    (inner.clone(), simple_predicate()).prop_map(|(input, predicate)| {
                        QueryAstNode::Filter {
                            input: Box::new(input),
                            predicate,
                        }
                    }),
                    (inner.clone(), field_name()).prop_map(|(input, column)| {
                        QueryAstNode::Project {
                            input: Box::new(input),
                            columns: vec![column],
                        }
                    }),
                    inner.clone().prop_map(|input| QueryAstNode::Aggregate {
                        input: Box::new(input),
                        group_by: vec!["kind".to_owned()],
                        aggregations: vec![AggregateFunction::Count],
                    }),
                    (inner.clone(), field_name(), any::<bool>()).prop_map(
                        |(input, field, descending)| QueryAstNode::Sort {
                            input: Box::new(input),
                            by: vec![SortKey { field, descending }],
                        },
                    ),
                    (inner.clone(), 1_u16..=1_000).prop_map(|(input, max_rows)| {
                        QueryAstNode::Limit {
                            input: Box::new(input),
                            max_rows,
                        }
                    }),
                    (inner.clone(), inner, field_name()).prop_map(|(left, right, on)| {
                        QueryAstNode::Join {
                            left: Box::new(left),
                            right: Box::new(right),
                            on,
                        }
                    }),
                ]
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: GENERATED_CASES,
            max_shrink_iters: 512,
            failure_persistence: None,
            rng_algorithm: RngAlgorithm::ChaCha,
            rng_seed: RngSeed::Fixed(GENERATED_SEED),
            ..ProptestConfig::default()
        })]

        #[test]
        fn generated_valid_asts_round_trip_to_the_same_plan(query in valid_ast()) {
            let encoded = serde_json::to_vec(&query).expect("generated AST serializes");
            let decoded: QueryAstNode =
                serde_json::from_slice(&encoded).expect("generated AST deserializes");
            let first = AdvancedQueryPlan::from_ast(
                &query,
                100,
                MAX_ADVANCED_TRAVERSAL,
                None,
            ).expect("generated AST is structurally valid");
            let replay = AdvancedQueryPlan::from_ast(
                &decoded,
                100,
                MAX_ADVANCED_TRAVERSAL,
                None,
            ).expect("round-tripped AST is structurally valid");

            prop_assert_eq!(decoded, query);
            prop_assert_eq!(replay, first);
            prop_assert!(encoded.len() <= 32 * 1024);
        }

        #[test]
        fn bounded_deserializer_corpus_never_escapes_the_typed_ast(
            bytes in prop::collection::vec(any::<u8>(), 0..=512),
        ) {
            if let Ok(query) = serde_json::from_slice::<QueryAstNode>(&bytes) {
                let outcome = AdvancedQueryPlan::from_ast(
                    &query,
                    100,
                    MAX_ADVANCED_TRAVERSAL,
                    None,
                );
                prop_assert!(outcome.is_ok() || matches!(
                    outcome,
                    Err(
                        AdvancedQueryError::DepthExceeded
                            | AdvancedQueryError::Malformed
                            | AdvancedQueryError::RowLimitExceeded
                            | AdvancedQueryError::TraversalLimitExceeded
                            | AdvancedQueryError::InvalidParameter
                            | AdvancedQueryError::CostExceeded
                    )
                ));
            }
        }
    }

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
    fn parameter_payloads_cannot_inject_fields_or_operators() {
        for payload in [
            "name; DROP TABLE symbols",
            "MATCH (n) RETURN n",
            "$(shutdown -h now)",
            ".*",
            "{\"op\":\"join\",\"on\":\"id\"}",
        ] {
            let parameters =
                BTreeMap::from([("needle".to_owned(), QueryValue::Text(payload.to_owned()))]);
            let bound = bind_query_parameters(&parameter_query("needle"), Some(&parameters))
                .expect("executable-looking text remains a typed value");
            let plan = AdvancedQueryPlan::from_ast(&bound, 100, MAX_ADVANCED_TRAVERSAL, None)
                .expect("typed text is not interpreted as query structure");

            assert_eq!(plan.operators, vec![QueryOperator::Scan]);
            assert!(matches!(
                bound,
                QueryAstNode::Scan {
                    filter: Some(predicate),
                    ..
                } if matches!(
                    &*predicate,
                    QueryPredicate::Equals {
                        value: QueryValue::Text(value),
                        ..
                    } if value == payload
                )
            ));
        }
    }

    #[test]
    fn executable_language_and_unknown_grammar_corpus_is_rejected() {
        let corpus = [
            r#""SELECT * FROM symbols""#,
            r#"{"op":"sql","query":"SELECT * FROM symbols"}"#,
            r#"{"op":"cypher","query":"MATCH (n) RETURN n"}"#,
            r#"{"op":"shell","command":"rm -rf /"}"#,
            r#"{"op":"eval","code":"loop {}"}"#,
            r#"{"op":"scan","entity":"function","filter":{"pred":"regex","field":"name","pattern":".*.*"}}"#,
            r#"{"op":"scan","entity":"function","filter":{"pred":"execute","field":"name","value":{"text":"x"}}}"#,
            r#"{"op":"scan","entity":"function","filter":{"pred":"equals","field":{"parameter":"field"},"value":{"text":"x"}}}"#,
        ];

        for input in corpus {
            assert!(
                serde_json::from_str::<QueryAstNode>(input).is_err(),
                "closed AST accepted hostile grammar: {input}"
            );
        }
    }

    #[test]
    fn structural_collection_limits_are_enforced_at_the_boundary() {
        let scan = || QueryAstNode::Scan {
            entity: EntityKind::Function,
            filter: None,
        };
        let field = "a".to_owned();

        for columns in [vec![field.clone()], vec![field.clone(); 64]] {
            let query = QueryAstNode::Project {
                input: Box::new(scan()),
                columns,
            };
            assert!(AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None).is_ok());
        }
        for columns in [Vec::new(), vec![field.clone(); 65]] {
            let query = QueryAstNode::Project {
                input: Box::new(scan()),
                columns,
            };
            assert_eq!(
                AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None),
                Err(AdvancedQueryError::Malformed)
            );
        }

        for key_count in [1, 8] {
            let query = QueryAstNode::Sort {
                input: Box::new(scan()),
                by: vec![
                    SortKey {
                        field: field.clone(),
                        descending: false,
                    };
                    key_count
                ],
            };
            assert!(AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None).is_ok());
        }
        for key_count in [0, 9] {
            let query = QueryAstNode::Sort {
                input: Box::new(scan()),
                by: vec![
                    SortKey {
                        field: field.clone(),
                        descending: false,
                    };
                    key_count
                ],
            };
            assert_eq!(
                AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None),
                Err(AdvancedQueryError::Malformed)
            );
        }

        for aggregation_count in [1, 16] {
            let query = QueryAstNode::Aggregate {
                input: Box::new(scan()),
                group_by: vec![field.clone(); 16],
                aggregations: vec![AggregateFunction::Count; aggregation_count],
            };
            assert!(AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None).is_ok());
        }
        for aggregation_count in [0, 17] {
            let query = QueryAstNode::Aggregate {
                input: Box::new(scan()),
                group_by: Vec::new(),
                aggregations: vec![AggregateFunction::Count; aggregation_count],
            };
            assert_eq!(
                AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None),
                Err(AdvancedQueryError::Malformed)
            );
        }
        let excessive_groups = QueryAstNode::Aggregate {
            input: Box::new(scan()),
            group_by: vec![field; 17],
            aggregations: vec![AggregateFunction::Count],
        };
        assert_eq!(
            AdvancedQueryPlan::from_ast(&excessive_groups, 100, MAX_ADVANCED_TRAVERSAL, None),
            Err(AdvancedQueryError::Malformed)
        );
    }

    #[test]
    fn predicate_limits_are_enforced_at_below_and_above_boundaries() {
        fn equals() -> QueryPredicate {
            QueryPredicate::Equals {
                field: "name".to_owned(),
                value: QueryValue::Text("x".to_owned()),
            }
        }
        fn scan_with(predicate: QueryPredicate) -> QueryAstNode {
            QueryAstNode::Scan {
                entity: EntityKind::Function,
                filter: Some(Box::new(predicate)),
            }
        }

        for value_count in [1, 256] {
            let query = scan_with(QueryPredicate::In {
                field: "name".to_owned(),
                values: vec![QueryValue::Text("x".to_owned()); value_count],
            });
            assert!(AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None).is_ok());
        }
        for value_count in [0, 257] {
            let query = scan_with(QueryPredicate::In {
                field: "name".to_owned(),
                values: vec![QueryValue::Text("x".to_owned()); value_count],
            });
            assert_eq!(
                AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None),
                Err(AdvancedQueryError::Malformed)
            );
        }

        let exact_node_limit = QueryPredicate::And {
            predicates: (0..15)
                .map(|_| QueryPredicate::And {
                    predicates: vec![equals(); 16],
                })
                .collect(),
        };
        assert_eq!(1 + 15 * 17, MAX_PREDICATE_NODES);
        assert!(
            AdvancedQueryPlan::from_ast(
                &scan_with(exact_node_limit),
                100,
                MAX_ADVANCED_TRAVERSAL,
                None
            )
            .is_ok()
        );

        let excessive_nodes = QueryPredicate::And {
            predicates: (0..15)
                .map(|_| QueryPredicate::And {
                    predicates: vec![equals(); 16],
                })
                .chain(std::iter::once(equals()))
                .collect(),
        };
        assert_eq!(
            AdvancedQueryPlan::from_ast(
                &scan_with(excessive_nodes),
                100,
                MAX_ADVANCED_TRAVERSAL,
                None
            ),
            Err(AdvancedQueryError::Malformed)
        );
    }

    #[test]
    fn embedded_row_and_traversal_limits_are_runtime_validated() {
        let scan = || QueryAstNode::Scan {
            entity: EntityKind::Function,
            filter: None,
        };
        for max_rows in [1, u16::try_from(MAX_ADVANCED_ROWS).expect("limit fits u16")] {
            let query = QueryAstNode::Limit {
                input: Box::new(scan()),
                max_rows,
            };
            assert!(
                AdvancedQueryPlan::from_ast(
                    &query,
                    MAX_ADVANCED_ROWS,
                    MAX_ADVANCED_TRAVERSAL,
                    None
                )
                .is_ok()
            );
        }
        for max_rows in [
            0,
            u16::try_from(MAX_ADVANCED_ROWS + 1).expect("limit fits u16"),
        ] {
            let query = QueryAstNode::Limit {
                input: Box::new(scan()),
                max_rows,
            };
            assert_eq!(
                AdvancedQueryPlan::from_ast(
                    &query,
                    MAX_ADVANCED_ROWS,
                    MAX_ADVANCED_TRAVERSAL,
                    None
                ),
                Err(AdvancedQueryError::RowLimitExceeded)
            );
        }

        for max_depth in [Some(1), Some(5), None] {
            let query = QueryAstNode::Traverse {
                seed: SymbolId::from_bytes([0; 20]),
                relation: RelationKind::Calls,
                direction: TraverseDirection::Outbound,
                max_depth,
            };
            assert!(AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None).is_ok());
        }
        for max_depth in [Some(0), Some(6)] {
            let query = QueryAstNode::Traverse {
                seed: SymbolId::from_bytes([0; 20]),
                relation: RelationKind::Calls,
                direction: TraverseDirection::Outbound,
                max_depth,
            };
            assert_eq!(
                AdvancedQueryPlan::from_ast(&query, 100, MAX_ADVANCED_TRAVERSAL, None),
                Err(AdvancedQueryError::TraversalLimitExceeded)
            );
        }
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
