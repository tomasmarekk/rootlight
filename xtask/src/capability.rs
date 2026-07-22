//! Deterministic parity gate for the canonical MCP capability registry.
//!
//! The gate validates catalog, profile, batch, field, enum/const/boolean value,
//! pagination, generation, and budget metadata against the checked generated
//! schemas. It proves that every public input shape was reviewed; process-level
//! acceptance tests remain responsible for proving executor behavior.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use rootlight_mcp_contract::MCP_SCHEMA_VERSION;
use rootlight_mcp_contract::capability::{
    BudgetSemantics, CAPABILITIES, CapabilityStatus, GenerationSemantics, PaginationSemantics,
    ToolCapability, is_batch_eligible,
};
use rootlight_mcp_contract::catalog::{ExposureProfile, McpTool};
use rootlight_mcp_contract::{ErrorCode, VerticalTool};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub(crate) fn check() -> Result<(), CapabilityError> {
    let registry = CAPABILITIES.to_vec();
    let mut problems: Vec<Problem> = Vec::new();
    validate_catalog_parity(&registry, &mut problems);
    validate_contract_version(&registry, &mut problems);
    validate_batch_eligibility(&registry, &mut problems);
    validate_profile_membership(&registry, &mut problems);
    validate_handler_disposition(&registry, &mut problems);
    validate_input_contracts(&registry, &mut problems);

    if problems.is_empty() {
        let reviewed_fields = reviewed_field_count().map_err(CapabilityError::Schema)?;
        let blocked = registry
            .iter()
            .flat_map(|entry| entry.rules)
            .filter(|rule| rule.status == CapabilityStatus::Blocked)
            .count();
        println!(
            "capability check passed: {} tools, {reviewed_fields} input fields, {blocked} explicit blocked gaps",
            registry.len()
        );
        return Ok(());
    }
    problems.sort();
    let report = problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    Err(CapabilityError::Problems { report })
}

fn validate_catalog_parity(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    if registry.len() != McpTool::ALL.len() {
        problems.push(Problem::new(
            "<registry>",
            ProblemKind::CountMismatch {
                expected: McpTool::ALL.len(),
                observed: registry.len(),
            },
        ));
    }
    let mut seen = BTreeSet::new();
    for (position, entry) in registry.iter().enumerate() {
        if !seen.insert(entry.tool.name()) {
            problems.push(Problem::new(entry.tool.name(), ProblemKind::DuplicateTool));
        }
        let Some(expected) = McpTool::ALL.get(position) else {
            continue;
        };
        if entry.tool != *expected {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::OrderMismatch {
                    position,
                    expected: expected.name().to_owned(),
                },
            ));
        }
    }
}

fn validate_contract_version(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        if entry.contract_version != MCP_SCHEMA_VERSION {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::ContractVersion {
                    version: entry.contract_version.to_owned(),
                },
            ));
        }
    }
}

fn validate_batch_eligibility(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        if entry.batch_eligible != is_batch_eligible(entry.tool) {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::BatchEligibilityDrift,
            ));
        }
        if entry.batch_eligible && !entry.tool.read_only() {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::BatchNotReadOnly,
            ));
        }
        if entry.batch_shared_budget {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::UnprovenSharedBatchBudget,
            ));
        }
    }
}

fn validate_profile_membership(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        let expected: Vec<ExposureProfile> = ExposureProfile::ALL
            .into_iter()
            .filter(|profile| profile.exposes(entry.tool))
            .collect();
        if entry.profiles != expected {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::ProfileMembership {
                    expected: expected
                        .iter()
                        .map(|profile| profile.name())
                        .collect::<Vec<_>>()
                        .join(","),
                    observed: entry
                        .profiles
                        .iter()
                        .map(|profile| profile.name())
                        .collect::<Vec<_>>()
                        .join(","),
                },
            ));
        }
    }
}

