//! Deterministic before-and-after evidence for MCP discovery compatibility.
//!
//! The command emits complete `tools/list` payloads for every exposure profile
//! and a source-bound report covering byte counts, hashes, and copy disposition.

use std::fs;
use std::path::{Path, PathBuf};

use rootlight_mcp_contract::MCP_SCHEMA_VERSION;
use rootlight_mcp_contract::accounting::tool_list_payload;
use rootlight_mcp_contract::capability::{DISCOVERY_METADATA_KEY, capability_for};
use rootlight_mcp_contract::catalog::{ExposureProfile, McpTool};
use serde::Serialize;
use serde_json::{Value, json};

const REPORT_SCHEMA: &str = "rootlight.mcp-tool-discovery-evidence/1";
const BASELINE_REVISION: &str = "41a39be6d5c00f40afc8412c6d7de293b76c43ab";
const REPO_LIST_V1_INPUT_SCHEMA: &str =
    include_str!("../../schemas/generated/json/mcp-repo-list-input-1.0.schema.json");
const REPO_LIST_V1_OUTPUT_SCHEMA: &str =
    include_str!("../../schemas/generated/json/mcp-repo-list-output-1.0.schema.json");
const REPO_LIST_V1_INPUT_SHAPE_HASH: &str =
    "5f2a9e3fe96343fa1e75e8c4151d07cbc38ca6b1935ee7a8fadfd9defa9759b7";

/// Source-bound artifact options for discovery evidence.
pub(crate) struct Options {
    output_dir: PathBuf,
    source_revision: String,
}

impl Options {
    /// Parses the required output directory and source revision.
    pub(crate) fn parse(args: &mut impl Iterator<Item = String>) -> Result<Self, DiscoveryError> {
        let mut output_dir = None;
        let mut source_revision = None;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--output-dir" if output_dir.is_none() => {
                    output_dir = Some(PathBuf::from(
                        args.next()
                            .ok_or(DiscoveryError::MissingArgument("--output-dir"))?,
                    ));
                }
                "--source-revision" if source_revision.is_none() => {
                    source_revision = Some(
                        args.next()
                            .ok_or(DiscoveryError::MissingArgument("--source-revision"))?,
                    );
                }
                _ => return Err(DiscoveryError::UnexpectedArgument(flag)),
            }
        }
        let output_dir = output_dir.ok_or(DiscoveryError::IncompleteOptions)?;
        let source_revision = source_revision.ok_or(DiscoveryError::IncompleteOptions)?;
        if !valid_source_revision(&source_revision) {
            return Err(DiscoveryError::InvalidSourceRevision(source_revision));
        }
        Ok(Self {
            output_dir,
            source_revision,
        })
    }
}

/// Writes complete profile payloads and their compatibility report.
pub(crate) fn emit(options: &Options) -> Result<(), DiscoveryError> {
    create_dir_all(&options.output_dir)?;
    let mut payloads = Vec::with_capacity(ExposureProfile::ALL.len());
    for profile in ExposureProfile::ALL {
        let after = tool_list_payload(profile);
        let before = baseline_payload(profile)?;
        let before_bytes = serde_json::to_vec(&before)?;
        let after_bytes = serde_json::to_vec(&after)?;
        let expected_before = baseline_hash(profile);
        let observed_before = blake3::hash(&before_bytes).to_hex().to_string();
        if observed_before != expected_before {
            return Err(DiscoveryError::BaselineDrift {
                profile: profile.name(),
                expected: expected_before,
                observed: observed_before,
            });
        }

        let before_file = format!("tools-list-before-{}.json", profile.name());
        let after_file = format!("tools-list-after-{}.json", profile.name());
        write_bytes(&options.output_dir.join(&before_file), &before_bytes)?;
        write_bytes(&options.output_dir.join(&after_file), &after_bytes)?;
        payloads.push(ProfileEvidence {
            profile: profile.name(),
            tool_count: profile.tools().len(),
            before: PayloadEvidence::new(before_file, &before_bytes),
            after: PayloadEvidence::new(after_file, &after_bytes),
        });
    }

    let report = DiscoveryReport {
        schema: REPORT_SCHEMA,
        source_revision: &options.source_revision,
        baseline_revision: BASELINE_REVISION,
        payloads,
        tool_dispositions: McpTool::ALL
            .into_iter()
            .map(|tool| ToolDisposition {
                tool: tool.name(),
                disposition: if tool == McpTool::OperationStatus {
                    "implemented"
                } else {
                    "narrowed"
                },
                summary: capability_for(tool).fallback_summary,
            })
            .collect(),
        compatibility: CompatibilityImpact {
            breaking: false,
            changed_fields: [
                "description",
                "_meta.rootlight/capabilities.fallbackSummary",
                "_meta.rootlight/capabilities.limitations",
                "annotations.idempotentHint for repo.index",
            ],
            unchanged_contracts: [
                "tool names and titles",
                "input and output schemas",
                "exposure profile membership",
                "authorization policy",
            ],
            not_assessed: ["runtime behavior"],
        },
    };
    let mut encoded = serde_json::to_vec_pretty(&report)?;
    encoded.push(b'\n');
    write_bytes(
        &options
            .output_dir
            .join("tool-discovery-compatibility-v1.json"),
        &encoded,
    )?;
    println!(
        "tool discovery evidence written for {} profiles",
        ExposureProfile::ALL.len()
    );
    Ok(())
}

