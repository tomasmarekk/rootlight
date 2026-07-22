//! Deterministic parity gate for the canonical MCP capability registry.
//!
//! The gate validates catalog, profile, batch, field, enum/const/boolean value,
//! pagination, generation, and budget metadata against the checked generated
//! schemas. It proves that every public input shape was reviewed; process-level
//! acceptance tests remain responsible for proving executor behavior.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use rootlight_mcp_contract::MCP_SCHEMA_VERSION;
use rootlight_mcp_contract::capability::{
    BudgetSemantics, CAPABILITIES, CapabilityRule, CapabilityStatus, GenerationSemantics,
    PaginationSemantics, ToolCapability, is_batch_eligible,
};
use rootlight_mcp_contract::catalog::{ExposureProfile, McpTool};
use rootlight_mcp_contract::{ErrorCode, VerticalTool};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const REGISTRY_ARTIFACT_SCHEMA: &str = "rootlight.mcp-capability-registry/1";
const PARITY_ARTIFACT_SCHEMA: &str = "rootlight.mcp-capability-parity/1";
const EXECUTION_MATRIX_SCHEMA: &str = "rootlight.mcp-execution-matrix/1";
const INITIAL_MISMATCH_SCHEMA: &str = "rootlight.mcp-capability-mismatches/1";
const SCHEMA_GOLDEN_SCHEMA: &str = "rootlight.mcp-schema-goldens/1";
const MCP_EXECUTOR_SOURCE: &str = include_str!("../../apps/rootlight-mcp/src/executor.rs");

/// Optional source-bound artifact output requested from the parity gate.
pub(crate) struct Options {
    output_dir: Option<PathBuf>,
    source_revision: Option<String>,
}

impl Options {
    /// Parses the artifact output pair, or selects validation-only mode.
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, CapabilityError> {
        let mut output_dir = None;
        let mut source_revision = None;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--output-dir" if output_dir.is_none() => {
                    output_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or(CapabilityError::MissingArgument("--output-dir"))?,
                    ));
                }
                "--source-revision" if source_revision.is_none() => {
                    source_revision = Some(
                        args.next()
                            .ok_or(CapabilityError::MissingArgument("--source-revision"))?,
                    );
                }
                _ => return Err(CapabilityError::UnexpectedArgument(flag)),
            }
        }
        match (&output_dir, &source_revision) {
            (None, None) => {}
            (Some(_), Some(revision)) if valid_source_revision(revision) => {}
            (Some(_), Some(revision)) => {
                return Err(CapabilityError::InvalidSourceRevision(revision.clone()));
            }
            _ => return Err(CapabilityError::IncompleteArtifactOptions),
        }
        Ok(Self {
            output_dir,
            source_revision,
        })
    }
}

pub(crate) fn check(options: &Options) -> Result<(), CapabilityError> {
    let registry = CAPABILITIES.to_vec();
    let mut problems: Vec<Problem> = Vec::new();
    validate_catalog_parity(&registry, &mut problems);
    validate_contract_version(&registry, &mut problems);
    validate_discovery_descriptions(&registry, &mut problems);
    validate_batch_eligibility(&registry, &mut problems);
    validate_profile_membership(&registry, &mut problems);
    validate_handler_disposition(&registry, &registered_handler_names(), &mut problems);
    validate_input_contracts(&registry, &mut problems);
    validate_schema_goldens(&mut problems)?;

    if !problems.is_empty() {
        problems.sort();
        let report = problems
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        return Err(CapabilityError::Problems { report });
    }

    let reviewed_fields = reviewed_field_count().map_err(CapabilityError::Schema)?;
    let blocked = registry
        .iter()
        .flat_map(|entry| entry.rules)
        .filter(|rule| rule.status == CapabilityStatus::Blocked)
        .count();
    if let (Some(output_dir), Some(source_revision)) =
        (&options.output_dir, &options.source_revision)
    {
        write_artifacts(output_dir, source_revision, reviewed_fields, blocked)?;
    }
    println!(
        "capability check passed: {} tools, {reviewed_fields} input fields, {blocked} explicit blocked gaps",
        registry.len()
    );
    Ok(())
}

fn valid_source_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
        let expected = entry.tool.contract_version();
        if entry.contract_version != expected {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::ContractVersion {
                    expected: expected.to_owned(),
                    observed: entry.contract_version.to_owned(),
                },
            ));
        }
    }
}

fn validate_discovery_descriptions(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        if entry.status == CapabilityStatus::Implemented {
            continue;
        }
        let description = entry.tool.description().to_ascii_lowercase();
        if !description.contains(entry.fallback_summary) {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::DiscoveryDescriptionDrift {
                    summary: entry.fallback_summary.to_owned(),
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
        let expected_shared_budget = entry.tool == McpTool::QueryBatch;
        if entry.batch_shared_budget != expected_shared_budget {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::SharedBatchBudgetDrift {
                    expected: expected_shared_budget,
                    observed: entry.batch_shared_budget,
                },
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

fn registered_handler_names() -> BTreeSet<&'static str> {
    VerticalTool::ALL
        .into_iter()
        .map(VerticalTool::name)
        .collect()
}

fn handler_function_exists(path: &str) -> bool {
    let Some(function) = path.rsplit("::").next() else {
        return false;
    };
    let generic_prefix = format!("async fn {function}<");
    let plain_prefix = format!("async fn {function}(");
    MCP_EXECUTOR_SOURCE.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(&generic_prefix) || line.starts_with(&plain_prefix)
    })
}

fn validate_handler_disposition(
    registry: &[ToolCapability],
    handlers: &BTreeSet<&str>,
    problems: &mut Vec<Problem>,
) {
    for handler in handlers {
        if !registry.iter().any(|entry| entry.tool.name() == *handler) {
            problems.push(Problem::new(*handler, ProblemKind::UnregisteredHandler));
        }
    }
    for entry in registry {
        let registered = handlers.contains(entry.tool.name());
        let declared = entry.handler_path.is_some();
        if let Some(path) = entry.handler_path
            && !handler_function_exists(path)
        {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::HandlerPathMissing {
                    path: path.to_owned(),
                },
            ));
        }
        if declared != registered {
            problems.push(Problem::new(
                entry.tool.name(),
                ProblemKind::HandlerAvailabilityDrift {
                    declared,
                    registered,
                },
            ));
        }
        let has_explicit_disposition = matches!(
            entry.status,
            CapabilityStatus::UnsupportedStableError | CapabilityStatus::Blocked
        );
        if !registered && !has_explicit_disposition {
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
        validate_fail_closed_default(entry, problems);
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
        if entry.tool == McpTool::QueryBatch {
            validate_batch_tool_values(entry, &shape, problems);
        }
    }
}