fn validate_handler_disposition(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        let has_explicit_disposition = matches!(
            entry.status,
            CapabilityStatus::UnsupportedStableError | CapabilityStatus::Blocked
        );
        if !entry.handler_available && !has_explicit_disposition {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::MissingHandlerOrDisposition,
            ));
        }
        if entry.fallback_summary.trim().is_empty() {
            problems.push(Problem::new(entry.tool.name(), ProblemKind::EmptySummary));
        }
    }
}

fn validate_input_contracts(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        let Some(vertical) = VerticalTool::ALL
            .into_iter()
            .find(|tool| tool.name() == entry.tool.name())
        else {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::MissingGeneratedSchema,
            ));
            continue;
        };
        let shape = match schema_shape(vertical.input_schema_json()) {
            Ok(shape) => shape,
            Err(error) => {
                problems.push(Problem::new(
                    entry.tool.name(),
                    ProblemKind::InvalidGeneratedSchema {
                        message: error.to_string(),
                    },
                ));
                continue;
            }
        };
        validate_shape_hash(entry, &shape, problems);
        validate_rules(entry, &shape, problems);
        validate_field_dispositions(entry, &shape, problems);
        validate_cross_cutting_metadata(entry, &shape, problems);
    }
}

fn validate_shape_hash(
    entry: &ToolCapability,
    shape: &BTreeMap<String, BTreeSet<String>>,
    problems: &mut Vec<Problem>,
) {
    let observed = input_shape_hash(shape);
    if entry.input_shape_hash != observed {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::InputShapeHash {
                expected: entry.input_shape_hash.to_owned(),
                observed,
            },
        ));
    }
}

fn validate_rules(
    entry: &ToolCapability,
    shape: &BTreeMap<String, BTreeSet<String>>,
    problems: &mut Vec<Problem>,
) {
    let mut seen = BTreeSet::new();
    for rule in entry.rules {
        let identity = (rule.path, rule.value);
        if !seen.insert(identity) {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::DuplicateRule {
                    path: rule_identity(rule.path, rule.value),
                },
            ));
        }
        let Some(values) = shape.get(rule.path) else {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::UnknownRuleField {
                    path: rule.path.to_owned(),
                },
            ));
            continue;
        };
        if let Some(value) = rule.value
            && !values.contains(value)
        {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::UnknownRuleValue {
                    path: rule.path.to_owned(),
                    value: value.to_owned(),
                },
            ));
        }
        if rule.summary.trim().is_empty() {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::EmptyRuleSummary {
                    path: rule_identity(rule.path, rule.value),
                },
            ));
        }
        match (rule.status, rule.error_code) {
            (CapabilityStatus::UnsupportedStableError, Some(ErrorCode::UnsupportedCapability)) => {}
            (CapabilityStatus::UnsupportedStableError, observed) => {
                problems.push(Problem::new(
                    entry.tool.name(),
                    ProblemKind::UnsupportedWithoutStableError {
                        path: rule_identity(rule.path, rule.value),
                        observed: format!("{observed:?}"),
                    },
                ));
            }
            (_, Some(error)) => {
                problems.push(Problem::new(
                    entry.tool.name(),
                    ProblemKind::UnexpectedStableError {
                        path: rule_identity(rule.path, rule.value),
                        observed: format!("{error:?}"),
                    },
                ));
            }
            (_, None) => {}
        }
    }
}

fn validate_field_dispositions(
    entry: &ToolCapability,
    shape: &BTreeMap<String, BTreeSet<String>>,
    problems: &mut Vec<Problem>,
) {
    for (path, values) in shape {
        validate_resolved_rule(entry, path, None, problems);
        for value in values {
            validate_resolved_rule(entry, path, Some(value), problems);
        }
    }
}

fn validate_resolved_rule(
    entry: &ToolCapability,
    path: &str,
    value: Option<&str>,
    problems: &mut Vec<Problem>,
) {
    let rule = entry.disposition(path, value);
    if rule.summary.trim().is_empty() {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::EmptyRuleSummary {
                path: rule_identity(path, value),
            },
        ));
    }
    if rule.status == CapabilityStatus::UnsupportedStableError
        && rule.error_code != Some(ErrorCode::UnsupportedCapability)
    {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::UnsupportedWithoutStableError {
                path: rule_identity(path, value),
                observed: format!("{:?}", rule.error_code),
            },
        ));
    }
}