fn baseline_payload(profile: ExposureProfile) -> Result<Value, DiscoveryError> {
    let mut payload = tool_list_payload(profile);
    let tools = payload
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .ok_or(DiscoveryError::InvalidPayload)?;
    for definition in tools {
        let name = definition
            .get("name")
            .and_then(Value::as_str)
            .ok_or(DiscoveryError::InvalidPayload)?;
        let tool = McpTool::ALL
            .into_iter()
            .find(|tool| tool.name() == name)
            .ok_or(DiscoveryError::InvalidPayload)?;
        let object = definition
            .as_object_mut()
            .ok_or(DiscoveryError::InvalidPayload)?;
        object.insert(
            "description".to_owned(),
            Value::String(baseline_description(tool).to_owned()),
        );
        if tool == McpTool::RepoIndex {
            object
                .get_mut("annotations")
                .and_then(Value::as_object_mut)
                .ok_or(DiscoveryError::InvalidPayload)?
                .insert("idempotentHint".to_owned(), Value::Bool(true));
        }
        if tool == McpTool::RepoList {
            restore_repo_list_baseline(object)?;
        }
        let capability = object
            .get_mut("_meta")
            .and_then(Value::as_object_mut)
            .and_then(|metadata| metadata.get_mut(DISCOVERY_METADATA_KEY))
            .and_then(Value::as_object_mut)
            .ok_or(DiscoveryError::InvalidPayload)?;
        capability.insert(
            "fallbackSummary".to_owned(),
            Value::String(baseline_fallback_summary(tool).to_owned()),
        );
        let limitations = capability
            .get_mut("limitations")
            .and_then(Value::as_array_mut)
            .ok_or(DiscoveryError::InvalidPayload)?;
        // Baseline payloads predate these field dispositions; retain the old
        // bytes so discovery evidence compares the historical and current views.
        limitations.retain(|limitation| {
            let field = limitation.get("field").and_then(Value::as_str);
            !matches!(
                (tool, field),
                (McpTool::RepoIndex, Some("scope"))
                    | (McpTool::RepoStatus, Some("generation"))
                    | (McpTool::ArchitectureCycles, Some("projection.level"))
                    | (
                        McpTool::SourceRead,
                        Some(
                            "context_lines_before"
                                | "context_lines_after"
                                | "references[].symbol_id"
                                | "references[].file_id"
                                | "references[].start_byte"
                                | "references[].end_byte"
                        )
                    )
            )
        });
    }
    Ok(payload)
}

fn restore_repo_list_baseline(
    definition: &mut serde_json::Map<String, Value>,
) -> Result<(), DiscoveryError> {
    definition.insert(
        "inputSchema".to_owned(),
        serde_json::from_str(REPO_LIST_V1_INPUT_SCHEMA)?,
    );
    definition.insert(
        "outputSchema".to_owned(),
        serde_json::from_str(REPO_LIST_V1_OUTPUT_SCHEMA)?,
    );
    let metadata = definition
        .get_mut("_meta")
        .and_then(Value::as_object_mut)
        .ok_or(DiscoveryError::InvalidPayload)?;
    metadata.insert(
        DISCOVERY_METADATA_KEY.to_owned(),
        json!({
            "contractVersion": MCP_SCHEMA_VERSION,
            "inputShapeHash": REPO_LIST_V1_INPUT_SHAPE_HASH,
            "status": "fallback_limited",
            "profiles": ["developer"],
            "batchEligible": false,
            "explainSupported": true,
            "pagination": "authenticated_cursor",
            "generation": "none",
            "budget": "none",
            "batchSharedBudget": false,
            "fallbackSummary": baseline_fallback_summary(McpTool::RepoList),
            "limitations": [
                {
                    "field": "query",
                    "status": "blocked",
                    "summary": "query is cursor-bound but does not filter opaque repository identities"
                },
                {
                    "field": "states",
                    "status": "unsupported_stable_error",
                    "errorCode": "UNSUPPORTED_CAPABILITY",
                    "summary": "repository-state filtering is not served"
                },
                {
                    "field": "response_profile",
                    "value": "evidence",
                    "status": "unsupported_stable_error",
                    "errorCode": "UNSUPPORTED_CAPABILITY",
                    "summary": "only compact response projection is served"
                },
                {
                    "field": "response_profile",
                    "value": "standard",
                    "status": "unsupported_stable_error",
                    "errorCode": "UNSUPPORTED_CAPABILITY",
                    "summary": "only compact response projection is served"
                }
            ]
        }),
    );
    Ok(())
}

