//! Offline conformance evidence for public resource budgets.
//!
//! The retained report is derived from the canonical capability registry and
//! executable ledger cases. Runtime-only claims are limited to source-bound
//! regression tests; this gate does not turn source inspection into a latency
//! measurement.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use rootlight_agent::policy::{
    BudgetCharge, BudgetLedger, BudgetLimits, BudgetResource, ExecutionPolicyError,
};
use rootlight_bench::ActualTokenizerIdentity;
use rootlight_mcp_contract::{
    McpTool,
    accounting::estimate_tokens,
    capability::{BudgetSemantics, CAPABILITIES},
    completeness::{
        CompletenessState, ContinuationAvailability, ContinuationGuidance, LimitingResource,
        LimitingResourceKind, ResultCompleteness,
    },
    vertical::ResponseBudget,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::token_accounting::{O200kTokenizer, OfflineTokenizer};

const REPORT_SCHEMA: &str = "rootlight.mcp-budget-conformance/1";
const REPORT_FILE: &str = "v1.json";
const ACCOUNTING_INPUT_SCHEMA: &str = "rootlight.mcp-budget-conformance-input/1";
const MAX_REPORT_BYTES: usize = 2 * 1024 * 1024;
const NORMALIZATION: &str = "none_exact_utf8";
const FRAMING: &str = "canonical_compact_json_without_accounting_measurement";
const RUNTIME_REPORT_SCHEMA: &str = "rootlight.mcp-budget-runtime/1";
const CANCELLATION_REPORT_SCHEMA: &str = "rootlight.mcp-cancellation-process/1";
const RUNTIME_COVERED_TOOLS: [&str; 14] = [
    "code.locate",
    "symbol.explain",
    "symbol.relationships",
    "flow.trace",
    "change.impact",
    "tests.select",
    "architecture.overview",
    "architecture.cycles",
    "code.dead",
    "history.compare",
    "plan.change",
    "context.pack",
    "source.read",
    "query.advanced",
];

const SOURCE_PROOFS: [SourceProofDeclaration; 4] = [
    SourceProofDeclaration {
        file: "crates/rootlight-agent/src/policy.rs",
        symbols: &[
            "every_dimension_accepts_below_and_exact_but_rejects_one_above",
            "child_limits_never_exceed_parent_tool_or_local_caps",
        ],
    },
    SourceProofDeclaration {
        file: "crates/rootlight-query/tests/query_slice.rs",
        symbols: &[
            "execution_enforces_cancellation_and_exact_output_bounds",
            "plans_and_execution_enforce_all_query_resource_families",
            "symbol_relationships_enforces_plan_and_serialization_limits",
        ],
    },
    SourceProofDeclaration {
        file: "apps/rootlight-mcp/src/executor/tests.rs",
        symbols: &[
            "analytical_budget_lowers_every_public_resource_dimension",
            "omitted_analytical_budget_transports_the_complete_server_ceiling",
            "final_serialization_enforces_exact_byte_and_conservative_token_boundaries",
            "any_lower_layer_truncation_survives_final_serialization",
            "cancellation_drops_a_pending_client_port_future",
            "query_batch_enforces_aggregate_budget_across_the_app_boundary",
        ],
    },
    SourceProofDeclaration {
        file: "apps/rootlight-daemon/src/first_slice.rs",
        symbols: &[
            "peer_cancellation_leaves_work_lane_reusable_before_publication",
            "durable_commit_boundary_failure_is_terminal_and_publishes_nothing",
        ],
    },
];

const LEDGER_RESOURCES: [BudgetResource; 11] = [
    BudgetResource::Rows,
    BudgetResource::Results,
    BudgetResource::Tokens,
    BudgetResource::ActualTokens,
    BudgetResource::SourceBytes,
    BudgetResource::TraversalFacts,
    BudgetResource::Depth,
    BudgetResource::Paths,
    BudgetResource::JsonBytes,
    BudgetResource::MemoryBytes,
    BudgetResource::Time,
];

/// Options for the retained budget-conformance gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Options {
    fixture_root: PathBuf,
    refresh: bool,
    runtime_report: Option<PathBuf>,
    cancellation_report: Option<PathBuf>,
    output: Option<PathBuf>,
}

impl Options {
    /// Parses an optional fixture root and explicit refresh mode.
    pub(crate) fn parse(
        arguments: &mut impl Iterator<Item = String>,
    ) -> Result<Self, BudgetConformanceError> {
        let mut fixture_root = None;
        let mut refresh = false;
        let mut runtime_report = None;
        let mut cancellation_report = None;
        let mut output = None;
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--fixture-root" if fixture_root.is_none() => {
                    fixture_root =
                        Some(PathBuf::from(arguments.next().ok_or(
                            BudgetConformanceError::MissingArgument("--fixture-root"),
                        )?));
                }
                "--refresh" if !refresh => refresh = true,
                "--runtime-report" if runtime_report.is_none() => {
                    runtime_report =
                        Some(PathBuf::from(arguments.next().ok_or(
                            BudgetConformanceError::MissingArgument("--runtime-report"),
                        )?));
                }
                "--cancellation-report" if cancellation_report.is_none() => {
                    cancellation_report = Some(PathBuf::from(arguments.next().ok_or(
                        BudgetConformanceError::MissingArgument("--cancellation-report"),
                    )?));
                }
                "--output" if output.is_none() => {
                    output =
                        Some(PathBuf::from(arguments.next().ok_or(
                            BudgetConformanceError::MissingArgument("--output"),
                        )?));
                }
                _ => return Err(BudgetConformanceError::UnexpectedArgument(argument)),
            }
        }
        let options = Self {
            fixture_root: fixture_root.unwrap_or_else(default_fixture_root),
            refresh,
            runtime_report,
            cancellation_report,
            output,
        };
        let runtime_option_count = [
            options.runtime_report.is_some(),
            options.cancellation_report.is_some(),
            options.output.is_some(),
        ]
        .into_iter()
        .filter(|present| *present)
        .count();
        if runtime_option_count != 0 && runtime_option_count != 3 {
            return Err(BudgetConformanceError::RuntimeArgumentsRequiredTogether);
        }
        if options.refresh && runtime_option_count != 0 {
            return Err(BudgetConformanceError::RefreshWithRuntimeEvidence);
        }
        Ok(options)
    }
}