fn validate_fail_closed_default(entry: &ToolCapability, problems: &mut Vec<Problem>) {
    if entry.default_field_status != CapabilityStatus::Blocked {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::NonFailClosedFieldDefault {
                observed: entry.default_field_status.name().to_owned(),
            },
        ));
    }
}

fn validate_batch_tool_values(
    entry: &ToolCapability,
    shape: &SchemaShape,
    problems: &mut Vec<Problem>,
) {
    let path = "operations[].tool";
    let Some(values) = shape.get(path) else {
        return;
    };
    for tool in McpTool::ALL {
        if is_batch_eligible(tool) && !values.closed_values.contains(tool.name()) {
            problems.push(Problem::new(
                tool.name(),
                ProblemKind::BatchToolMissingSchemaValue,
            ));
        }
    }
    for value in &values.closed_values {
        let Some(tool) = McpTool::ALL
            .into_iter()
            .find(|tool| tool.name() == value.as_str())
        else {
            continue;
        };
        if is_batch_eligible(tool) {
            continue;
        }
        let disposition = entry
            .rules
            .iter()
            .find(|rule| rule.path == path && rule.value == Some(value.as_str()));
        if !disposition.is_some_and(|rule| {
            rule.status == CapabilityStatus::UnsupportedStableError && rule.error_code.is_some()
        }) {
            problems.push(Problem::new(
                value,
                ProblemKind::BatchToolMissingExplicitDisposition,
            ));
        }
    }
}

