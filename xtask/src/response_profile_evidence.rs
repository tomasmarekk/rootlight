//! Offline response-profile matrix, golden serialization, and token evidence.
//!
//! The capability registry is the only source of advertised combinations.
//! Existing source-free compatibility examples provide canonical typed output,
//! while the agent shaper produces every retained public representation.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use rootlight_agent::response_profile::shape_batch_child_data;
use rootlight_mcp_contract::{
    accounting::estimate_tokens,
    capability::{CAPABILITIES, ResponseProfileSupport},
    catalog::McpTool,
    context::BatchTool,
    vertical::ResponseProfile,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::token_accounting::{O200kTokenizer, OfflineTokenizer};

const MATRIX_SCHEMA: &str = "rootlight.response-profile-matrix/1";
const GOLDEN_SCHEMA: &str = "rootlight.response-profile-goldens/1";
const REPORT_SCHEMA: &str = "rootlight.response-profile-accounting/1";
const SOURCE_SCHEMA: &str = "rootlight.mcp-compatibility-current/1";
const CANONICAL_SCHEMA: &str = "rootlight.response-profile-canonical-cases/1";
const CANONICAL_FILE: &str = "canonical-cases-v1.json";
const MATRIX_FILE: &str = "matrix-v1.json";
const GOLDEN_FILE: &str = "goldens-v1.json";
const REPORT_FILE: &str = "token-report-v1.json";
const SOURCE_EXAMPLES: &str = "tests/fixtures/mcp/compatibility/current/success-examples.json";
const MAX_FIXTURE_BYTES: u64 = 16 * 1024 * 1024;
const PROFILE_ORDER: [ResponseProfile; 3] = [
    ResponseProfile::Compact,
    ResponseProfile::Standard,
    ResponseProfile::Evidence,
];

/// Command-line options for the offline response-profile gate.
pub(crate) struct Options {
    fixture_root: Option<PathBuf>,
    refresh: bool,
}

impl Options {
    /// Parses an optional fixture root and explicit refresh mode.
    pub(crate) fn parse(
        args: &mut impl Iterator<Item = String>,
    ) -> Result<Self, ResponseProfileEvidenceError> {
        let mut fixture_root = None;
        let mut refresh = false;
        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--fixture-root" if fixture_root.is_none() => {
                    fixture_root = Some(PathBuf::from(args.next().ok_or(
                        ResponseProfileEvidenceError::MissingArgument("--fixture-root"),
                    )?));
                }
                "--refresh" if !refresh => refresh = true,
                _ => {
                    return Err(ResponseProfileEvidenceError::UnexpectedArgument(argument));
                }
            }
        }
        Ok(Self {
            fixture_root,
            refresh,
        })
    }
}

/// Refreshes explicitly requested fixtures and verifies the retained bundle.
pub(crate) fn check(options: &Options) -> Result<(), ResponseProfileEvidenceError> {
    let fixture_root = options
        .fixture_root
        .clone()
        .unwrap_or_else(default_fixture_root);
    let expected = build_bundle()?;
    if options.refresh {
        fs::create_dir_all(&fixture_root).map_err(|source| ResponseProfileEvidenceError::Io {
            path: fixture_root.clone(),
            source,
        })?;
        write_json(&fixture_root.join(CANONICAL_FILE), &expected.canonical)?;
        write_json(&fixture_root.join(MATRIX_FILE), &expected.matrix)?;
        write_json(&fixture_root.join(GOLDEN_FILE), &expected.goldens)?;
        write_json(&fixture_root.join(REPORT_FILE), &expected.report)?;
    }

    verify_file(&fixture_root.join(CANONICAL_FILE), &expected.canonical)?;
    verify_file(&fixture_root.join(MATRIX_FILE), &expected.matrix)?;
    verify_file(&fixture_root.join(GOLDEN_FILE), &expected.goldens)?;
    verify_file(&fixture_root.join(REPORT_FILE), &expected.report)?;
    println!(
        "response-profile evidence verified: {} tools, {} advertised combinations, {} serialized bytes, {} o200k tokens",
        expected.matrix.tools.len(),
        expected.goldens.cases.len(),
        expected.report.total_serialized_bytes,
        expected.report.total_o200k_tokens,
    );
    Ok(())
}

fn default_fixture_root() -> PathBuf {
    PathBuf::from("tests/fixtures/mcp/response-profiles")
}