/// Checks the retained report or refreshes it from canonical offline inputs.
pub(crate) fn check(options: &Options) -> Result<(), BudgetConformanceError> {
    let report_path = options.fixture_root.join(REPORT_FILE);
    if let (Some(runtime_path), Some(cancellation_path), Some(output_path)) = (
        options.runtime_report.as_deref(),
        options.cancellation_report.as_deref(),
        options.output.as_deref(),
    ) {
        let source_revision = current_source_revision()?;
        let runtime_reports =
            validate_runtime_reports(runtime_path, cancellation_path, &source_revision)?;
        let runtime_cases = runtime_reports.runtime_cases;
        let report = build_report(&source_revision, Some(runtime_reports))?;
        write_report(output_path, &report)?;
        println!(
            "budget runtime conformance verified for {} tools at {}",
            runtime_cases, source_revision
        );
        return Ok(());
    }
    if options.refresh {
        let source_revision = current_source_revision()?;
        let report = build_report(&source_revision, None)?;
        fs::create_dir_all(&options.fixture_root).map_err(|source| BudgetConformanceError::Io {
            path: options.fixture_root.clone(),
            source,
        })?;
        let mut encoded = serde_json::to_vec_pretty(&report)?;
        encoded.push(b'\n');
        fs::write(&report_path, encoded).map_err(|source| BudgetConformanceError::Io {
            path: report_path.clone(),
            source,
        })?;
        println!(
            "budget conformance refreshed for {} tools at {}",
            report.tools.len(),
            report_path.display()
        );
        return Ok(());
    }

    let encoded = read_bounded(&report_path)?;
    let retained: BudgetConformanceReport = serde_json::from_slice(&encoded)?;
    validate_revision(&retained.source_revision)?;
    let observed = build_report(&retained.source_revision, None)?;
    if retained != observed {
        return Err(BudgetConformanceError::ReportDrift {
            expected: sha256_hex(&serde_json::to_vec(&retained)?),
            observed: sha256_hex(&serde_json::to_vec(&observed)?),
        });
    }
    println!(
        "budget conformance verified for {} tools at {}",
        retained.tools.len(),
        retained.source_revision
    );
    Ok(())
}

fn default_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/mcp/budget-conformance")
}

fn build_report(
    source_revision: &str,
    runtime_reports: Option<RuntimeReportEvidence>,
) -> Result<BudgetConformanceReport, BudgetConformanceError> {
    validate_revision(source_revision)?;
    let has_runtime_evidence = runtime_reports.is_some();
    let tools = capability_inventory(has_runtime_evidence)?;
    let ledger_boundaries = ledger_boundary_cases()?;
    let effective_limits = effective_limit_evidence()?;
    let completeness = completeness_evidence()?;
    let source_proofs = source_proofs()?;
    let runtime_evidence = runtime_evidence();
    let input = AccountingInput {
        schema: ACCOUNTING_INPUT_SCHEMA,
        source_revision,
        tools: &tools,
        ledger_boundaries: &ledger_boundaries,
        effective_limits: &effective_limits,
        completeness: &completeness,
        source_proofs: &source_proofs,
        runtime_evidence: &runtime_evidence,
        runtime_reports: runtime_reports.as_ref(),
    };
    let accounting_input = serde_json::to_vec(&input)?;
    let tokenizer = O200kTokenizer::new()?;
    let accounting_text = std::str::from_utf8(&accounting_input)
        .map_err(BudgetConformanceError::AccountingInputUtf8)?;
    let token_accounting = TokenAccountingEvidence {
        tokenizer: tokenizer.benchmark_identity(),
        input_schema: ACCOUNTING_INPUT_SCHEMA.to_owned(),
        input_sha256: sha256_hex(&accounting_input),
        serialized_bytes: u64::try_from(accounting_input.len())
            .map_err(|_| BudgetConformanceError::IntegerOverflow)?,
        deterministic_estimated_tokens: estimate_tokens(accounting_input.len()),
        actual_tokens: tokenizer.count(accounting_text)?,
        normalization: NORMALIZATION.to_owned(),
        framing: FRAMING.to_owned(),
    };

    Ok(BudgetConformanceReport {
        schema: REPORT_SCHEMA.to_owned(),
        source_revision: source_revision.to_owned(),
        tools,
        ledger_boundaries,
        effective_limits,
        completeness,
        source_proofs,
        runtime_evidence,
        runtime_reports,
        token_accounting,
        residual_approximations: vec![
            "runtime hard token admission uses a deterministic provider-neutral estimate"
                .to_owned(),
            "actual o200k tokens are offline evidence and are not a universal runtime ceiling"
                .to_owned(),
            if has_runtime_evidence {
                "cancellation latency is sampled for representative active daemon analyses, not every budgeted tool".to_owned()
            } else {
                "source-bound cancellation proofs do not claim a retained latency sample".to_owned()
            },
        ],
    })
}