fn validate_shape_hash(entry: &ToolCapability, shape: &SchemaShape, problems: &mut Vec<Problem>) {
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

fn validate_rules(entry: &ToolCapability, shape: &SchemaShape, problems: &mut Vec<Problem>) {
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
            && !values.closed_values.contains(value)
            && !values.accepts_open_string_value(value)
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
            (CapabilityStatus::UnsupportedStableError, Some(_)) => {}
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
    shape: &SchemaShape,
    problems: &mut Vec<Problem>,
) {
    for (path, values) in shape {
        validate_resolved_rule(entry, path, None, problems);
        for value in &values.closed_values {
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
    let is_explicit = entry.rules.contains(&rule);
    if rule.path.is_empty() || !is_explicit {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::ImplicitFieldDisposition {
                path: rule_identity(path, value),
                observed: rule.status.name().to_owned(),
            },
        ));
    }
    if rule.summary.trim().is_empty() {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::EmptyRuleSummary {
                path: rule_identity(path, value),
            },
        ));
    }
    if rule.status == CapabilityStatus::UnsupportedStableError && rule.error_code.is_none() {
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
    shape: &SchemaShape,
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

    let has_cursor = shape.contains_key("cursor")
        || (entry.tool == McpTool::ContextPack && shape.contains_key("continuation"));
    let pagination_has_cursor = entry.pagination == PaginationSemantics::AuthenticatedCursor;
    if has_cursor != pagination_has_cursor {
        problems.push(Problem::new(
            entry.tool.name(),
            ProblemKind::PaginationDrift {
                has_cursor,
                semantics: format!("{:?}", entry.pagination),
            },
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

#[derive(Debug, Default, PartialEq, Eq)]
struct SchemaField {
    closed_values: BTreeSet<String>,
    open_string_schemas: Vec<Value>,
}

impl SchemaField {
    fn accepts_open_string_value(&self, value: &str) -> bool {
        let instance = Value::String(value.to_owned());
        self.open_string_schemas.iter().any(|schema| {
            jsonschema::draft202012::new(schema)
                .is_ok_and(|validator| validator.is_valid(&instance))
        })
    }
}

type SchemaShape = BTreeMap<String, SchemaField>;

fn schema_shape(schema_text: &str) -> Result<SchemaShape, SchemaError> {
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
    fields: &mut SchemaShape,
) -> Result<(), SchemaError> {
    if !path.is_empty() {
        let field = fields.entry(path.to_owned()).or_default();
        collect_value_shape(
            node,
            definitions,
            &mut BTreeSet::new(),
            &mut field.closed_values,
            &mut field.open_string_schemas,
        )?;
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

fn collect_value_shape(
    node: &Value,
    definitions: &serde_json::Map<String, Value>,
    active_refs: &mut BTreeSet<String>,
    values: &mut BTreeSet<String>,
    open_string_schemas: &mut Vec<Value>,
) -> Result<(), SchemaError> {
    if let Some(reference) = node.get("$ref").and_then(Value::as_str) {
        if active_refs.insert(reference.to_owned()) {
            let resolved = resolve_reference(reference, definitions)?;
            collect_value_shape(
                resolved,
                definitions,
                active_refs,
                values,
                open_string_schemas,
            )?;
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
    if schema_allows_string(node) && node.get("enum").is_none() && node.get("const").is_none() {
        open_string_schemas.push(node.clone());
    }
    if schema_allows_boolean(node) {
        values.insert("false".to_owned());
        values.insert("true".to_owned());
    }
    for keyword in ["allOf", "anyOf", "oneOf"] {
        if let Some(branches) = node.get(keyword).and_then(Value::as_array) {
            for branch in branches {
                collect_value_shape(
                    branch,
                    definitions,
                    active_refs,
                    values,
                    open_string_schemas,
                )?;
            }
        }
    }
    Ok(())
}

fn schema_allows_string(node: &Value) -> bool {
    match node.get("type") {
        Some(Value::String(kind)) => kind == "string",
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == "string"),
        _ => false,
    }
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

fn input_shape_hash(shape: &SchemaShape) -> String {
    let canonical: Vec<(&str, Vec<&str>)> = shape
        .iter()
        .map(|(path, field)| {
            (
                path.as_str(),
                field
                    .closed_values
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
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

fn schema_document_hash(schema_text: &str) -> Result<String, SchemaError> {
    let document: Value =
        serde_json::from_str(schema_text).map_err(|source| SchemaError::InvalidJson { source })?;
    let canonical =
        serde_json::to_vec(&document).map_err(|source| SchemaError::CanonicalJson { source })?;
    Ok(sha256_hex(&canonical))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a string cannot fail");
            output
        })
}

fn validate_schema_goldens(problems: &mut Vec<Problem>) -> Result<(), CapabilityError> {
    let goldens: SchemaGoldenFixture = serde_json::from_str(include_str!(
        "../../tests/fixtures/capability/schema-goldens-v1.json"
    ))
    .map_err(|source| CapabilityError::FixtureJson {
        fixture: "schema-goldens-v1.json",
        source,
    })?;
    if goldens.schema != SCHEMA_GOLDEN_SCHEMA {
        return Err(CapabilityError::FixtureContract {
            fixture: "schema-goldens-v1.json",
            detail: "schema identifier differs",
        });
    }

    let catalog_names: BTreeSet<&str> = VerticalTool::ALL
        .into_iter()
        .map(VerticalTool::name)
        .collect();
    for unexpected in goldens
        .tools
        .keys()
        .filter(|name| !catalog_names.contains(name.as_str()))
    {
        problems.push(Problem::new(
            unexpected,
            ProblemKind::UnexpectedSchemaGolden,
        ));
    }

    for tool in VerticalTool::ALL {
        let Some(expected) = goldens.tools.get(tool.name()) else {
            problems.push(Problem::new(tool.name(), ProblemKind::MissingSchemaGolden));
            continue;
        };
        let observed_input =
            schema_document_hash(tool.input_schema_json()).map_err(CapabilityError::Schema)?;
        let observed_output =
            schema_document_hash(tool.output_schema_json()).map_err(CapabilityError::Schema)?;
        if observed_input != expected.input_sha256 {
            problems.push(Problem::new(
                tool.name(),
                ProblemKind::SchemaGoldenDrift {
                    direction: "input",
                    expected: expected.input_sha256.clone(),
                    observed: observed_input,
                },
            ));
        }
        if observed_output != expected.output_sha256 {
            problems.push(Problem::new(
                tool.name(),
                ProblemKind::SchemaGoldenDrift {
                    direction: "output",
                    expected: expected.output_sha256.clone(),
                    observed: observed_output,
                },
            ));
        }
    }
    Ok(())
}

fn write_artifacts(
    output_dir: &Path,
    source_revision: &str,
    reviewed_fields: usize,
    blocked_gaps: usize,
) -> Result<(), CapabilityError> {
    fs::create_dir_all(output_dir).map_err(|source| CapabilityError::Io {
        operation: "create capability artifact directory",
        path: output_dir.to_path_buf(),
        source,
    })?;

    let tools = build_registry_artifact_tools()?;
    let registry = RegistryArtifact {
        schema: REGISTRY_ARTIFACT_SCHEMA,
        source_revision,
        contract_version: MCP_SCHEMA_VERSION,
        tools,
    };
    write_json(&output_dir.join("capability-registry-v1.json"), &registry)?;

    let matrix = ExecutionMatrixArtifact {
        schema: EXECUTION_MATRIX_SCHEMA,
        source_revision,
        observation_state: "not_run",
        verdict: "unknown",
        cases: build_execution_matrix_cases()?,
    };
    write_json(
        &output_dir.join("capability-execution-matrix-v1.json"),
        &matrix,
    )?;

    let parity = ParityArtifact {
        schema: PARITY_ARTIFACT_SCHEMA,
        source_revision,
        status: "passed_with_explicit_blockers",
        tool_count: CAPABILITIES.len(),
        reviewed_input_fields: reviewed_fields,
        schema_golden_pairs: VerticalTool::ALL.len(),
        profile_counts: ProfileCounts {
            scout: ExposureProfile::Scout.tools().len(),
            analysis: ExposureProfile::Analysis.tools().len(),
            developer: ExposureProfile::Developer.tools().len(),
        },
        blocked_gaps,
    };
    write_json(
        &output_dir.join("capability-parity-report-v1.json"),
        &parity,
    )?;

    let mut initial_mismatches: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/capability/initial-mismatches-v1.json"
    ))
    .map_err(|source| CapabilityError::FixtureJson {
        fixture: "initial-mismatches-v1.json",
        source,
    })?;
    validate_initial_mismatches(&initial_mismatches)?;
    initial_mismatches
        .as_object_mut()
        .expect("validated mismatch fixture is an object")
        .insert(
            "generatedForRevision".to_owned(),
            Value::String(source_revision.to_owned()),
        );
    write_json(
        &output_dir.join("capability-initial-mismatches-v1.json"),
        &initial_mismatches,
    )?;
    Ok(())
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), CapabilityError> {
    let mut encoded =
        serde_json::to_vec_pretty(value).map_err(|source| CapabilityError::ArtifactJson {
            path: path.to_path_buf(),
            source,
        })?;
    encoded.push(b'\n');
    fs::write(path, encoded).map_err(|source| CapabilityError::Io {
        operation: "write capability artifact",
        path: path.to_path_buf(),
        source,
    })
}

fn validate_initial_mismatches(value: &Value) -> Result<(), CapabilityError> {
    let object = value.as_object().ok_or(CapabilityError::FixtureContract {
        fixture: "initial-mismatches-v1.json",
        detail: "root must be an object",
    })?;
    if object.get("schema").and_then(Value::as_str) != Some(INITIAL_MISMATCH_SCHEMA) {
        return Err(CapabilityError::FixtureContract {
            fixture: "initial-mismatches-v1.json",
            detail: "schema identifier differs",
        });
    }
    let findings = object.get("findings").and_then(Value::as_array).ok_or(
        CapabilityError::FixtureContract {
            fixture: "initial-mismatches-v1.json",
            detail: "findings must be an array",
        },
    )?;
    let expected_blocked: BTreeSet<String> = CAPABILITIES
        .iter()
        .flat_map(|entry| {
            entry
                .rules
                .iter()
                .filter(|rule| rule.status == CapabilityStatus::Blocked)
                .map(|rule| {
                    format!(
                        "{}::{}",
                        entry.tool.name(),
                        rule_identity(rule.path, rule.value)
                    )
                })
        })
        .collect();
    let mut reported_blocked = BTreeSet::new();
    for finding in findings {
        let tool_name = finding.get("tool").and_then(Value::as_str).ok_or(
            CapabilityError::FixtureContract {
                fixture: "initial-mismatches-v1.json",
                detail: "finding tool is missing",
            },
        )?;
        let capability = finding.get("capability").and_then(Value::as_str).ok_or(
            CapabilityError::FixtureContract {
                fixture: "initial-mismatches-v1.json",
                detail: "finding capability is missing",
            },
        )?;
        let Some(entry) = CAPABILITIES
            .iter()
            .find(|entry| entry.tool.name() == tool_name)
        else {
            return Err(CapabilityError::FixtureContract {
                fixture: "initial-mismatches-v1.json",
                detail: "finding tool is not registered",
            });
        };
        let disposition = entry.disposition(capability, None);
        if disposition.status != CapabilityStatus::Blocked
            || finding
                .get("currentRegistryDisposition")
                .and_then(Value::as_str)
                != Some("blocked")
        {
            return Err(CapabilityError::FixtureContract {
                fixture: "initial-mismatches-v1.json",
                detail: "finding no longer matches an explicit blocked disposition",
            });
        }
        if !reported_blocked.insert(format!(
            "{tool_name}::{}",
            rule_identity(disposition.path, disposition.value)
        )) {
            return Err(CapabilityError::FixtureContract {
                fixture: "initial-mismatches-v1.json",
                detail: "findings contain a duplicate blocked disposition",
            });
        }
    }
    if reported_blocked != expected_blocked {
        return Err(CapabilityError::FixtureContract {
            fixture: "initial-mismatches-v1.json",
            detail: "findings do not cover the complete current blocked set",
        });
    }
    Ok(())
}

fn build_registry_artifact_tools() -> Result<Vec<RegistryArtifactTool>, CapabilityError> {
    CAPABILITIES
        .iter()
        .map(|entry| {
            let vertical = vertical_tool(entry.tool)?;
            Ok(RegistryArtifactTool {
                name: entry.tool.name(),
                status: entry.status.name(),
                input_shape_hash: entry.input_shape_hash,
                input_schema_sha256: schema_document_hash(vertical.input_schema_json())
                    .map_err(CapabilityError::Schema)?,
                output_schema_sha256: schema_document_hash(vertical.output_schema_json())
                    .map_err(CapabilityError::Schema)?,
                profiles: entry
                    .profiles
                    .iter()
                    .map(|profile| profile.name())
                    .collect(),
                batch_eligible: entry.batch_eligible,
                explain_supported: entry.explain_supported,
                handler_path: entry.handler_path,
                pagination: entry.pagination.name(),
                generation: entry.generation.name(),
                budget: entry.budget.name(),
                batch_shared_budget: entry.batch_shared_budget,
                fallback_summary: entry.fallback_summary,
                fields: artifact_field_dispositions(entry, vertical)?,
            })
        })
        .collect()
}

fn build_execution_matrix_cases() -> Result<Vec<ExecutionMatrixCase>, CapabilityError> {
    let mut cases = Vec::new();
    for entry in &CAPABILITIES {
        cases.push(ExecutionMatrixCase {
            id: format!("{}::handler", entry.tool.name()),
            tool: entry.tool.name(),
            capability: "handler".to_owned(),
            value: None,
            expected_disposition: if entry.handler_path.is_some() {
                "handler_available"
            } else {
                entry.status.name()
            },
            expected_error: None,
            observation: "not_run",
            verdict: "unknown",
        });
        let vertical = vertical_tool(entry.tool)?;
        for field in artifact_field_dispositions(entry, vertical)? {
            cases.push(ExecutionMatrixCase {
                id: field.value.as_ref().map_or_else(
                    || format!("{}::{}", entry.tool.name(), field.path),
                    |value| format!("{}::{}={value}", entry.tool.name(), field.path),
                ),
                tool: entry.tool.name(),
                capability: field.path,
                value: field.value,
                expected_disposition: field.status,
                expected_error: field.error_code,
                observation: "not_run",
                verdict: "unknown",
            });
        }
    }
    Ok(cases)
}

fn artifact_field_dispositions(
    entry: &ToolCapability,
    vertical: VerticalTool,
) -> Result<Vec<ArtifactFieldDisposition>, CapabilityError> {
    let shape = schema_shape(vertical.input_schema_json()).map_err(CapabilityError::Schema)?;
    let mut fields = Vec::new();
    for (path, field) in shape {
        fields.push(artifact_field(
            entry.disposition(&path, None),
            path.clone(),
            None,
        ));
        for value in &field.closed_values {
            fields.push(artifact_field(
                entry.disposition(&path, Some(value)),
                path.clone(),
                Some(value.clone()),
            ));
        }
        let open_values: BTreeSet<&str> = entry
            .rules
            .iter()
            .filter(|rule| {
                !field.open_string_schemas.is_empty()
                    && rule.path == path
                    && rule.value.is_some_and(|value| {
                        !field.closed_values.contains(value)
                            && field.accepts_open_string_value(value)
                    })
            })
            .filter_map(|rule| rule.value)
            .collect();
        for value in open_values {
            fields.push(artifact_field(
                entry.disposition(&path, Some(value)),
                path.clone(),
                Some(value.to_owned()),
            ));
        }
    }
    Ok(fields)
}

fn artifact_field(
    rule: CapabilityRule,
    path: String,
    value: Option<String>,
) -> ArtifactFieldDisposition {
    ArtifactFieldDisposition {
        path,
        value,
        status: rule.status.name(),
        error_code: rule.error_code,
        summary: rule.summary,
    }
}

fn vertical_tool(tool: McpTool) -> Result<VerticalTool, CapabilityError> {
    VerticalTool::ALL
        .into_iter()
        .find(|candidate| candidate.name() == tool.name())
        .ok_or(CapabilityError::MissingVerticalTool(tool.name()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SchemaGoldenFixture {
    schema: String,
    tools: BTreeMap<String, SchemaGolden>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SchemaGolden {
    input_sha256: String,
    output_sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryArtifact<'a> {
    schema: &'static str,
    source_revision: &'a str,
    contract_version: &'static str,
    tools: Vec<RegistryArtifactTool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RegistryArtifactTool {
    name: &'static str,
    status: &'static str,
    input_shape_hash: &'static str,
    input_schema_sha256: String,
    output_schema_sha256: String,
    profiles: Vec<&'static str>,
    batch_eligible: bool,
    explain_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    handler_path: Option<&'static str>,
    pagination: &'static str,
    generation: &'static str,
    budget: &'static str,
    batch_shared_budget: bool,
    fallback_summary: &'static str,
    fields: Vec<ArtifactFieldDisposition>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactFieldDisposition {
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_code: Option<ErrorCode>,
    summary: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionMatrixArtifact<'a> {
    schema: &'static str,
    source_revision: &'a str,
    observation_state: &'static str,
    verdict: &'static str,
    cases: Vec<ExecutionMatrixCase>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ExecutionMatrixCase {
    id: String,
    tool: &'static str,
    capability: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    expected_disposition: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    expected_error: Option<ErrorCode>,
    observation: &'static str,
    verdict: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ParityArtifact<'a> {
    schema: &'static str,
    source_revision: &'a str,
    status: &'static str,
    tool_count: usize,
    reviewed_input_fields: usize,
    schema_golden_pairs: usize,
    profile_counts: ProfileCounts,
    blocked_gaps: usize,
}

#[derive(Serialize)]
struct ProfileCounts {
    scout: usize,
    analysis: usize,
    developer: usize,
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
        expected: String,
        observed: String,
    },
    DiscoveryDescriptionDrift {
        summary: String,
    },
    BatchEligibilityDrift,
    BatchNotReadOnly,
    SharedBatchBudgetDrift {
        expected: bool,
        observed: bool,
    },
    ProfileMembership {
        expected: String,
        observed: String,
    },
    UnregisteredHandler,
    HandlerAvailabilityDrift {
        declared: bool,
        registered: bool,
    },
    HandlerPathMissing {
        path: String,
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
    NonFailClosedFieldDefault {
        observed: String,
    },
    ImplicitFieldDisposition {
        path: String,
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
    BatchToolMissingSchemaValue,
    BatchToolMissingExplicitDisposition,
    ExplainDrift {
        expected: bool,
        observed: bool,
    },
    PaginationDrift {
        has_cursor: bool,
        semantics: String,
    },
    GenerationDrift {
        has_generation: bool,
        semantics: String,
    },
    BudgetDrift {
        has_budget: bool,
        has_token_budget: bool,
        semantics: String,
    },
    MissingSchemaGolden,
    UnexpectedSchemaGolden,
    SchemaGoldenDrift {
        direction: &'static str,
        expected: String,
        observed: String,
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
            ProblemKind::ContractVersion { expected, observed } => write!(
                formatter,
                "contract_version {observed} does not match {expected}"
            ),
            ProblemKind::DiscoveryDescriptionDrift { summary } => write!(
                formatter,
                "discovery description does not contain its reviewed fallback summary: {summary}"
            ),
            ProblemKind::BatchEligibilityDrift => {
                write!(formatter, "batch flag drifted from the allowlist")
            }
            ProblemKind::BatchNotReadOnly => {
                write!(formatter, "batch-eligible tool must be read-only")
            }
            ProblemKind::SharedBatchBudgetDrift { expected, observed } => write!(
                formatter,
                "shared child-execution budget is {observed}, expected {expected}"
            ),
            ProblemKind::ProfileMembership { expected, observed } => write!(
                formatter,
                "profile membership is [{observed}], expected [{expected}]"
            ),
            ProblemKind::UnregisteredHandler => {
                write!(
                    formatter,
                    "runtime handler has no capability registry entry"
                )
            }
            ProblemKind::HandlerAvailabilityDrift {
                declared,
                registered,
            } => write!(
                formatter,
                "handler availability is declared {declared}, runtime registry reports {registered}"
            ),
            ProblemKind::HandlerPathMissing { path } => {
                write!(formatter, "declared process handler does not exist: {path}")
            }
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
            ProblemKind::NonFailClosedFieldDefault { observed } => write!(
                formatter,
                "unreviewed input fields default to {observed}, expected blocked"
            ),
            ProblemKind::ImplicitFieldDisposition { path, observed } => write!(
                formatter,
                "schema field or value {path} resolves implicitly to {observed}"
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
            ProblemKind::BatchToolMissingSchemaValue => {
                write!(
                    formatter,
                    "batch-eligible tool is absent from the batch schema"
                )
            }
            ProblemKind::BatchToolMissingExplicitDisposition => write!(
                formatter,
                "schema-valid ineligible batch tool lacks an explicit stable pre-execution error"
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
            ProblemKind::MissingSchemaGolden => {
                write!(formatter, "generated schema golden is missing")
            }
            ProblemKind::UnexpectedSchemaGolden => {
                write!(formatter, "schema golden has no catalog tool")
            }
            ProblemKind::SchemaGoldenDrift {
                direction,
                expected,
                observed,
            } => write!(
                formatter,
                "{direction} schema changed; expected {expected}, observed {observed}"
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
    #[error("{0} requires a value")]
    MissingArgument(&'static str),
    #[error("unexpected capability-check argument: {0}")]
    UnexpectedArgument(String),
    #[error("--output-dir and --source-revision must be supplied together")]
    IncompleteArtifactOptions,
    #[error("source revision must be a lowercase 40- or 64-character hexadecimal object id: {0}")]
    InvalidSourceRevision(String),
    #[error("capability fixture {fixture} is invalid JSON")]
    FixtureJson {
        fixture: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("capability fixture {fixture} violates its contract: {detail}")]
    FixtureContract {
        fixture: &'static str,
        detail: &'static str,
    },
    #[error("MCP catalog tool has no vertical contract: {0}")]
    MissingVerticalTool(&'static str),
    #[error("failed to serialize capability artifact {}", path.display())]
    ArtifactJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{operation} at {}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SchemaError {
    #[error("invalid generated JSON schema")]
    InvalidJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("generated JSON schema cannot be canonicalized")]
    CanonicalJson {
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
        validate_discovery_descriptions(&CAPABILITIES, &mut problems);
        validate_batch_eligibility(&CAPABILITIES, &mut problems);
        validate_profile_membership(&CAPABILITIES, &mut problems);
        validate_handler_disposition(&CAPABILITIES, &registered_handler_names(), &mut problems);
        validate_input_contracts(&CAPABILITIES, &mut problems);
        validate_schema_goldens(&mut problems).expect("schema goldens are readable");
        assert!(problems.is_empty(), "unexpected problems: {problems:#?}");
    }

    #[test]
    fn broader_description_than_registry_summary_is_rejected() {
        let mut capability = entry(McpTool::CodeLocate);
        capability.fallback_summary = "unreviewed semantic repository search";
        let mut problems = Vec::new();
        validate_discovery_descriptions(&[capability], &mut problems);
        assert!(
            problems.iter().any(|problem| matches!(
                problem.kind,
                ProblemKind::DiscoveryDescriptionDrift { .. }
            ))
        );
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
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/capability/batch-allowlist-drift.json"
        ))
        .expect("batch drift fixture is valid");
        let tool = McpTool::ALL
            .into_iter()
            .find(|tool| fixture["tool"] == tool.name())
            .expect("fixture tool is registered");
        let mut drifted = entry(tool);
        drifted.batch_eligible = fixture["batchEligible"]
            .as_bool()
            .expect("fixture batch flag is boolean");
        let mut problems = Vec::new();
        validate_batch_eligibility(&[drifted], &mut problems);
        assert!(problems.iter().any(|problem| matches!(
            problem.kind,
            ProblemKind::BatchEligibilityDrift | ProblemKind::BatchNotReadOnly
        )));
    }

    #[test]
    fn shared_child_budget_drift_is_rejected() {
        let mut entry = entry(McpTool::CodeLocate);
        entry.batch_shared_budget = true;
        let mut problems = Vec::new();
        validate_batch_eligibility(&[entry], &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::SharedBatchBudgetDrift { .. }))
        );
    }

    #[test]
    fn ignored_schema_field_is_rejected_by_the_parity_gate() {
        let baseline = schema_shape(include_str!(
            "../../tests/fixtures/capability/baseline.schema.json"
        ))
        .expect("baseline fixture is valid");
        let added = schema_shape(include_str!(
            "../../tests/fixtures/capability/added-field.schema.json"
        ))
        .expect("field fixture is valid");
        let mut capability = entry(McpTool::OperationStatus);
        capability.input_shape_hash = Box::leak(input_shape_hash(&baseline).into_boxed_str());
        let mut problems = Vec::new();
        validate_shape_hash(&capability, &added, &mut problems);
        assert!(added.contains_key("ignored"));
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::InputShapeHash { .. }))
        );
    }

    #[test]
    fn non_blocked_field_default_is_rejected() {
        let mut capability = entry(McpTool::OperationStatus);
        capability.default_field_status = CapabilityStatus::Implemented;
        let mut problems = Vec::new();

        validate_fail_closed_default(&capability, &mut problems);

        assert!(
            problems.iter().any(|problem| matches!(
                problem.kind,
                ProblemKind::NonFailClosedFieldDefault { .. }
            ))
        );
    }

    #[test]
    fn explicit_ancestors_cover_fields_and_closed_values() {
        let shape = schema_shape(include_str!(
            "../../tests/fixtures/capability/baseline.schema.json"
        ))
        .expect("baseline fixture is valid");
        let mut capability = entry(McpTool::OperationStatus);
        capability.rules = &[
            CapabilityRule {
                path: "enabled",
                value: None,
                status: CapabilityStatus::Implemented,
                error_code: None,
                summary: "reviewed boolean",
            },
            CapabilityRule {
                path: "mode",
                value: None,
                status: CapabilityStatus::FallbackLimited,
                error_code: None,
                summary: "reviewed mode",
            },
        ];
        let mut problems = Vec::new();

        validate_field_dispositions(&capability, &shape, &mut problems);

        assert!(problems.is_empty(), "unexpected problems: {problems:#?}");
    }

    #[test]
    fn added_schema_field_requires_an_explicit_rule() {
        let shape = schema_shape(include_str!(
            "../../tests/fixtures/capability/added-field.schema.json"
        ))
        .expect("field fixture is valid");
        let mut capability = entry(McpTool::OperationStatus);
        capability.rules = &[
            CapabilityRule {
                path: "enabled",
                value: None,
                status: CapabilityStatus::Implemented,
                error_code: None,
                summary: "reviewed boolean",
            },
            CapabilityRule {
                path: "mode",
                value: None,
                status: CapabilityStatus::FallbackLimited,
                error_code: None,
                summary: "reviewed mode",
            },
        ];
        let mut problems = Vec::new();

        validate_field_dispositions(&capability, &shape, &mut problems);

        assert!(problems.iter().any(|problem| matches!(
            &problem.kind,
            ProblemKind::ImplicitFieldDisposition { path, observed }
                if path == "ignored" && observed == "blocked"
        )));
    }

    #[test]
    fn schema_only_enum_value_is_rejected_by_the_parity_gate() {
        let baseline = schema_shape(include_str!(
            "../../tests/fixtures/capability/baseline.schema.json"
        ))
        .expect("baseline fixture is valid");
        let added = schema_shape(include_str!(
            "../../tests/fixtures/capability/added-enum-value.schema.json"
        ))
        .expect("enum fixture is valid");
        let mut capability = entry(McpTool::OperationStatus);
        capability.input_shape_hash = Box::leak(input_shape_hash(&baseline).into_boxed_str());
        let mut problems = Vec::new();
        validate_shape_hash(&capability, &added, &mut problems);
        assert!(added["mode"].closed_values.contains("future"));
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::InputShapeHash { .. }))
        );
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

    #[test]
    fn exact_rule_for_open_string_value_is_accepted_and_emitted() {
        let cycles = entry(McpTool::ArchitectureCycles);
        let vertical =
            vertical_tool(McpTool::ArchitectureCycles).expect("cycles has a generated contract");
        let shape =
            schema_shape(vertical.input_schema_json()).expect("cycles input schema is valid");
        assert!(
            shape["projection.level"].accepts_open_string_value("symbol"),
            "the exact supported projection must remain schema-valid"
        );

        let mut problems = Vec::new();
        validate_rules(&cycles, &shape, &mut problems);
        assert!(
            !problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::UnknownRuleValue { .. })),
            "open-string exact values must be valid registry rules: {problems:#?}"
        );

        let fields = artifact_field_dispositions(&cycles, vertical)
            .expect("cycles dispositions are generated");
        assert!(fields.iter().any(|field| {
            field.path == "projection.level"
                && field.value.as_deref() == Some("symbol")
                && field.status == "implemented"
        }));
        assert!(fields.iter().any(|field| {
            field.path == "projection.level"
                && field.value.is_none()
                && field.status == "unsupported_stable_error"
        }));
    }

    #[test]
    fn schema_invalid_open_string_rule_is_rejected() {
        let vertical =
            vertical_tool(McpTool::ArchitectureCycles).expect("cycles has a generated contract");
        let shape =
            schema_shape(vertical.input_schema_json()).expect("cycles input schema is valid");
        let invalid_level = Box::leak("x".repeat(65).into_boxed_str());
        let mut cycles = entry(McpTool::ArchitectureCycles);
        cycles.rules = Box::leak(
            vec![rootlight_mcp_contract::capability::CapabilityRule {
                path: "projection.level",
                value: Some(invalid_level),
                status: CapabilityStatus::Blocked,
                error_code: None,
                summary: "not schema-valid",
            }]
            .into_boxed_slice(),
        );

        let mut problems = Vec::new();
        validate_rules(&cycles, &shape, &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::UnknownRuleValue { .. }))
        );
    }

    #[test]
    fn schema_valid_ineligible_batch_tool_requires_explicit_disposition() {
        let mut batch = entry(McpTool::QueryBatch);
        let rules = batch
            .rules
            .iter()
            .copied()
            .filter(|rule| !(rule.path == "operations[].tool" && rule.value == Some("plan.change")))
            .collect::<Vec<_>>();
        batch.rules = Box::leak(rules.into_boxed_slice());
        let shape = schema_shape(
            vertical_tool(McpTool::QueryBatch)
                .expect("batch has a generated contract")
                .input_schema_json(),
        )
        .expect("batch input schema is valid");
        let mut problems = Vec::new();
        validate_batch_tool_values(&batch, &shape, &mut problems);
        assert!(problems.iter().any(|problem| matches!(
            problem.kind,
            ProblemKind::BatchToolMissingExplicitDisposition
        )));
    }

    #[test]
    fn hidden_profile_bypass_fixture_is_rejected() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/capability/hidden-profile-bypass.json"
        ))
        .expect("profile bypass fixture is valid");
        let tool = McpTool::ALL
            .into_iter()
            .find(|tool| fixture["tool"] == tool.name())
            .expect("fixture tool is registered");
        let profiles = fixture["declaredProfiles"]
            .as_array()
            .expect("fixture profiles are an array")
            .iter()
            .map(|profile| {
                ExposureProfile::from_name(profile.as_str().expect("profile is a string"))
                    .expect("fixture profile is known")
            })
            .collect::<Vec<_>>();
        let mut drifted = entry(tool);
        drifted.profiles = Box::leak(profiles.into_boxed_slice());
        let mut problems = Vec::new();
        validate_profile_membership(&[drifted], &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::ProfileMembership { .. }))
        );
    }

    #[test]
    fn handler_availability_drift_is_rejected() {
        let mut drifted = entry(McpTool::CodeLocate);
        drifted.handler_path = None;
        let mut problems = Vec::new();
        validate_handler_disposition(&[drifted], &BTreeSet::from(["code.locate"]), &mut problems);
        assert!(
            problems.iter().any(|problem| matches!(
                problem.kind,
                ProblemKind::HandlerAvailabilityDrift { .. }
            ))
        );
    }

    #[test]
    fn missing_declared_handler_function_is_rejected() {
        let mut drifted = entry(McpTool::CodeLocate);
        drifted.handler_path = Some("rootlight-mcp::executor::missing_handler");
        let mut problems = Vec::new();
        validate_handler_disposition(&[drifted], &BTreeSet::from(["code.locate"]), &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::HandlerPathMissing { .. }))
        );
    }

    #[test]
    fn unregistered_runtime_handler_is_rejected() {
        let handlers = BTreeSet::from(["future.tool"]);
        let mut problems = Vec::new();
        validate_handler_disposition(&CAPABILITIES, &handlers, &mut problems);
        assert!(problems.iter().any(|problem| {
            problem.id == "future.tool" && matches!(problem.kind, ProblemKind::UnregisteredHandler)
        }));
    }

    #[test]
    fn missing_handler_without_stable_disposition_is_rejected() {
        let mut drifted = entry(McpTool::CodeLocate);
        drifted.handler_path = None;
        let mut problems = Vec::new();
        validate_handler_disposition(&[drifted], &BTreeSet::new(), &mut problems);
        assert!(
            problems
                .iter()
                .any(|problem| matches!(problem.kind, ProblemKind::MissingHandlerOrDisposition))
        );
    }

    #[test]
    fn input_and_output_schema_goldens_cover_the_complete_catalog() {
        let fixture: SchemaGoldenFixture = serde_json::from_str(include_str!(
            "../../tests/fixtures/capability/schema-goldens-v1.json"
        ))
        .expect("schema golden fixture is valid");
        assert_eq!(fixture.schema, SCHEMA_GOLDEN_SCHEMA);
        assert_eq!(fixture.tools.len(), VerticalTool::ALL.len());
        let mut problems = Vec::new();
        validate_schema_goldens(&mut problems).expect("schema goldens are readable");
        assert!(problems.is_empty(), "unexpected problems: {problems:#?}");
    }

    #[test]
    fn versioned_artifacts_are_deterministic_and_execution_remains_unobserved() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let first = temporary.path().join("first");
        let second = temporary.path().join("second");
        let revision = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let reviewed_fields = reviewed_field_count().expect("generated schemas are valid");
        let blocked_gaps = CAPABILITIES
            .iter()
            .flat_map(|entry| entry.rules)
            .filter(|rule| rule.status == CapabilityStatus::Blocked)
            .count();
        write_artifacts(&first, revision, reviewed_fields, blocked_gaps)
            .expect("first artifacts are generated");
        write_artifacts(&second, revision, reviewed_fields, blocked_gaps)
            .expect("second artifacts are generated");

        for name in [
            "capability-registry-v1.json",
            "capability-execution-matrix-v1.json",
            "capability-parity-report-v1.json",
            "capability-initial-mismatches-v1.json",
        ] {
            let left = fs::read(first.join(name)).expect("first artifact is readable");
            let right = fs::read(second.join(name)).expect("second artifact is readable");
            assert_eq!(left, right, "{name} must serialize deterministically");
        }

        let registry: Value = serde_json::from_slice(
            &fs::read(first.join("capability-registry-v1.json"))
                .expect("registry artifact is readable"),
        )
        .expect("registry artifact is valid JSON");
        assert_eq!(registry["schema"], REGISTRY_ARTIFACT_SCHEMA);
        assert_eq!(
            registry["tools"].as_array().map(Vec::len),
            Some(CAPABILITIES.len())
        );

        let matrix: Value = serde_json::from_slice(
            &fs::read(first.join("capability-execution-matrix-v1.json"))
                .expect("matrix artifact is readable"),
        )
        .expect("matrix artifact is valid JSON");
        let cases = matrix["cases"]
            .as_array()
            .expect("matrix cases are an array");
        assert!(cases.len() > reviewed_fields);
        assert_eq!(
            cases
                .iter()
                .filter_map(|case| case["id"].as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            cases.len(),
            "matrix case identifiers must be unique"
        );
        assert!(
            cases
                .iter()
                .all(|case| { case["observation"] == "not_run" && case["verdict"] == "unknown" })
        );
        assert!(cases.iter().any(|case| {
            case["id"] == "query.batch::operations[].local_budget.max_tokens"
                && case["expectedDisposition"] == "blocked"
        }));
    }
}