fn build_bundle() -> Result<EvidenceBundle, ResponseProfileEvidenceError> {
    let source_path = PathBuf::from(SOURCE_EXAMPLES);
    let source_bytes = read_bounded(&source_path)?;
    let source: CompatibilityExamples =
        serde_json::from_slice(&source_bytes).map_err(|source| {
            ResponseProfileEvidenceError::Json {
                path: source_path.clone(),
                source,
            }
        })?;
    if source.schema != SOURCE_SCHEMA {
        return Err(ResponseProfileEvidenceError::Contract(format!(
            "unsupported source-example schema: {}",
            source.schema
        )));
    }
    let mut examples = BTreeMap::new();
    for example in source.tools {
        if examples
            .insert(example.tool.clone(), example.output)
            .is_some()
        {
            return Err(ResponseProfileEvidenceError::Contract(format!(
                "duplicate source example for {}",
                example.tool
            )));
        }
    }

    if CAPABILITIES.len() != McpTool::ALL.len() || examples.len() != McpTool::ALL.len() {
        return Err(ResponseProfileEvidenceError::Contract(
            "the response-profile source must contain the canonical 19-tool catalog".to_owned(),
        ));
    }
    let source_ref = examples
        .get(McpTool::SourceRead.name())
        .and_then(|output| output.pointer("/data/chunks/0/source_ref"))
        .cloned()
        .ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(
                "source.read example has no canonical source reference".to_owned(),
            )
        })?;
    let mut canonical_outputs = BTreeMap::new();
    for tool in McpTool::ALL {
        let output = examples.get(tool.name()).ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(format!(
                "missing source example for {}",
                tool.name()
            ))
        })?;
        canonical_outputs.insert(
            tool.name().to_owned(),
            representative_output(tool, output, &source_ref)?,
        );
    }

    let tokenizer = O200kTokenizer::new()?;
    let tokenizer_identity = serde_json::to_value(tokenizer.identity()).map_err(|source| {
        ResponseProfileEvidenceError::JsonValue {
            context: "tokenizer identity",
            source,
        }
    })?;
    let mut matrix_tools = Vec::with_capacity(McpTool::ALL.len());
    let mut cases = Vec::new();
    let mut measurements = Vec::new();

    for (index, tool) in McpTool::ALL.into_iter().enumerate() {
        let capability = &CAPABILITIES[index];
        if capability.tool != tool {
            return Err(ResponseProfileEvidenceError::Contract(
                "capability order differs from the canonical tool catalog".to_owned(),
            ));
        }
        let support = owned_support(capability.response_profiles);
        let profiles = advertised_profiles(capability.response_profiles);
        let canonical = canonical_outputs.remove(tool.name()).ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(format!(
                "missing source example for {}",
                tool.name()
            ))
        })?;
        let mut normalized = None;
        let mut invariant_projection = None;
        let mut previous_measurement = None;

        for profile in profiles.iter().copied() {
            let mut output = shape_output(tool, &canonical, profile)?;
            solve_usage_counters(&mut output)?;
            let semantic = normalize_output(tool, &output)?;
            match &normalized {
                Some(expected) if expected != &semantic => {
                    return Err(ResponseProfileEvidenceError::Contract(format!(
                        "{} changes semantic output under {:?}",
                        tool.name(),
                        profile
                    )));
                }
                None => normalized = Some(semantic),
                Some(_) => {}
            }
            let invariants = invariant_projection_for(&output);
            match &invariant_projection {
                Some(expected) if expected != &invariants => {
                    return Err(ResponseProfileEvidenceError::Contract(format!(
                        "{} changes envelope invariants under {:?}",
                        tool.name(),
                        profile
                    )));
                }
                None => invariant_projection = Some(invariants),
                Some(_) => {}
            }

            let serialized = serde_json::to_vec(&output).map_err(|source| {
                ResponseProfileEvidenceError::JsonValue {
                    context: "profile output",
                    source,
                }
            })?;
            let serialized_bytes = u64::try_from(serialized.len()).map_err(|_| {
                ResponseProfileEvidenceError::Contract(
                    "serialized output length does not fit u64".to_owned(),
                )
            })?;
            let text = std::str::from_utf8(&serialized).map_err(|_| {
                ResponseProfileEvidenceError::Contract(
                    "JSON serialization was not valid UTF-8".to_owned(),
                )
            })?;
            let o200k_tokens = tokenizer.count(text)?;
            if let Some((previous_profile, previous_bytes, previous_tokens)) = previous_measurement
            {
                let strict = profiles.len() == PROFILE_ORDER.len();
                let byte_ordered = if strict {
                    serialized_bytes > previous_bytes
                } else {
                    serialized_bytes >= previous_bytes
                };
                let token_ordered = if strict {
                    o200k_tokens > previous_tokens
                } else {
                    o200k_tokens >= previous_tokens
                };
                if !byte_ordered || !token_ordered {
                    return Err(ResponseProfileEvidenceError::Contract(format!(
                        "{} lacks the required representation delta from {:?} to {:?}",
                        tool.name(),
                        previous_profile,
                        profile
                    )));
                }
            }
            previous_measurement = Some((profile, serialized_bytes, o200k_tokens));
            let digest = sha256(&serialized);
            cases.push(GoldenCase {
                tool: tool.name().to_owned(),
                profile,
                output,
            });
            measurements.push(ProfileMeasurement {
                tool: tool.name().to_owned(),
                profile,
                framing: "compact_json_output_object".to_owned(),
                serialized_bytes,
                o200k_tokens,
                sha256: digest,
                semantic_identity_equal: true,
                envelope_invariants_preserved: true,
                hard_budget_ceiling_unchanged: true,
            });
        }

        matrix_tools.push(MatrixTool {
            tool: tool.name().to_owned(),
            support,
        });
    }

    if !canonical_outputs.is_empty() {
        return Err(ResponseProfileEvidenceError::Contract(
            "source examples contain tools outside the canonical catalog".to_owned(),
        ));
    }
    let total_serialized_bytes = measurements
        .iter()
        .try_fold(0_u64, |total, measurement| {
            total.checked_add(measurement.serialized_bytes)
        })
        .ok_or_else(|| {
            ResponseProfileEvidenceError::Contract("serialized-byte total overflowed".to_owned())
        })?;
    let total_o200k_tokens = measurements
        .iter()
        .try_fold(0_u64, |total, measurement| {
            total.checked_add(measurement.o200k_tokens)
        })
        .ok_or_else(|| {
            ResponseProfileEvidenceError::Contract("token total overflowed".to_owned())
        })?;

    Ok(EvidenceBundle {
        canonical: CanonicalBundle {
            schema: CANONICAL_SCHEMA.to_owned(),
            source_fixture: SOURCE_EXAMPLES.to_owned(),
            cases: McpTool::ALL
                .into_iter()
                .map(|tool| {
                    let output = examples.remove(tool.name()).ok_or_else(|| {
                        ResponseProfileEvidenceError::Contract(format!(
                            "missing canonical source for {}",
                            tool.name()
                        ))
                    })?;
                    Ok(CanonicalCase {
                        tool: tool.name().to_owned(),
                        output: representative_output(tool, &output, &source_ref)?,
                    })
                })
                .collect::<Result<Vec<_>, ResponseProfileEvidenceError>>()?,
        },
        matrix: ProfileMatrix {
            schema: MATRIX_SCHEMA.to_owned(),
            source: "canonical capability registry".to_owned(),
            tool_count: matrix_tools.len(),
            tools: matrix_tools,
        },
        goldens: GoldenBundle {
            schema: GOLDEN_SCHEMA.to_owned(),
            source_fixture: format!("tests/fixtures/mcp/response-profiles/{CANONICAL_FILE}"),
            cases,
        },
        report: TokenReport {
            schema: REPORT_SCHEMA.to_owned(),
            tokenizer: tokenizer_identity,
            measurements,
            total_serialized_bytes,
            total_o200k_tokens,
        },
    })
}