fn capability_inventory(
    has_runtime_evidence: bool,
) -> Result<Vec<ToolBudgetInventory>, BudgetConformanceError> {
    if CAPABILITIES.len() != McpTool::ALL.len() {
        return Err(BudgetConformanceError::CapabilityCount {
            expected: McpTool::ALL.len(),
            observed: CAPABILITIES.len(),
        });
    }
    CAPABILITIES
        .iter()
        .zip(McpTool::ALL)
        .map(|(capability, tool)| {
            if capability.tool != tool {
                return Err(BudgetConformanceError::CapabilityOrder(tool.name()));
            }
            Ok(ToolBudgetInventory {
                tool: tool.name().to_owned(),
                budget_semantics: capability.budget.name().to_owned(),
                default_token_budget: tool.default_token_budget(),
                batch_shared_budget: capability.batch_shared_budget,
                declared_budget_dimensions: declared_tool_dimensions(tool, capability.budget)
                    .iter()
                    .map(|dimension| (*dimension).to_owned())
                    .collect(),
                runtime_trigger_coverage: runtime_trigger_coverage(
                    tool,
                    capability.budget,
                    has_runtime_evidence,
                )
                .to_owned(),
                omitted_client_budget_disposition: omitted_budget_disposition(capability.budget)
                    .to_owned(),
            })
        })
        .collect()
}

fn declared_tool_dimensions(tool: McpTool, semantics: BudgetSemantics) -> &'static [&'static str] {
    const SHARED: &[&str] = &[
        "rows",
        "results",
        "estimated_tokens",
        "actual_tokens_when_available",
        "source_bytes",
        "traversal_facts",
        "depth",
        "paths",
        "response_bytes",
        "memory_bytes",
        "wall_time",
    ];
    match semantics {
        BudgetSemantics::PerRequest if tool == McpTool::QueryAdvanced => &[
            "rows",
            "results",
            "plan_cost",
            "traversal_facts",
            "depth",
            "response_bytes",
            "estimated_tokens",
            "memory_bytes",
            "wall_time",
        ],
        BudgetSemantics::PerRequest => SHARED,
        BudgetSemantics::TokenBudget => &[
            "estimated_tokens",
            "response_bytes",
            "source_bytes",
            "wall_time",
        ],
        BudgetSemantics::Unsupported => &["response_bytes", "wall_time"],
        BudgetSemantics::None => match tool {
            McpTool::RepoIndex => &[
                "files",
                "source_bytes",
                "memory_bytes",
                "wall_time",
                "cancellation",
            ],
            McpTool::RepoList => &["page_size", "response_bytes", "wall_time"],
            McpTool::OperationStatus => &["single_operation", "response_bytes", "wall_time"],
            _ => &["response_bytes", "wall_time"],
        },
    }
}

fn runtime_trigger_coverage(
    tool: McpTool,
    semantics: BudgetSemantics,
    has_runtime_evidence: bool,
) -> &'static str {
    match (tool, semantics) {
        (McpTool::QueryBatch, BudgetSemantics::PerRequest) => "covered_source_bound",
        (_, BudgetSemantics::Unsupported) => "unsupported",
        (_, BudgetSemantics::None) => "not_applicable",
        (_, BudgetSemantics::PerRequest | BudgetSemantics::TokenBudget)
            if has_runtime_evidence && RUNTIME_COVERED_TOOLS.contains(&tool.name()) =>
        {
            "covered_real_process"
        }
        (_, BudgetSemantics::PerRequest | BudgetSemantics::TokenBudget) => "not_measured",
    }
}

const fn omitted_budget_disposition(semantics: BudgetSemantics) -> &'static str {
    match semantics {
        BudgetSemantics::PerRequest | BudgetSemantics::TokenBudget => "server_ceiling_applies",
        BudgetSemantics::Unsupported => "budget_field_rejected_before_work",
        BudgetSemantics::None => "operation_has_server_owned_fixed_bounds",
    }
}

fn ledger_boundary_cases() -> Result<Vec<LedgerBoundaryEvidence>, BudgetConformanceError> {
    LEDGER_RESOURCES
        .into_iter()
        .map(|resource| {
            let limits = BudgetLimits::from_maximums(charge_for(resource, 1));
            let below_accepted = BudgetLedger::from_limits(limits)
                .charge(charge_for(resource, 0))
                .is_ok();
            let exact_accepted = BudgetLedger::from_limits(limits)
                .charge(charge_for(resource, 1))
                .is_ok();
            let above = BudgetLedger::from_limits(limits).charge(charge_for(resource, 2));
            let one_above_rejected = matches!(
                above,
                Err(ExecutionPolicyError::BudgetExceeded { resource: observed })
                    if observed == resource
            );
            let zero_limit_rejects_work = matches!(
                BudgetLedger::from_limits(BudgetLimits::from_maximums(charge_for(resource, 0)))
                    .charge(charge_for(resource, 1)),
                Err(ExecutionPolicyError::BudgetExceeded { resource: observed })
                    if observed == resource
            );
            if !(below_accepted && exact_accepted && one_above_rejected && zero_limit_rejects_work)
            {
                return Err(BudgetConformanceError::LedgerBoundary(resource_name(
                    resource,
                )));
            }
            Ok(LedgerBoundaryEvidence {
                resource: resource_name(resource).to_owned(),
                below_accepted,
                exact_accepted,
                one_above_rejected,
                zero_limit_rejects_work,
                stable_error: "budget_exceeded".to_owned(),
            })
        })
        .collect()
}