fn validate_cross_cutting_metadata(
    entry: &ToolCapability,
    shape: &BTreeMap<String, BTreeSet<String>>,
    problems: &mut Vec<Problem>,
) {
    let has_explain = shape.contains_key("explain");
    if entry.explain_supported != has_explain {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::ExplainDrift {
                expected: has_explain,
                observed: entry.explain_supported,
            },
        ));
    }

    let has_cursor = shape.contains_key("cursor");
    let pagination_has_cursor = !matches!(entry.pagination, PaginationSemantics::None);
    if has_cursor != pagination_has_cursor {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::PaginationDrift {
                has_cursor,
                semantics: format!("{:?}", entry.pagination),
            },
        ));
    }
    if entry.pagination == PaginationSemantics::UnsupportedCursor
        && entry.disposition("cursor", None).status != CapabilityStatus::UnsupportedStableError
    {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::UnsupportedCursorWithoutRule,
        ));
    }

    let has_generation = shape.contains_key("generation");
    let generation_shape_matches = match entry.generation {
        GenerationSemantics::None | GenerationSemantics::CreatesGeneration => !has_generation,
        GenerationSemantics::ComparesGenerations => {
            !has_generation && shape.contains_key("base") && shape.contains_key("head")
        }
        GenerationSemantics::SelectsGeneration
        | GenerationSemantics::ActiveGenerationFallback
        | GenerationSemantics::BatchInherited => has_generation,
    };
    if !generation_shape_matches {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::GenerationDrift {
                has_generation,
                semantics: format!("{:?}", entry.generation),
            },
        ));
    }

    let has_budget = shape.contains_key("budget");
    let has_token_budget = shape.contains_key("token_budget");
    let expected_budget = match entry.budget {
        BudgetSemantics::None => !has_budget && !has_token_budget,
        BudgetSemantics::TokenBudget => has_token_budget,
        BudgetSemantics::PerRequest => {
            has_budget
                || shape.contains_key("cost_limit")
                || shape.contains_key("max_results")
                || shape.contains_key("max_depth")
        }
        BudgetSemantics::Unsupported => {
            has_budget
                && entry.disposition("budget", None).status
                    == CapabilityStatus::UnsupportedStableError
        }
    };
    if !expected_budget {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::BudgetDrift {
                has_budget,
                has_token_budget,
                semantics: format!("{:?}", entry.budget),
            },
        ));
    }
}

fn reviewed_field_count() -> Result<usize, SchemaError> {
    VerticalTool::ALL
        .into_iter()
        .map(|tool| schema_shape(tool.input_schema_json()).map(|shape| shape.len()))
        .sum()
}

fn schema_shape(schema_text: &str) -> Result<BTreeMap<String, BTreeSet<String>>, SchemaError> {
    let schema: Value =
        serde_json::from_str(schema_text).map_err(|source| SchemaError::InvalidJson { source })?;
    let definitions = schema
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or(SchemaError::MissingDefinitions)?;
    let mut fields = BTreeMap::new();
    let mut active_refs = BTreeSet::new();
    visit_schema(&schema, "", definitions, &mut active_refs, &mut fields)?;
    Ok(fields)
}

fn visit_schema(
    node: &Value,
    path: &str,
    definitions: &serde_json::Map<String, Value>,
    active_refs: &mut BTreeSet<String>,
    fields: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<(), SchemaError> {
    if !path.is_empty() {
        let values = fields.entry(path.to_owned()).or_default();
        collect_closed_values(node, definitions, &mut BTreeSet::new(), values)?;
    }
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if active_refs.insert(reference.to_owned()) {
            let resolved = resolve_reference(reference, definitions)?;
            visit_schema(resolved, path, definitions, active_refs, fields)?;
            active_refs.remove(reference);
        }
        return Ok(());
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = node.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                visit_schema(branch, path, definitions, active_refs, fields)?;
            }
        }
    }
    if let Some(properties) = node.get("properties").and_then(Value::as_object) {
        for (name, property) in properties {
            let child_path = if path.is_empty() {
                name.clone()
            } else {
                format!("{path}.{name}")
            };
            visit_schema(property, &child_path, definitions, active_refs, fields)?;
        }
    }
    if let Some(items) = node.get("items") {
        visit_schema(
            items,
            &format!("{path}[]"),
            definitions,
            active_refs,
            fields,
        )?;
    }
    Ok(())
}