fn advertised_profiles(support: ResponseProfileSupport) -> Vec<ResponseProfile> {
    match support {
        ResponseProfileSupport::Fixed { representation } => vec![representation],
        ResponseProfileSupport::Selectable { supported, .. } => PROFILE_ORDER
            .into_iter()
            .filter(|profile| supported.contains(profile))
            .collect(),
    }
}

fn owned_support(support: ResponseProfileSupport) -> OwnedProfileSupport {
    match support {
        ResponseProfileSupport::Fixed { representation } => {
            OwnedProfileSupport::Fixed { representation }
        }
        ResponseProfileSupport::Selectable {
            wire_field,
            supported,
            default,
        } => OwnedProfileSupport::Selectable {
            wire_field: wire_field.name().to_owned(),
            supported: advertised_profiles(ResponseProfileSupport::Selectable {
                wire_field,
                supported,
                default,
            }),
            default,
        },
    }
}

fn representative_output(
    tool: McpTool,
    canonical: &Value,
    source_ref: &Value,
) -> Result<Value, ResponseProfileEvidenceError> {
    let mut output = canonical.clone();
    let symbol_id = "sym1_cecigxytq5fdpxizkjlxeqzrbmtnd2odobb4eey";
    let file_id = source_ref
        .pointer("/span/file")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(
                "canonical source reference has no file identity".to_owned(),
            )
        })?;
    let refs = |count: usize| vec![source_ref.clone(); count];
    let reasons = || {
        vec![
            Value::from("reason-0"),
            Value::from("reason-1"),
            Value::from("reason-2"),
            Value::from("reason-3"),
            Value::from("reason-4"),
            Value::from("reason-5"),
        ]
    };
    let data = match tool {
        McpTool::CodeLocate => serde_json::json!({
            "matches": [{
                "symbol_id": symbol_id,
                "file_id": file_id,
                "kind": "function",
                "display_name": "profile_target",
                "signature": "fn profile_target() -> bool",
                "path": "src/profile.rs",
                "score": 990,
                "why": [
                    "identifier_match",
                    "lexical_match",
                    "docs_match",
                    "path_match",
                    "structural_match"
                ],
                "source_ref": source_ref,
                "trust": "untrusted_repository_data"
            }],
            "query_interpretation": {
                "tokens": ["profile", "target"],
                "modes": ["exact", "lexical"],
                "semantic_available": true
            },
            "suggested_next": []
        }),
        McpTool::SymbolExplain => serde_json::json!({
            "symbols": [{
                "symbol_id": symbol_id,
                "kind": "function",
                "display_name": "profile_target",
                "signature": "fn profile_target() -> bool",
                "definition": source_ref,
                "relations": {
                    "outbound_exact": 2,
                    "outbound_candidates": 3,
                    "inbound_exact": 5,
                    "inbound_candidates": 7,
                    "references_exact": 11
                },
                "provenance": (0_u16..6).map(|index| serde_json::json!({
                    "provider": format!("provider-{index}"),
                    "evidence": format!("evidence-{index}"),
                    "confidence": 900 - index
                })).collect::<Vec<_>>(),
                "confidence": 925,
                "uncertainty": [],
                "trust": "untrusted_repository_data"
            }],
            "unresolved_ids": [],
            "detail_handles": [{
                "handle": "detail-handle",
                "kind": "source-preview"
            }]
        }),
        McpTool::SymbolRelationships => serde_json::json!({
            "groups": [{
                "seed": symbol_id,
                "relation": "calls",
                "direction": "outbound",
                "items": [{
                    "symbol_id": symbol_id,
                    "confidence": 880,
                    "source_refs": refs(3),
                    "provenance": (0_u16..3).map(|index| serde_json::json!({
                        "provider": format!("provider-{index}"),
                        "evidence": format!("edge-{index}"),
                        "confidence": 850 - index
                    })).collect::<Vec<_>>(),
                    "trust": "untrusted_repository_data"
                }],
                "total_count": 9
            }],
            "unresolved": [],
            "totals": {
                "returned_edges": 1,
                "total_edges": 9,
                "exact": false
            }
        }),
        McpTool::FlowTrace => serde_json::json!({
            "paths": [{
                "confidence": 870,
                "nodes": [symbol_id, symbol_id],
                "edges": [{
                    "kind": "data_flow",
                    "confidence": 860,
                    "source_refs": refs(6),
                    "trust": "untrusted_repository_data"
                }],
                "cyclic": false
            }],
            "frontier": {
                "reached_nodes": 13,
                "examined_edges": 21,
                "truncated": true,
                "unresolved_boundaries": 3
            },
            "projection": {
                "relations": ["calls", "data_flow"],
                "min_confidence": 700
            }
        }),
        McpTool::ChangeImpact => serde_json::json!({
            "resolved_changes": [{
                "symbol_id": symbol_id,
                "file_id": file_id,
                "classification": "body",
                "kind": "function"
            }],
            "impacted": [{
                "source_index": 0,
                "dependents": [{
                    "symbol_id": symbol_id,
                    "kind": "function",
                    "distance": 2,
                    "confidence": 875,
                    "via": ["calls", "depends_on"],
                    "is_public": true
                }]
            }],
            "service_impacts": [],
            "tests": [{
                "test_id": "profile-contract",
                "relevance": 980,
                "why": reasons(),
                "estimated_cost_ms": 250
            }],
            "risk_summary": {
                "level": "medium",
                "reasons": ["public-surface", "transitive"],
                "coverage": "complete",
                "breaking_surface": true,
                "fanout": 1,
                "dynamic_blind_spots": false
            }
        }),
        McpTool::TestsSelect => serde_json::json!({
            "tests": [{
                "test_id": "profile-contract",
                "kind": "unit",
                "path": "tests/profile_contract.rs",
                "score": 975,
                "why": reasons(),
                "estimated_cost_ms": 250,
                "command_hint": "cargo test -p rootlight-agent"
            }],
            "coverage_strategy": {
                "direct_edges": true,
                "transitive_signals": true,
                "history_signals": false,
                "file_colocation_signals": true
            },
            "gaps": []
        }),
        McpTool::ArchitectureOverview => serde_json::json!({
            "components": [{
                "id": "component-a",
                "kind": "crate",
                "name": "agent",
                "symbol_count": 17,
                "responsibility_evidence": [
                    "responsibility-0",
                    "responsibility-1",
                    "responsibility-2",
                    "responsibility-3",
                    "responsibility-4",
                    "responsibility-5"
                ],
                "confidence": 940,
                "trust": "untrusted_repository_data"
            }],
            "connections": [{
                "from": "component-a",
                "to": "component-b",
                "kind": "build_dependency",
                "weight": 5,
                "confidence": 910
            }],
            "hotspots": [{
                "component_id": "component-a",
                "fan_in": 8,
                "fan_out": 13,
                "change_frequency": 21,
                "complexity": 34,
                "score": 915
            }],
            "views": [{
                "view": "modules",
                "algorithm_version": "modules-v1"
            }]
        }),
        McpTool::ArchitectureCycles => serde_json::json!({
            "components": [{
                "size": 2,
                "members": ["component-a", "component-b"],
                "internal_edges": 3
            }],
            "cycles": [{
                "nodes": ["component-a", "component-b", "component-a"],
                "edge_evidence": refs(6),
                "confidence": 900
            }],
            "break_candidates": [{
                "from": "component-a",
                "to": "component-b",
                "kind": "imports",
                "break_cost": 420,
                "source_refs": refs(6)
            }]
        }),
        McpTool::CodeDead => serde_json::json!({
            "candidates": [{
                "symbol_id": symbol_id,
                "classification": "not_observed_from_entry_points_strong_references",
                "confidence": 900,
                "why": reasons(),
                "suppressions_checked": [
                    "entry-point",
                    "public-export",
                    "reflection-hook"
                ],
                "source_refs": refs(6),
                "trust": "untrusted_repository_data"
            }],
            "entry_points": {
                "policy": "standard",
                "entry_point_count": 7,
                "complete": false
            },
            "blind_spots": [{
                "category": "dynamic-dispatch",
                "affected_count": 3
            }],
            "false_positive_controls": [{
                "rule": "exported-symbol",
                "suppressed_count": 5
            }]
        }),
        McpTool::PlanChange => serde_json::json!({
            "plan": [{
                "step": 1,
                "action": "update the bounded response",
                "targets": [symbol_id],
                "depends_on": [],
                "risks": ["public-contract"],
                "verification": "run focused contract tests"
            }],
            "affected_scope": {
                "affected_symbols": 3,
                "affected_files": 2,
                "risk_level": "medium",
                "touches_public_surface": true
            },
            "test_plan": [{
                "test_id": "profile-contract",
                "relevance": 980,
                "why": reasons(),
                "estimated_cost_ms": 250
            }],
            "open_decisions": [{
                "question": "preserve wire compatibility",
                "recommended_default": "yes"
            }],
            "context_pack_request": {
                "symbols": [symbol_id],
                "files": [file_id]
            }
        }),
        McpTool::ContextPack => {
            let target = output
                .get_mut("data")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    ResponseProfileEvidenceError::Contract(
                        "context.pack source example has no data object".to_owned(),
                    )
                })?;
            target.insert(
                "items".to_owned(),
                serde_json::json!([{
                    "role": "definition",
                    "symbol_id": symbol_id,
                    "source_ref": source_ref,
                    "signature": "fn profile_target() -> bool",
                    "score": 990,
                    "tokens": 1500,
                    "trust": "untrusted_repository_data",
                    "snippet": {
                        "source_ref": source_ref,
                        "content": "x".repeat(6000),
                        "language": "rust",
                        "provenance": "source_read",
                        "truncated": false,
                        "trust": "untrusted_repository_data"
                    }
                }]),
            );
            if let Some(role) = target
                .get_mut("role_coverage")
                .and_then(|coverage| coverage.get_mut("roles"))
                .and_then(Value::as_array_mut)
                .and_then(|roles| roles.first_mut())
                .and_then(Value::as_object_mut)
            {
                role.remove("missing_reason");
                role.insert("observed_candidates".to_owned(), Value::from(1));
                role.insert("selected_items".to_owned(), Value::from(1));
                role.insert("status".to_owned(), Value::from("satisfied"));
            }
            return Ok(output);
        }
        McpTool::QueryBatch => {
            let child_data = representative_output(McpTool::CodeLocate, canonical, source_ref)?
                .get("data")
                .cloned()
                .ok_or_else(|| {
                    ResponseProfileEvidenceError::Contract(
                        "representative code.locate output has no data".to_owned(),
                    )
                })?;
            let result = output
                .pointer_mut("/data/operation_results/0")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| {
                    ResponseProfileEvidenceError::Contract(
                        "query.batch source example has no first operation".to_owned(),
                    )
                })?;
            result.insert("data".to_owned(), child_data);
            return Ok(output);
        }
        McpTool::RepoIndex
        | McpTool::RepoStatus
        | McpTool::RepoList
        | McpTool::OperationStatus
        | McpTool::HistoryCompare
        | McpTool::SourceRead
        | McpTool::QueryAdvanced => return Ok(output),
    };
    let target = output.get_mut("data").ok_or_else(|| {
        ResponseProfileEvidenceError::Contract(format!(
            "{} source example has no data",
            tool.name()
        ))
    })?;
    *target = data;
    if let Some(batch_tool) = analytical_batch_tool(tool) {
        shape_batch_child_data(batch_tool, target, ResponseProfile::Evidence).map_err(|error| {
            ResponseProfileEvidenceError::Projection {
                tool: tool.name(),
                profile: ResponseProfile::Evidence,
                message: format!("{error:?}"),
            }
        })?;
    }
    Ok(output)
}