fn effective_limit_evidence() -> Result<EffectiveLimitEvidence, BudgetConformanceError> {
    let ceiling = BudgetLimits::server_ceiling().maximums();
    let requested = ResponseBudget {
        max_results: Some(1),
        max_tokens: Some(1),
        max_source_bytes: Some(1),
        max_traversal_facts: Some(1),
        max_depth: Some(1),
        max_paths: Some(1),
        timeout_ms: Some(1),
        evidence_level: None,
    };
    let reduced = BudgetLedger::new(Some(requested.clone()))
        .limits()
        .maximums();
    let client_reduces_all_accepted_dimensions = reduced.results == 1
        && reduced.tokens == 1
        && reduced.source_bytes == 1
        && reduced.traversal_facts == 1
        && reduced.depth == 1
        && reduced.paths == 1
        && reduced.time_ms == 1
        && charge_not_above(reduced, ceiling);

    let attempted_raise = ResponseBudget {
        max_results: Some(u16::MAX),
        max_tokens: Some(u16::MAX),
        max_source_bytes: Some(u32::MAX),
        max_traversal_facts: Some(u32::MAX),
        max_depth: Some(u8::MAX),
        max_paths: Some(u16::MAX),
        timeout_ms: Some(u32::MAX),
        evidence_level: None,
    };
    let raised = BudgetLedger::new(Some(attempted_raise)).limits().maximums();
    let client_cannot_raise_server_ceiling = charge_not_above(raised, ceiling);

    let parent_limits = BudgetLimits::from_maximums(uniform_charge(10));
    let tool_limits = BudgetLimits::from_maximums(uniform_charge(8));
    let mut parent = BudgetLedger::from_limits(parent_limits);
    let child = parent.allocate_child(tool_limits, Some(&requested))?;
    let child_maximums = child.limits().maximums();
    let child_never_exceeds_parent_or_tool = charge_not_above(child_maximums, uniform_charge(8))
        && charge_not_above(child_maximums, uniform_charge(10));
    child.release();

    let mut admission_ledger =
        BudgetLedger::from_limits(BudgetLimits::from_maximums(uniform_charge(1)));
    let reservation_is_checked_before_work = matches!(
        admission_ledger.reserve(uniform_charge(2)),
        Err(ExecutionPolicyError::BudgetExceeded { .. })
    ) && admission_ledger.consumed()
        == BudgetCharge::default()
        && admission_ledger.snapshot().reserved() == BudgetCharge::default();

    let mut commit_ledger =
        BudgetLedger::from_limits(BudgetLimits::from_maximums(uniform_charge(1)));
    let measured_commit_cannot_exceed_reservation = match commit_ledger.reserve(uniform_charge(1)) {
        Ok(reservation) => matches!(
            reservation.commit(uniform_charge(2)),
            Err(ExecutionPolicyError::BudgetExceeded { .. })
        ),
        Err(_) => false,
    } && commit_ledger.consumed()
        == BudgetCharge::default()
        && commit_ledger.snapshot().reserved() == BudgetCharge::default();

    if !(client_reduces_all_accepted_dimensions
        && client_cannot_raise_server_ceiling
        && child_never_exceeds_parent_or_tool
        && reservation_is_checked_before_work
        && measured_commit_cannot_exceed_reservation)
    {
        return Err(BudgetConformanceError::EffectiveLimits);
    }
    Ok(EffectiveLimitEvidence {
        omitted_budget_uses_complete_server_ceiling: BudgetLedger::new(None).limits()
            == BudgetLimits::server_ceiling(),
        client_reduces_all_accepted_dimensions,
        client_cannot_raise_server_ceiling,
        child_never_exceeds_parent_or_tool,
        reservation_is_checked_before_work,
        measured_commit_cannot_exceed_reservation,
    })
}

fn completeness_evidence() -> Result<Vec<CompletenessEvidence>, BudgetConformanceError> {
    LEDGER_RESOURCES
        .into_iter()
        .map(|resource| {
            let Some(kind) = completeness_kind(resource) else {
                return Ok(CompletenessEvidence {
                    resource: resource_name(resource).to_owned(),
                    disposition: "pre_execution_error_only".to_owned(),
                    limiting_resource: None,
                    observed_not_below_limit: true,
                });
            };
            let completeness = ResultCompleteness::new(
                CompletenessState::Truncated,
                vec![LimitingResource {
                    kind,
                    limit: Some(1),
                    observed: Some(1),
                }],
                ContinuationAvailability::Unavailable,
                vec![ContinuationGuidance::NarrowScope],
            )?;
            Ok(CompletenessEvidence {
                resource: resource_name(resource).to_owned(),
                disposition: "truthful_truncation_when_partial_results_are_safe".to_owned(),
                limiting_resource: completeness
                    .limiting_resources
                    .first()
                    .map(|limiting| limiting_resource_name(limiting.kind).to_owned()),
                observed_not_below_limit: true,
            })
        })
        .collect()
}

fn source_proofs() -> Result<Vec<SourceProof>, BudgetConformanceError> {
    SOURCE_PROOFS
        .iter()
        .map(|declaration| {
            let path = workspace_root().join(declaration.file);
            let source = fs::read(&path).map_err(|source| BudgetConformanceError::Io {
                path: path.clone(),
                source,
            })?;
            let text = std::str::from_utf8(&source).map_err(|source| {
                BudgetConformanceError::SourceUtf8 {
                    path: path.clone(),
                    source,
                }
            })?;
            for symbol in declaration.symbols {
                if !text.contains(symbol) {
                    return Err(BudgetConformanceError::MissingProofSymbol {
                        path: declaration.file,
                        symbol,
                    });
                }
            }
            Ok(SourceProof {
                file: declaration.file.to_owned(),
                sha256: sha256_hex(&source),
                symbols: declaration
                    .symbols
                    .iter()
                    .map(|symbol| (*symbol).to_owned())
                    .collect(),
            })
        })
        .collect()
}

fn runtime_evidence() -> Vec<RuntimeEvidence> {
    vec![
        RuntimeEvidence {
            evidence_class: "source_bound_regression".to_owned(),
            behavior: "pending client-port work is dropped after cooperative cancellation"
                .to_owned(),
            proof_file: "apps/rootlight-mcp/src/executor/tests.rs".to_owned(),
            proof_symbol: "cancellation_drops_a_pending_client_port_future".to_owned(),
            measured_latency_us: None,
            work_after_cancel: "no new client-port result is published".to_owned(),
        },
        RuntimeEvidence {
            evidence_class: "source_bound_process_regression".to_owned(),
            behavior: "daemon cancellation leaves the work lane reusable before publication"
                .to_owned(),
            proof_file: "apps/rootlight-daemon/src/first_slice.rs".to_owned(),
            proof_symbol: "peer_cancellation_leaves_work_lane_reusable_before_publication"
                .to_owned(),
            measured_latency_us: None,
            work_after_cancel: "cancelled pre-publication work publishes no parent generation"
                .to_owned(),
        },
    ]
}