fn collect_closed_values(
    node: &Value,
    definitions: &serde_json::Map<String, Value>,
    active_refs: &mut BTreeSet<String>,
    values: &mut BTreeSet<String>,
) -> Result<(), SchemaError> {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if active_refs.insert(reference.to_owned()) {
            let resolved = resolve_reference(reference, definitions)?;
            collect_closed_values(resolved, definitions, active_refs, values)?;
            active_refs.remove(reference);
        }
        return Ok(());
    }
    if let Some(closed) = node.get("enum").and_then(Value::as_array) {
        for value in closed {
            values.insert(canonical_value(value)?);
        }
    }
    if let Some(constant) = node.get("const") {
        values.insert(canonical_value(constant)?);
    }
    if schema_allows_boolean(node) {
        values.insert("false".to_owned());
        values.insert("true".to_owned());
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = node.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                collect_closed_values(branch, definitions, active_refs, values)?;
            }
        }
    }
    Ok(())
}

fn schema_allows_boolean(node: &Value) -> bool {
    match node.get("type") {
        Some(Value::String(kind)) => kind == "boolean",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "boolean"),
        _ => false,
    }
}

fn canonical_value(value: &Value) -> Result<String, SchemaError> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Null => Ok("null".to_owned()),
        _ => Err(SchemaError::NonScalarClosedValue),
    }
}

fn resolve_reference<'a>(
    reference: &str,
    definitions: &'a serde_json::Map<String, Value>,
) -> Result<&'a Value, SchemaError> {
    let name = reference
        .strip_prefix("#/$defs/")
        .ok_or_else(|| SchemaError::ExternalReference {
            reference: reference.to_owned(),
        })?
        .replace("~1", "/")
        .replace("~0", "~");
    definitions
        .get(&name)
        .ok_or(SchemaError::MissingDefinition { name })
}

fn input_shape_hash(shape: &BTreeMap<String, BTreeSet<String>>) -> String {
    let canonical: Vec<(&str, Vec<&str>)> = shape
        .iter()
        .map(|(path, values)| {
            (
                path.as_str(),
                values.iter().map(String::as_str).collect::<Vec<_>>(),
            )
        })
        .collect();
    let encoded = serde_json::to_vec(&canonical).expect("schema inventory contains only strings");
    Sha256::digest(encoded)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        })
}