fn shape_output(
    tool: McpTool,
    canonical: &Value,
    profile: ResponseProfile,
) -> Result<Value, ResponseProfileEvidenceError> {
    let mut output = canonical.clone();
    if let Some(batch_tool) = analytical_batch_tool(tool) {
        let data = output.get_mut("data").ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(format!(
                "{} source example has no data",
                tool.name()
            ))
        })?;
        *data = shape_batch_child_data(batch_tool, data, profile).map_err(|error| {
            ResponseProfileEvidenceError::Projection {
                tool: tool.name(),
                profile,
                message: format!("{error:?}"),
            }
        })?;
    } else if matches!(tool, McpTool::QueryBatch) {
        shape_batch_output(&mut output, profile)?;
    }
    Ok(output)
}

fn shape_batch_output(
    output: &mut Value,
    profile: ResponseProfile,
) -> Result<(), ResponseProfileEvidenceError> {
    let results = output
        .pointer_mut("/data/operation_results")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(
                "query.batch source example has no operation_results".to_owned(),
            )
        })?;
    for result in results {
        let tool_value = result.get("tool").cloned().ok_or_else(|| {
            ResponseProfileEvidenceError::Contract(
                "query.batch operation result has no tool".to_owned(),
            )
        })?;
        let Some(data) = result.get_mut("data") else {
            continue;
        };
        let tool: BatchTool = serde_json::from_value(tool_value).map_err(|source| {
            ResponseProfileEvidenceError::JsonValue {
                context: "query.batch child tool",
                source,
            }
        })?;
        *data = shape_batch_child_data(tool, data, profile).map_err(|error| {
            ResponseProfileEvidenceError::Projection {
                tool: McpTool::QueryBatch.name(),
                profile,
                message: format!("{error:?}"),
            }
        })?;
    }
    Ok(())
}