fn current_source_revision() -> Result<String, BudgetConformanceError> {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(workspace_root())
        .output()
        .map_err(BudgetConformanceError::Git)?;
    if !output.status.success() {
        return Err(BudgetConformanceError::GitStatus(output.status.code()));
    }
    let revision = String::from_utf8(output.stdout)
        .map_err(BudgetConformanceError::GitOutputUtf8)?
        .trim()
        .to_owned();
    validate_revision(&revision)?;
    Ok(revision)
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask is directly beneath the workspace root")
        .to_path_buf()
}

fn validate_revision(revision: &str) -> Result<(), BudgetConformanceError> {
    if matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(BudgetConformanceError::InvalidSourceRevision(
            revision.to_owned(),
        ))
    }
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, BudgetConformanceError> {
    let bytes = fs::read(path).map_err(|source| BudgetConformanceError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(BudgetConformanceError::ReportTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_REPORT_BYTES,
        });
    }
    Ok(bytes)
}

fn write_report(
    path: &Path,
    report: &BudgetConformanceReport,
) -> Result<(), BudgetConformanceError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| BudgetConformanceError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut encoded = serde_json::to_vec_pretty(report)?;
    encoded.push(b'\n');
    if encoded.len() > MAX_REPORT_BYTES {
        return Err(BudgetConformanceError::ReportTooLarge {
            path: path.to_path_buf(),
            maximum: MAX_REPORT_BYTES,
        });
    }
    fs::write(path, encoded).map_err(|source| BudgetConformanceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn validate_runtime_reports(
    runtime_path: &Path,
    cancellation_path: &Path,
    source_revision: &str,
) -> Result<RuntimeReportEvidence, BudgetConformanceError> {
    let runtime_bytes = read_bounded(runtime_path)?;
    let runtime: RuntimeBudgetReport = serde_json::from_slice(&runtime_bytes)?;
    if runtime.schema != RUNTIME_REPORT_SCHEMA || runtime.source_revision != source_revision {
        return Err(BudgetConformanceError::RuntimeReport(
            "runtime schema or exact source revision differs",
        ));
    }
    if runtime.fixture.name != "budget-runtime-repository-v1"
        || !valid_digest(&runtime.fixture.sha256)
        || runtime.fixture.regular_files == 0
        || runtime.process_boundary.daemon != "rootlight-daemon supervised stdio"
        || runtime.process_boundary.mcp != "rootlight-mcp JSON-RPC stdio"
        || runtime.process_boundary.indexed_generations != 2
        || runtime.tokenizer.name != "o200k_base"
        || runtime.tokenizer.implementation != "tiktoken-rs"
        || runtime.tokenizer.implementation_version != "0.12.0"
        || runtime.cases.len() != RUNTIME_COVERED_TOOLS.len()
    {
        return Err(BudgetConformanceError::RuntimeReport(
            "runtime metadata is incomplete or unexpected",
        ));
    }
    for (index, observation) in runtime.cases.iter().enumerate() {
        let expected_tool = RUNTIME_COVERED_TOOLS[index];
        let (expected_trigger, expected_code) = if expected_tool == "context.pack" {
            ("token_budget", "BUDGET_EXCEEDED")
        } else if expected_tool == "query.advanced" {
            ("cost_limit", "COST_LIMIT")
        } else {
            ("budget.max_tokens", "BUDGET_EXCEEDED")
        };
        if observation.tool != expected_tool
            || observation.trigger != expected_trigger
            || observation.limit == 0
            || observation.limited.error_code != expected_code
            || !valid_measurement(&observation.baseline)
            || !valid_measurement(&observation.limited.measurement())
        {
            return Err(BudgetConformanceError::RuntimeReport(
                "runtime tool observation failed conformance",
            ));
        }
    }

    let cancellation_bytes = read_bounded(cancellation_path)?;
    let cancellation: CancellationProcessReport = serde_json::from_slice(&cancellation_bytes)?;
    if cancellation.schema != CANCELLATION_REPORT_SCHEMA
        || cancellation.source_revision != source_revision
        || cancellation.process_boundary.daemon != "rootlight-daemon supervised stdio"
        || cancellation.process_boundary.mcp != "rootlight-mcp JSON-RPC stdio"
        || cancellation.cases.len() != 2
    {
        return Err(BudgetConformanceError::RuntimeReport(
            "cancellation metadata is incomplete or unexpected",
        ));
    }
    for (index, observation) in cancellation.cases.iter().enumerate() {
        let expected_tool = ["architecture.cycles", "query.advanced"][index];
        if observation.tool != expected_tool
            || observation.cancellation_reason != "client_request"
            || !observation.hook_entry_observed
            || !observation.cancellation_observed
            || observation.cancellation_latency_us > observation.cancellation_bound_us
            || observation.follow_up_latency_us > observation.follow_up_bound_us
            || observation.late_response_window_ms < 250
            || !observation.no_json_rpc_response_published
            || !observation.lane_reusable
            || observation.work_after_cancel != "none_observed"
        {
            return Err(BudgetConformanceError::RuntimeReport(
                "cancellation observation failed conformance",
            ));
        }
    }

    Ok(RuntimeReportEvidence {
        runtime_schema: runtime.schema,
        runtime_sha256: sha256_hex(&runtime_bytes),
        runtime_cases: runtime.cases.len(),
        cancellation_schema: cancellation.schema,
        cancellation_sha256: sha256_hex(&cancellation_bytes),
        cancellation_cases: cancellation.cases.len(),
        exact_source_revision: true,
    })
}

fn valid_measurement(measurement: &RuntimeMeasurement) -> bool {
    let Ok(bytes) = usize::try_from(measurement.serialized_json_bytes) else {
        return false;
    };
    measurement.serialized_json_bytes > 0
        && measurement.public_estimated_tokens == estimate_tokens(bytes)
        && measurement.actual_o200k_tokens > 0
        && valid_digest(&measurement.structured_sha256)
}

fn valid_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const fn charge_for(resource: BudgetResource, value: u64) -> BudgetCharge {
    let mut charge = BudgetCharge {
        rows: 0,
        results: 0,
        tokens: 0,
        actual_tokens: 0,
        source_bytes: 0,
        traversal_facts: 0,
        depth: 0,
        paths: 0,
        json_bytes: 0,
        memory_bytes: 0,
        time_ms: 0,
    };
    match resource {
        BudgetResource::Rows => charge.rows = value,
        BudgetResource::Results => charge.results = value,
        BudgetResource::Tokens => charge.tokens = value,
        BudgetResource::ActualTokens => charge.actual_tokens = value,
        BudgetResource::SourceBytes => charge.source_bytes = value,
        BudgetResource::TraversalFacts => charge.traversal_facts = value,
        BudgetResource::Depth => charge.depth = value,
        BudgetResource::Paths => charge.paths = value,
        BudgetResource::JsonBytes => charge.json_bytes = value,
        BudgetResource::MemoryBytes => charge.memory_bytes = value,
        BudgetResource::Time => charge.time_ms = value,
        _ => {}
    }
    charge
}

const fn uniform_charge(value: u64) -> BudgetCharge {
    BudgetCharge {
        rows: value,
        results: value,
        tokens: value,
        actual_tokens: value,
        source_bytes: value,
        traversal_facts: value,
        depth: value,
        paths: value,
        json_bytes: value,
        memory_bytes: value,
        time_ms: value,
    }
}

const fn charge_not_above(left: BudgetCharge, right: BudgetCharge) -> bool {
    left.rows <= right.rows
        && left.results <= right.results
        && left.tokens <= right.tokens
        && left.actual_tokens <= right.actual_tokens
        && left.source_bytes <= right.source_bytes
        && left.traversal_facts <= right.traversal_facts
        && left.depth <= right.depth
        && left.paths <= right.paths
        && left.json_bytes <= right.json_bytes
        && left.memory_bytes <= right.memory_bytes
        && left.time_ms <= right.time_ms
}

const fn resource_name(resource: BudgetResource) -> &'static str {
    match resource {
        BudgetResource::Rows => "rows",
        BudgetResource::Results => "results",
        BudgetResource::Tokens => "estimated_tokens",
        BudgetResource::ActualTokens => "actual_tokens",
        BudgetResource::SourceBytes => "source_bytes",
        BudgetResource::TraversalFacts => "traversal_facts",
        BudgetResource::Depth => "depth",
        BudgetResource::Paths => "paths",
        BudgetResource::JsonBytes => "response_bytes",
        BudgetResource::MemoryBytes => "memory_bytes",
        BudgetResource::Time => "wall_time",
        _ => "unknown",
    }
}