fn rule_identity(path: &str, value: Option<&str>) -> String {
    value.map_or_else(|| path.to_owned(), |value| format!("{path}={value}"))
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Problem {
    id: String,
    kind: ProblemKind,
}

impl Problem {
    fn new(id: impl Into<String>, kind: ProblemKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProblemKind {
    CountMismatch {
        expected: usize,
        observed: usize,
    },
    DuplicateTool,
    OrderMismatch {
        position: usize,
        expected: String,
    },
    ContractVersion {
        version: String,
    },
    BatchEligibilityDrift,
    BatchNotReadOnly,
    UnprovenSharedBatchBudget,
    ProfileMembership {
        expected: String,
        observed: String,
    },
    MissingHandlerOrDisposition,
    EmptySummary,
    MissingGeneratedSchema,
    InvalidGeneratedSchema {
        message: String,
    },
    InputShapeHash {
        expected: String,
        observed: String,
    },
    DuplicateRule {
        path: String,
    },
    UnknownRuleField {
        path: String,
    },
    UnknownRuleValue {
        path: String,
        value: String,
    },
    EmptyRuleSummary {
        path: String,
    },
    UnsupportedWithoutStableError {
        path: String,
        observed: String,
    },
    UnexpectedStableError {
        path: String,
        observed: String,
    },
    ExplainDrift {
        expected: bool,
        observed: bool,
    },
    PaginationDrift {
        has_cursor: bool,
        semantics: String,
    },
    UnsupportedCursorWithoutRule,
    GenerationDrift {
        has_generation: bool,
        semantics: String,
    },
    BudgetDrift {
        has_budget: bool,
        has_token_budget: bool,
        semantics: String,
    },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: ", self.id)?;
        match &self.kind {
            ProblemKind::CountMismatch { expected, observed } => {
                write!(
                    formatter,
                    "registry has {observed} entries, catalog has {expected}"
                )
            }
            ProblemKind::DuplicateTool => write!(formatter, "duplicate tool in registry"),
            ProblemKind::OrderMismatch { position, expected } => {
                write!(
                    formatter,
                    "registry position {position} should be {expected}"
                )
            }
            ProblemKind::ContractVersion { version } => write!(
                formatter,
                "contract_version {version} does not match {MCP_SCHEMA_VERSION}"
            ),
            ProblemKind::BatchEligibilityDrift => {
                write!(formatter, "batch flag drifted from the allowlist")
            }
            ProblemKind::BatchNotReadOnly => {
                write!(formatter, "batch-eligible tool must be read-only")
            }
            ProblemKind::UnprovenSharedBatchBudget => {
                write!(formatter, "shared batch budget is not mechanically proven")
            }
            ProblemKind::ProfileMembership { expected, observed } => write!(
                formatter,
                "profile membership is [{observed}], expected [{expected}]"
            ),
            ProblemKind::MissingHandlerOrDisposition => {
                write!(formatter, "no handler and no explicit disposition")
            }
            ProblemKind::EmptySummary => write!(formatter, "fallback summary is empty"),
            ProblemKind::MissingGeneratedSchema => {
                write!(formatter, "generated input schema is missing")
            }
            ProblemKind::InvalidGeneratedSchema { message } => {
                write!(formatter, "generated input schema is invalid: {message}")
            }
            ProblemKind::InputShapeHash { expected, observed } => write!(
                formatter,
                "input field/value shape changed; expected {expected}, observed {observed}"
            ),
            ProblemKind::DuplicateRule { path } => {
                write!(formatter, "duplicate capability rule for {path}")
            }
            ProblemKind::UnknownRuleField { path } => {
                write!(formatter, "capability rule references unknown field {path}")
            }
            ProblemKind::UnknownRuleValue { path, value } => {
                write!(
                    formatter,
                    "capability rule references unknown value {path}={value}"
                )
            }
            ProblemKind::EmptyRuleSummary { path } => {
                write!(formatter, "capability rule for {path} has an empty summary")
            }
            ProblemKind::UnsupportedWithoutStableError { path, observed } => write!(
                formatter,
                "unsupported capability {path} has error metadata {observed}"
            ),
            ProblemKind::UnexpectedStableError { path, observed } => write!(
                formatter,
                "non-error capability {path} unexpectedly declares {observed}"
            ),
            ProblemKind::ExplainDrift { expected, observed } => write!(
                formatter,
                "explain_supported is {observed}, schema presence requires {expected}"
            ),
            ProblemKind::PaginationDrift {
                has_cursor,
                semantics,
            } => write!(
                formatter,
                "pagination semantics {semantics} disagree with cursor presence {has_cursor}"
            ),
            ProblemKind::UnsupportedCursorWithoutRule => write!(
                formatter,
                "unsupported cursor lacks an UNSUPPORTED_CAPABILITY field rule"
            ),
            ProblemKind::GenerationDrift {
                has_generation,
                semantics,
            } => write!(
                formatter,
                "generation semantics {semantics} disagree with selector presence {has_generation}"
            ),
            ProblemKind::BudgetDrift {
                has_budget,
                has_token_budget,
                semantics,
            } => write!(
                formatter,
                "budget semantics {semantics} disagree with budget={has_budget}, token_budget={has_token_budget}"
            ),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CapabilityError {
    #[error("capability parity check failed:\n{report}")]
    Problems { report: String },
    #[error("capability schema inventory failed: {0}")]
    Schema(#[from] SchemaError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SchemaError {
    #[error("invalid generated JSON schema")]
    InvalidJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("generated schema has no $defs object")]
    MissingDefinitions,
    #[error("external schema reference is unsupported: {reference}")]
    ExternalReference { reference: String },
    #[error("schema definition is missing: {name}")]
    MissingDefinition { name: String },
    #[error("enum or const contains a non-scalar value")]
    NonScalarClosedValue,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tool: McpTool) -> ToolCapability {
        CAPABILITIES
            .iter()
            .find(|entry| entry.tool == tool)
            .copied()
            .expect("tool has a capability entry")
    }

    #[test]
    fn live_registry_passes_every_validation() {
        let mut problems = Vec::new();
        validate_catalog_parity(&CAPABILITIES, &mut problems);
        validate_contract_version(&CAPABILITIES, &mut problems);
        validate_batch_eligibility(&CAPABILITIES, &mut problems);
        validate_profile_membership(&CAPABILITIES, &mut problems);
        validate_handler_disposition(&CAPABILITIES, &mut problems);
        validate_input_contracts(&CAPABILITIES, &mut problems);
        assert!(problems.is_empty(), "unexpected problems: {problems:#?}");
    }

    #[test]
    fn stale_contract_version_is_rejected() {
        let mut stale = entry(McpTool::CodeLocate);
        stale.contract_version = "0.9";
        let mut problems = Vec::new();
        validate_contract_version(&[stale], &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::ContractVersion { .. }))
        );
    }

    #[test]
    fn drifted_batch_flag_is_rejected() {
        let mut drifted = entry(McpTool::RepoIndex);
        drifted.batch_eligible = true;
        let mut problems = Vec::new();
        validate_batch_eligibility(&[drifted], &mut problems);
        assert!(problems.iter().any(|problem| matches!(
            problem.kind,
            ProblemKind::BatchEligibilityDrift | ProblemKind::BatchNotReadOnly
        )));
    }

    #[test]
    fn unproven_shared_batch_budget_is_rejected() {
        let mut entry = entry(McpTool::QueryBatch);
        entry.batch_shared_budget = true;
        let mut problems = Vec::new();
        validate_batch_eligibility(&[entry], &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::UnprovenSharedBatchBudget))
        );
    }

    #[test]
    fn schema_field_addition_changes_the_reviewed_shape() {
        let baseline = schema_shape(include_str!(
            "../../tests/fixtures/capability/baseline.schema.json"
        ))
        .expect("baseline fixture is valid");
        let added = schema_shape(include_str!(
            "../../tests/fixtures/capability/added-field.schema.json"
        ))
        .expect("field fixture is valid");
        assert_ne!(input_shape_hash(&baseline), input_shape_hash(&added));
        assert!(added.contains_key("ignored"));
    }

    #[test]
    fn schema_enum_addition_changes_the_reviewed_shape() {
        let baseline = schema_shape(include_str!(
            "../../tests/fixtures/capability/baseline.schema.json"
        ))
        .expect("baseline fixture is valid");
        let added = schema_shape(include_str!(
            "../../tests/fixtures/capability/added-enum-value.schema.json"
        ))
        .expect("enum fixture is valid");
        assert_ne!(input_shape_hash(&baseline), input_shape_hash(&added));
        assert!(added["mode"].contains("future"));
    }

    #[test]
    fn rule_for_unknown_field_or_value_is_rejected() {
        let shape = schema_shape(include_str!(
            "../../tests/fixtures/capability/baseline.schema.json"
        ))
        .expect("baseline fixture is valid");
        let mut capability = entry(McpTool::OperationStatus);
        capability.rules = &[rootlight_mcp_contract::capability::CapabilityRule {
            path: "mode",
            value: Some("future"),
            status: CapabilityStatus::Blocked,
            error_code: None,
            summary: "not reviewed",
        }];
        let mut problems = Vec::new();
        validate_rules(&capability, &shape, &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::UnknownRuleValue { .. }))
        );
    }
}