fn analytical_batch_tool(tool: McpTool) -> Option<BatchTool> {
    match tool {
        McpTool::CodeLocate => Some(BatchTool::CodeLocate),
        McpTool::SymbolExplain => Some(BatchTool::SymbolExplain),
        McpTool::SymbolRelationships => Some(BatchTool::SymbolRelationships),
        McpTool::FlowTrace => Some(BatchTool::FlowTrace),
        McpTool::ChangeImpact => Some(BatchTool::ChangeImpact),
        McpTool::TestsSelect => Some(BatchTool::TestsSelect),
        McpTool::ArchitectureOverview => Some(BatchTool::ArchitectureOverview),
        McpTool::ArchitectureCycles => Some(BatchTool::ArchitectureCycles),
        McpTool::CodeDead => Some(BatchTool::CodeDead),
        McpTool::PlanChange => Some(BatchTool::PlanChange),
        McpTool::ContextPack => Some(BatchTool::ContextPack),
        McpTool::RepoIndex
        | McpTool::RepoStatus
        | McpTool::RepoList
        | McpTool::OperationStatus
        | McpTool::HistoryCompare
        | McpTool::SourceRead
        | McpTool::QueryAdvanced
        | McpTool::QueryBatch => None,
    }
}

fn normalize_output(tool: McpTool, output: &Value) -> Result<Value, ResponseProfileEvidenceError> {
    let mut normalized = shape_output(tool, output, ResponseProfile::Compact)?;
    normalize_usage_counters(&mut normalized);
    Ok(normalized)
}