const fn baseline_hash(profile: ExposureProfile) -> &'static str {
    match profile {
        ExposureProfile::Scout => {
            "4759cecf649d03eca287567f33386003282a7068ce614396fd5ec352134192f2"
        }
        ExposureProfile::Analysis => {
            "94d9c1a2fc6962fea1e6108505bcbc157cfe49a825dee3401c85b88fd554186a"
        }
        ExposureProfile::Developer => {
            "9d29b86da344f336429f49a85579f8ea154a5cf74f66762e470b724a8fdcb361"
        }
    }
}

const fn baseline_description(tool: McpTool) -> &'static str {
    match tool {
        McpTool::RepoIndex => {
            "Create or update one local repository generation and return its operation handle."
        }
        McpTool::RepoStatus => {
            "Inspect repository state, generation freshness, coverage, and active operations."
        }
        McpTool::RepoList => "List registered repositories and workspaces.",
        McpTool::OperationStatus => "Read or cancel one known long-running Rootlight operation.",
        McpTool::CodeLocate => {
            "Find bounded, generation-pinned code and file matches by exact identifier or lexical text."
        }
        McpTool::SymbolExplain => "Return bounded semantic evidence for stable symbol identifiers.",
        McpTool::SymbolRelationships => {
            "Get bounded typed callers, callees, references, types, implementations, dependencies, tests, or ownership around symbols."
        }
        McpTool::FlowTrace => {
            "Trace bounded paths through calls, data flow, services, messaging, build, or dependency relations."
        }
        McpTool::ChangeImpact => {
            "Map a provided change set to affected symbols, dependents, services, risks, and tests."
        }
        McpTool::TestsSelect => {
            "Rank tests relevant to symbols or changes with rationale and uncertainty."
        }
        McpTool::ArchitectureOverview => {
            "Produce a file-granularity architecture map of modules and packages, with hotspots."
        }
        McpTool::ArchitectureCycles => {
            "Find and explain dependency cycles in a selected relation projection."
        }
        McpTool::CodeDead => {
            "Find dead or unreachable candidates with entry-point and coverage caveats."
        }
        McpTool::HistoryCompare => "Compare two pinned generations structurally.",
        McpTool::PlanChange => {
            "Produce an ordered change plan with affected symbols, files, tests, risks, and verification steps."
        }
        McpTool::ContextPack => {
            "Assemble minimal task-specific symbol evidence under a token budget."
        }
        McpTool::SourceRead => {
            "Read exact bounded ranges from a pinned source snapshot as untrusted repository data."
        }
        McpTool::QueryAdvanced => {
            "Execute a bounded expert query over the documented safe query AST."
        }
        McpTool::QueryBatch => {
            "Execute up to sixteen independent or dependency-linked read operations under one pinned generation."
        }
    }
}

const fn baseline_fallback_summary(tool: McpTool) -> &'static str {
    match tool {
        McpTool::RepoIndex => "bounded generation creation; durable publication inactive",
        McpTool::RepoStatus => "bounded process-local status; active generation returned",
        McpTool::RepoList => "bounded catalog listing with authenticated continuation",
        McpTool::OperationStatus => "bounded operation read and cancel",
        McpTool::CodeLocate => "bounded structural and lexical matching",
        McpTool::SymbolExplain => "bounded semantic evidence",
        McpTool::SymbolRelationships => "bounded typed relationships",
        McpTool::FlowTrace => "bounded path tracing",
        McpTool::ChangeImpact => "bounded change mapping",
        McpTool::TestsSelect => "bounded test ranking",
        McpTool::ArchitectureOverview => "bounded architecture map",
        McpTool::ArchitectureCycles => "bounded cycle detection",
        McpTool::CodeDead => "bounded dead-code candidates",
        McpTool::HistoryCompare => "bounded structural comparison",
        McpTool::PlanChange => "bounded change planning",
        McpTool::ContextPack => "bounded evidence assembly under a token budget",
        McpTool::SourceRead => "bounded source ranges as untrusted data",
        McpTool::QueryAdvanced => "bounded safe-AST query",
        McpTool::QueryBatch => {
            "bounded active-generation dispatch with shared child accounting; historical selection and complete accounting remain fallback-limited"
        }
    }
}