const fn completeness_kind(resource: BudgetResource) -> Option<LimitingResourceKind> {
    match resource {
        BudgetResource::Rows => Some(LimitingResourceKind::Rows),
        BudgetResource::Results => Some(LimitingResourceKind::Results),
        BudgetResource::Tokens => Some(LimitingResourceKind::EstimatedTokens),
        BudgetResource::ActualTokens => None,
        BudgetResource::SourceBytes => Some(LimitingResourceKind::SourceBytes),
        BudgetResource::TraversalFacts => Some(LimitingResourceKind::Edges),
        BudgetResource::Depth => Some(LimitingResourceKind::Depth),
        BudgetResource::Paths => Some(LimitingResourceKind::Paths),
        BudgetResource::JsonBytes => Some(LimitingResourceKind::ResponseBytes),
        BudgetResource::MemoryBytes => Some(LimitingResourceKind::MemoryBytes),
        BudgetResource::Time => Some(LimitingResourceKind::Deadline),
        _ => None,
    }
}

const fn limiting_resource_name(resource: LimitingResourceKind) -> &'static str {
    match resource {
        LimitingResourceKind::Rows => "rows",
        LimitingResourceKind::Edges => "edges",
        LimitingResourceKind::Results => "results",
        LimitingResourceKind::Depth => "depth",
        LimitingResourceKind::Paths => "paths",
        LimitingResourceKind::SourceBytes => "source_bytes",
        LimitingResourceKind::ResponseBytes => "response_bytes",
        LimitingResourceKind::MemoryBytes => "memory_bytes",
        LimitingResourceKind::Deadline => "deadline",
        LimitingResourceKind::EstimatedTokens => "estimated_tokens",
        LimitingResourceKind::Cancellation => "cancellation",
        LimitingResourceKind::Capability => "capability",
        LimitingResourceKind::Coverage => "coverage",
        LimitingResourceKind::PageSize => "page_size",
    }
}