fn invariant_projection_for(output: &Value) -> Value {
    let mut projection = output.clone();
    if let Some(object) = projection.as_object_mut() {
        object.remove("data");
    }
    normalize_usage_counters(&mut projection);
    projection
}

fn normalize_usage_counters(output: &mut Value) {
    if let Some(usage) = output.get_mut("usage").and_then(Value::as_object_mut) {
        usage.remove("json_bytes");
        usage.remove("estimated_tokens");
    }
}

fn solve_usage_counters(output: &mut Value) -> Result<(), ResponseProfileEvidenceError> {
    if output.get("usage").is_none() {
        return Ok(());
    }
    for _ in 0..8 {
        let serialized = serde_json::to_vec(output).map_err(|source| {
            ResponseProfileEvidenceError::JsonValue {
                context: "measured profile output",
                source,
            }
        })?;
        let json_bytes = u64::try_from(serialized.len()).map_err(|_| {
            ResponseProfileEvidenceError::Contract(
                "serialized output length does not fit u64".to_owned(),
            )
        })?;
        let estimated_tokens = estimate_tokens(serialized.len());
        let usage = output
            .get_mut("usage")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                ResponseProfileEvidenceError::Contract("usage must be a JSON object".to_owned())
            })?;
        let current_bytes = usage.get("json_bytes").and_then(Value::as_u64);
        let current_tokens = usage.get("estimated_tokens").and_then(Value::as_u64);
        if current_bytes == Some(json_bytes) && current_tokens == Some(estimated_tokens) {
            return Ok(());
        }
        usage.insert("json_bytes".to_owned(), Value::from(json_bytes));
        usage.insert("estimated_tokens".to_owned(), Value::from(estimated_tokens));
    }
    Err(ResponseProfileEvidenceError::Contract(
        "self-describing usage counters did not converge".to_owned(),
    ))
}