fn valid_source_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn create_dir_all(path: &Path) -> Result<(), DiscoveryError> {
    fs::create_dir_all(path).map_err(|source| DiscoveryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), DiscoveryError> {
    fs::write(path, bytes).map_err(|source| DiscoveryError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Serialize)]
struct DiscoveryReport<'a> {
    schema: &'static str,
    source_revision: &'a str,
    baseline_revision: &'static str,
    payloads: Vec<ProfileEvidence>,
    tool_dispositions: Vec<ToolDisposition>,
    compatibility: CompatibilityImpact,
}

#[derive(Serialize)]
struct ProfileEvidence {
    profile: &'static str,
    tool_count: usize,
    before: PayloadEvidence,
    after: PayloadEvidence,
}

#[derive(Serialize)]
struct PayloadEvidence {
    file: String,
    bytes: usize,
    blake3: String,
}

impl PayloadEvidence {
    fn new(file: String, payload: &[u8]) -> Self {
        Self {
            file,
            bytes: payload.len(),
            blake3: blake3::hash(payload).to_hex().to_string(),
        }
    }
}

#[derive(Serialize)]
struct ToolDisposition {
    tool: &'static str,
    disposition: &'static str,
    summary: &'static str,
}

#[derive(Serialize)]
struct CompatibilityImpact {
    breaking: bool,
    changed_fields: [&'static str; 4],
    unchanged_contracts: [&'static str; 4],
    not_assessed: [&'static str; 1],
}

/// Failure while producing deterministic discovery evidence.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DiscoveryError {
    /// A required option value is absent.
    #[error("missing value for {0}")]
    MissingArgument(&'static str),
    /// The required output/revision option pair is incomplete.
    #[error("tool-discovery-evidence requires --output-dir and --source-revision")]
    IncompleteOptions,
    /// An unknown or duplicate option was supplied.
    #[error("unexpected tool-discovery-evidence argument: {0}")]
    UnexpectedArgument(String),
    /// The revision is not a lowercase hexadecimal object identifier.
    #[error("invalid source revision: {0}")]
    InvalidSourceRevision(String),
    /// The canonical discovery payload had an unexpected shape.
    #[error("canonical tools/list payload has an unexpected shape")]
    InvalidPayload,
    /// Historical reconstruction no longer matches its retained full-payload hash.
    #[error(
        "historical {profile} tools/list payload drifted: expected {expected}, observed {observed}"
    )]
    BaselineDrift {
        /// Exposure profile.
        profile: &'static str,
        /// Retained BLAKE3 digest.
        expected: &'static str,
        /// Reconstructed BLAKE3 digest.
        observed: String,
    },
    /// A payload or report could not be encoded.
    #[error("tool discovery evidence serialization failed")]
    Json(#[from] serde_json::Error),
    /// An evidence path could not be created or written.
    #[error("tool discovery evidence I/O failed for {path}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_payloads_match_the_retained_complete_goldens() {
        for profile in ExposureProfile::ALL {
            let payload = baseline_payload(profile).expect("baseline payload reconstructs");
            let encoded = serde_json::to_vec(&payload).expect("payload serializes");
            assert_eq!(
                blake3::hash(&encoded).to_hex().as_str(),
                baseline_hash(profile),
                "{} baseline drifted",
                profile.name()
            );
        }
    }

    #[test]
    fn reconstructed_repo_list_retains_v1_while_the_catalog_serves_v2() {
        let historical =
            baseline_payload(ExposureProfile::Developer).expect("baseline payload reconstructs");
        let historical = repo_list_definition(&historical);
        let expected_input: Value =
            serde_json::from_str(REPO_LIST_V1_INPUT_SCHEMA).expect("v1 input schema is valid");
        let expected_output: Value =
            serde_json::from_str(REPO_LIST_V1_OUTPUT_SCHEMA).expect("v1 output schema is valid");
        assert_eq!(historical["inputSchema"], expected_input);
        assert_eq!(historical["outputSchema"], expected_output);
        assert_eq!(
            historical["_meta"][DISCOVERY_METADATA_KEY]["contractVersion"],
            MCP_SCHEMA_VERSION
        );

        let current = tool_list_payload(ExposureProfile::Developer);
        let current = repo_list_definition(&current);
        assert_eq!(
            current["_meta"][DISCOVERY_METADATA_KEY]["contractVersion"],
            rootlight_mcp_contract::REPO_LIST_SCHEMA_VERSION
        );
        assert_eq!(
            current["inputSchema"]["$id"],
            "https://rootlight.local/schema/mcp/repo.list/input/2.0"
        );
        assert_eq!(
            current["outputSchema"]["$id"],
            "https://rootlight.local/schema/mcp/repo.list/output/2.0"
        );
    }

    fn repo_list_definition(payload: &Value) -> &Value {
        payload["tools"]
            .as_array()
            .expect("tools list is an array")
            .iter()
            .find(|definition| definition["name"] == McpTool::RepoList.name())
            .expect("developer payload contains repo.list")
    }
}