fn sha256_hex(input: &[u8]) -> String {
    use std::fmt::Write as _;

    let digest = Sha256::digest(input);
    let mut encoded = String::with_capacity(64);
    for byte in digest {
        write!(encoded, "{byte:02x}").expect("writing to a string cannot fail");
    }
    encoded
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BudgetConformanceReport {
    schema: String,
    source_revision: String,
    tools: Vec<ToolBudgetInventory>,
    ledger_boundaries: Vec<LedgerBoundaryEvidence>,
    effective_limits: EffectiveLimitEvidence,
    completeness: Vec<CompletenessEvidence>,
    source_proofs: Vec<SourceProof>,
    runtime_evidence: Vec<RuntimeEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_reports: Option<RuntimeReportEvidence>,
    token_accounting: TokenAccountingEvidence,
    residual_approximations: Vec<String>,
}

#[derive(Serialize)]
struct AccountingInput<'a> {
    schema: &'static str,
    source_revision: &'a str,
    tools: &'a [ToolBudgetInventory],
    ledger_boundaries: &'a [LedgerBoundaryEvidence],
    effective_limits: &'a EffectiveLimitEvidence,
    completeness: &'a [CompletenessEvidence],
    source_proofs: &'a [SourceProof],
    runtime_evidence: &'a [RuntimeEvidence],
    #[serde(skip_serializing_if = "Option::is_none")]
    runtime_reports: Option<&'a RuntimeReportEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolBudgetInventory {
    tool: String,
    budget_semantics: String,
    default_token_budget: u16,
    batch_shared_budget: bool,
    declared_budget_dimensions: Vec<String>,
    runtime_trigger_coverage: String,
    omitted_client_budget_disposition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerBoundaryEvidence {
    resource: String,
    below_accepted: bool,
    exact_accepted: bool,
    one_above_rejected: bool,
    zero_limit_rejects_work: bool,
    stable_error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EffectiveLimitEvidence {
    omitted_budget_uses_complete_server_ceiling: bool,
    client_reduces_all_accepted_dimensions: bool,
    client_cannot_raise_server_ceiling: bool,
    child_never_exceeds_parent_or_tool: bool,
    reservation_is_checked_before_work: bool,
    measured_commit_cannot_exceed_reservation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompletenessEvidence {
    resource: String,
    disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limiting_resource: Option<String>,
    observed_not_below_limit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceProof {
    file: String,
    sha256: String,
    symbols: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeEvidence {
    evidence_class: String,
    behavior: String,
    proof_file: String,
    proof_symbol: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    measured_latency_us: Option<u64>,
    work_after_cancel: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeReportEvidence {
    runtime_schema: String,
    runtime_sha256: String,
    runtime_cases: usize,
    cancellation_schema: String,
    cancellation_sha256: String,
    cancellation_cases: usize,
    exact_source_revision: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeBudgetReport {
    schema: String,
    source_revision: String,
    fixture: RuntimeFixture,
    process_boundary: RuntimeProcessBoundary,
    tokenizer: RuntimeTokenizer,
    cases: Vec<RuntimeToolObservation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFixture {
    name: String,
    sha256: String,
    regular_files: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeProcessBoundary {
    daemon: String,
    mcp: String,
    indexed_generations: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTokenizer {
    name: String,
    implementation: String,
    implementation_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeToolObservation {
    tool: String,
    trigger: String,
    limit: u64,
    baseline: RuntimeMeasurement,
    limited: RuntimeLimitMeasurement,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeMeasurement {
    serialized_json_bytes: u64,
    public_estimated_tokens: u64,
    actual_o200k_tokens: u64,
    structured_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeLimitMeasurement {
    error_code: String,
    serialized_json_bytes: u64,
    public_estimated_tokens: u64,
    actual_o200k_tokens: u64,
    structured_sha256: String,
}

impl RuntimeLimitMeasurement {
    fn measurement(&self) -> RuntimeMeasurement {
        RuntimeMeasurement {
            serialized_json_bytes: self.serialized_json_bytes,
            public_estimated_tokens: self.public_estimated_tokens,
            actual_o200k_tokens: self.actual_o200k_tokens,
            structured_sha256: self.structured_sha256.clone(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancellationProcessReport {
    schema: String,
    source_revision: String,
    process_boundary: CancellationProcessBoundary,
    cases: Vec<CancellationObservation>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancellationProcessBoundary {
    daemon: String,
    mcp: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CancellationObservation {
    tool: String,
    cancellation_reason: String,
    hook_entry_observed: bool,
    cancellation_observed: bool,
    cancellation_latency_us: u64,
    cancellation_bound_us: u64,
    follow_up_latency_us: u64,
    follow_up_bound_us: u64,
    late_response_window_ms: u64,
    no_json_rpc_response_published: bool,
    lane_reusable: bool,
    work_after_cancel: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TokenAccountingEvidence {
    tokenizer: ActualTokenizerIdentity,
    input_schema: String,
    input_sha256: String,
    serialized_bytes: u64,
    deterministic_estimated_tokens: u64,
    actual_tokens: u64,
    normalization: String,
    framing: String,
}

#[derive(Clone, Copy)]
struct SourceProofDeclaration {
    file: &'static str,
    symbols: &'static [&'static str],
}

/// Failure while deriving or checking offline budget evidence.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BudgetConformanceError {
    /// A command option is missing its value.
    #[error("missing value for {0}")]
    MissingArgument(&'static str),
    /// An unknown or duplicate option was supplied.
    #[error("unexpected budget-conformance argument: {0}")]
    UnexpectedArgument(String),
    /// Runtime reports and their merged output must be supplied as one unit.
    #[error("--runtime-report, --cancellation-report, and --output are required together")]
    RuntimeArgumentsRequiredTogether,
    /// Offline fixture refresh cannot consume runtime evidence.
    #[error("--refresh cannot be combined with runtime evidence")]
    RefreshWithRuntimeEvidence,
    /// A real-process report did not satisfy its strict evidence contract.
    #[error("runtime budget evidence is invalid: {0}")]
    RuntimeReport(&'static str),
    /// The canonical capability registry has an unexpected length.
    #[error("capability count differs: expected {expected}, observed {observed}")]
    CapabilityCount {
        /// Required catalog length.
        expected: usize,
        /// Observed registry length.
        observed: usize,
    },
    /// Registry order differs from the canonical catalog.
    #[error("capability registry order differs at {0}")]
    CapabilityOrder(&'static str),
    /// One executable ledger boundary did not hold.
    #[error("ledger boundary conformance failed for {0}")]
    LedgerBoundary(&'static str),
    /// Parent, client, and server effective limits did not reconcile.
    #[error("effective budget limits did not reconcile")]
    EffectiveLimits,
    /// A retained report differs from canonical current inputs.
    #[error("budget-conformance report drifted: expected {expected}, observed {observed}")]
    ReportDrift {
        /// Retained report digest.
        expected: String,
        /// Rebuilt report digest.
        observed: String,
    },
    /// A retained proof symbol no longer exists in its source.
    #[error("source-bound proof {symbol} is absent from {path}")]
    MissingProofSymbol {
        /// Workspace-relative source path.
        path: &'static str,
        /// Required test or helper symbol.
        symbol: &'static str,
    },
    /// A source revision is not a canonical lowercase object identifier.
    #[error("invalid source revision: {0}")]
    InvalidSourceRevision(String),
    /// The report exceeds its fixed offline read ceiling.
    #[error("report {path} exceeds {maximum} bytes")]
    ReportTooLarge {
        /// Report path.
        path: PathBuf,
        /// Maximum accepted bytes.
        maximum: usize,
    },
    /// A filesystem operation failed.
    #[error("filesystem operation failed for {path}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// A proof source was not UTF-8.
    #[error("source-bound proof is not UTF-8: {path}")]
    SourceUtf8 {
        /// Affected source path.
        path: PathBuf,
        /// Decoding failure.
        #[source]
        source: std::str::Utf8Error,
    },
    /// The canonical accounting input unexpectedly failed UTF-8 decoding.
    #[error("canonical accounting input is not UTF-8")]
    AccountingInputUtf8(#[source] std::str::Utf8Error),
    /// Git could not be started.
    #[error("failed to invoke git")]
    Git(#[source] std::io::Error),
    /// Git returned a failing status.
    #[error("git rev-parse failed with status {0:?}")]
    GitStatus(Option<i32>),
    /// Git emitted non-UTF-8 revision text.
    #[error("git revision output is not UTF-8")]
    GitOutputUtf8(#[source] std::string::FromUtf8Error),
    /// A checked conversion overflowed.
    #[error("budget-conformance integer overflow")]
    IntegerOverflow,
    /// JSON encoding or decoding failed.
    #[error("budget-conformance JSON failure")]
    Json(#[from] serde_json::Error),
    /// The shared budget ledger rejected an evidence setup.
    #[error(transparent)]
    Policy(#[from] ExecutionPolicyError),
    /// Completeness semantics rejected an evidence setup.
    #[error(transparent)]
    Completeness(#[from] rootlight_mcp_contract::completeness::CompletenessError),
    /// The pinned offline tokenizer failed.
    #[error(transparent)]
    Tokenizer(#[from] crate::token_accounting::TokenAccountingError),
}

#[cfg(test)]
mod tests {
    use super::{BudgetConformanceError, Options, REPORT_FILE, check};

    #[test]
    fn options_default_to_check_and_require_explicit_refresh() {
        let defaults = Options::parse(&mut std::iter::empty())
            .expect("default budget-conformance options parse");
        assert!(!defaults.refresh);
        assert!(defaults.runtime_report.is_none());

        let refreshed = Options::parse(
            &mut ["--fixture-root", "target/budget-fixture", "--refresh"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("refresh options parse");
        assert!(refreshed.refresh);
        assert_eq!(
            refreshed.fixture_root,
            std::path::PathBuf::from("target/budget-fixture")
        );
        assert!(matches!(
            Options::parse(&mut ["--refresh", "--refresh"].into_iter().map(str::to_owned)),
            Err(BudgetConformanceError::UnexpectedArgument(_))
        ));

        let runtime = Options::parse(
            &mut [
                "--runtime-report",
                "target/runtime.json",
                "--cancellation-report",
                "target/cancellation.json",
                "--output",
                "target/conformance.json",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("complete runtime evidence options parse");
        assert_eq!(
            runtime.runtime_report,
            Some(std::path::PathBuf::from("target/runtime.json"))
        );
        assert!(matches!(
            Options::parse(
                &mut ["--runtime-report", "target/runtime.json"]
                    .into_iter()
                    .map(str::to_owned)
            ),
            Err(BudgetConformanceError::RuntimeArgumentsRequiredTogether)
        ));
    }

    #[test]
    fn refreshed_report_passes_the_offline_default_gate() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let refresh = Options {
            fixture_root: directory.path().to_path_buf(),
            refresh: true,
            runtime_report: None,
            cancellation_report: None,
            output: None,
        };
        check(&refresh).expect("budget-conformance report refreshes");
        assert!(directory.path().join(REPORT_FILE).is_file());

        let verify = Options {
            fixture_root: directory.path().to_path_buf(),
            refresh: false,
            runtime_report: None,
            cancellation_report: None,
            output: None,
        };
        check(&verify).expect("fresh report passes the default gate");
    }

    #[test]
    fn modified_report_is_rejected() {
        let directory = tempfile::tempdir().expect("temporary directory creates");
        let refresh = Options {
            fixture_root: directory.path().to_path_buf(),
            refresh: true,
            runtime_report: None,
            cancellation_report: None,
            output: None,
        };
        check(&refresh).expect("budget-conformance report refreshes");
        let path = directory.path().join(REPORT_FILE);
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("report reads"))
                .expect("report parses");
        value["tools"][0]["budget_semantics"] = serde_json::json!("unbounded");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&value).expect("mutated report serializes"),
        )
        .expect("mutated report writes");

        let verify = Options {
            fixture_root: directory.path().to_path_buf(),
            refresh: false,
            runtime_report: None,
            cancellation_report: None,
            output: None,
        };
        assert!(matches!(
            check(&verify),
            Err(BudgetConformanceError::ReportDrift { .. })
        ));
    }
}