fn verify_file<T>(path: &Path, expected: &T) -> Result<(), ResponseProfileEvidenceError>
where
    T: Serialize,
{
    let bytes = read_bounded(path)?;
    let observed: Value =
        serde_json::from_slice(&bytes).map_err(|source| ResponseProfileEvidenceError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    let expected = serde_json::to_value(expected).map_err(|source| {
        ResponseProfileEvidenceError::JsonValue {
            context: "expected evidence",
            source,
        }
    })?;
    if observed != expected {
        return Err(ResponseProfileEvidenceError::Mismatch(path.to_path_buf()));
    }
    Ok(())
}

fn write_json<T>(path: &Path, value: &T) -> Result<(), ResponseProfileEvidenceError>
where
    T: Serialize,
{
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|source| {
        ResponseProfileEvidenceError::JsonValue {
            context: "retained evidence",
            source,
        }
    })?;
    bytes.push(b'\n');
    fs::write(path, bytes).map_err(|source| ResponseProfileEvidenceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ResponseProfileEvidenceError> {
    let metadata = fs::metadata(path).map_err(|source| ResponseProfileEvidenceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_FIXTURE_BYTES {
        return Err(ResponseProfileEvidenceError::FixtureTooLarge(
            path.to_path_buf(),
        ));
    }
    fs::read(path).map_err(|source| ResponseProfileEvidenceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

#[derive(Debug)]
struct EvidenceBundle {
    canonical: CanonicalBundle,
    matrix: ProfileMatrix,
    goldens: GoldenBundle,
    report: TokenReport,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalBundle {
    schema: String,
    source_fixture: String,
    cases: Vec<CanonicalCase>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct CanonicalCase {
    tool: String,
    output: Value,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileMatrix {
    schema: String,
    source: String,
    tool_count: usize,
    tools: Vec<MatrixTool>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct MatrixTool {
    tool: String,
    support: OwnedProfileSupport,
}

#[derive(Debug, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
enum OwnedProfileSupport {
    Fixed {
        representation: ResponseProfile,
    },
    Selectable {
        wire_field: String,
        supported: Vec<ResponseProfile>,
        default: ResponseProfile,
    },
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct GoldenBundle {
    schema: String,
    source_fixture: String,
    cases: Vec<GoldenCase>,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct GoldenCase {
    tool: String,
    profile: ResponseProfile,
    output: Value,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct TokenReport {
    schema: String,
    tokenizer: Value,
    measurements: Vec<ProfileMeasurement>,
    total_serialized_bytes: u64,
    total_o200k_tokens: u64,
}

#[derive(Debug, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileMeasurement {
    tool: String,
    profile: ResponseProfile,
    framing: String,
    serialized_bytes: u64,
    o200k_tokens: u64,
    sha256: String,
    semantic_identity_equal: bool,
    envelope_invariants_preserved: bool,
    hard_budget_ceiling_unchanged: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompatibilityExamples {
    schema: String,
    tools: Vec<CompatibilityExample>,
}

#[derive(Debug, Deserialize)]
struct CompatibilityExample {
    tool: String,
    output: Value,
}

/// Failure while generating or checking retained response-profile evidence.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResponseProfileEvidenceError {
    /// A required option value is absent.
    #[error("missing value for {0}")]
    MissingArgument(&'static str),
    /// An unknown or duplicate option was supplied.
    #[error("unexpected response-profile-check argument: {0}")]
    UnexpectedArgument(String),
    /// A retained fixture exceeded the offline gate's bounded read limit.
    #[error("response-profile fixture exceeds the size limit: {}", .0.display())]
    FixtureTooLarge(PathBuf),
    /// One evidence file could not be read, created, or written.
    #[error("response-profile evidence I/O failed for {}", path.display())]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// One evidence file was not valid JSON.
    #[error("response-profile evidence JSON failed for {}", path.display())]
    Json {
        /// Affected path.
        path: PathBuf,
        /// Underlying JSON failure.
        #[source]
        source: serde_json::Error,
    },
    /// An in-memory contract value could not be represented as JSON.
    #[error("response-profile {context} serialization failed")]
    JsonValue {
        /// Stable serialization context.
        context: &'static str,
        /// Underlying JSON failure.
        #[source]
        source: serde_json::Error,
    },
    /// Typed agent shaping rejected a canonical compatibility example.
    #[error("response-profile projection failed for {tool} {profile:?}: {message}")]
    Projection {
        /// Public tool name.
        tool: &'static str,
        /// Requested representation.
        profile: ResponseProfile,
        /// Source-free projection failure.
        message: String,
    },
    /// A generated matrix, golden, or report violated the evidence contract.
    #[error("response-profile evidence contract failed: {0}")]
    Contract(String),
    /// Retained evidence differs from the canonical regenerated value.
    #[error("response-profile evidence is stale: {}", .0.display())]
    Mismatch(PathBuf),
    /// The pinned offline tokenizer failed.
    #[error(transparent)]
    Tokenizer(#[from] crate::token_accounting::TokenAccountingError),
}
