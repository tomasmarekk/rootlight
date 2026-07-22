//! Deterministic before-and-after evidence for MCP discovery compatibility.
//!
//! The command emits complete `tools/list` payloads for every exposure profile
//! and a source-bound report covering byte counts, hashes, and copy disposition.

use std::fs;
use std::path::{Path, PathBuf};

use rootlight_mcp_contract::ErrorCode;
use rootlight_mcp_contract::accounting::tool_list_payload;
use rootlight_mcp_contract::capability::{
    DISCOVERY_METADATA_KEY, DiscoveryCapabilityLimit, capability_for,
};
use rootlight_mcp_contract::catalog::{ExposureProfile, McpTool};
use serde::Serialize;
use serde_json::Value;

const REPORT_SCHEMA: &str = "rootlight.mcp-tool-discovery-evidence/1";
const BASELINE_REVISION: &str = "ada4892d2f173249d495286bdde4d62afdea92f4";

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
                "_meta.rootlight/capabilities.responseProfiles",
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
        if let Some(description) = pre_profile_description(tool) {
            object.insert(
                "description".to_owned(),
                Value::String(description.to_owned()),
            );
        }
        let capability = object
            .get_mut("_meta")
            .and_then(Value::as_object_mut)
            .and_then(|metadata| metadata.get_mut(DISCOVERY_METADATA_KEY))
            .and_then(Value::as_object_mut)
            .ok_or(DiscoveryError::InvalidPayload)?;
        // The profile baseline predates positive response-representation
        // discovery and admits only the compact selector values restored below.
        capability.remove("responseProfiles");
        if tool == McpTool::SymbolExplain {
            capability.insert(
                "fallbackSummary".to_owned(),
                Value::String(
                    "bounded compact semantic evidence for explicit stable symbol identifiers"
                        .to_owned(),
                ),
            );
        }
        let limitations = capability
            .get_mut("limitations")
            .and_then(Value::as_array_mut)
            .ok_or(DiscoveryError::InvalidPayload)?;
        if tool == McpTool::HistoryCompare {
            for limitation in &mut *limitations {
                if limitation["field"] == "profile"
                    && matches!(limitation["value"].as_str(), Some("evidence" | "standard"))
                {
                    limitation["summary"] =
                        Value::String("only compact response projection is served".to_owned());
                }
            }
        }
        restore_compact_only_profile_limitations(tool, limitations)?;
    }
    Ok(payload)
}

fn restore_compact_only_profile_limitations(
    tool: McpTool,
    limitations: &mut Vec<Value>,
) -> Result<(), DiscoveryError> {
    let field = match tool {
        McpTool::CodeLocate
        | McpTool::SymbolExplain
        | McpTool::SymbolRelationships
        | McpTool::FlowTrace
        | McpTool::ArchitectureOverview
        | McpTool::ArchitectureCycles
        | McpTool::CodeDead => "response_profile",
        McpTool::ChangeImpact | McpTool::TestsSelect | McpTool::PlanChange => "profile",
        _ => return Ok(()),
    };
    for value in ["evidence", "standard"] {
        limitations.push(serde_json::to_value(DiscoveryCapabilityLimit {
            field,
            value: Some(value),
            status: "unsupported_stable_error",
            error_code: Some(ErrorCode::UnsupportedCapability),
            summary: "only compact response projection is served",
        })?);
    }
    Ok(())
}

const fn baseline_hash(profile: ExposureProfile) -> &'static str {
    match profile {
        ExposureProfile::Scout => {
            "3d633ccfced99a2e2cdaa80cce1133f13af37f4d1eec32cd4179495b48e383a6"
        }
        ExposureProfile::Analysis => {
            "e4122bdc0725bc3f94704a2186087c56d305033bbd6baa18c929d46eff2d6008"
        }
        ExposureProfile::Developer => {
            "d131ad767f752d065f626c1c00292ddd61f4e662e2c4891a3602cb12603bc924"
        }
    }
}

const fn pre_profile_description(tool: McpTool) -> Option<&'static str> {
    match tool {
        McpTool::CodeLocate => Some(
            "Use bounded exact-identifier and lexical matching in one selected generation; path, structural, semantic, documentation, and continuation modes are unsupported.",
        ),
        McpTool::SymbolExplain => Some(
            "Return bounded compact semantic evidence for explicit stable symbol identifiers; custom sections and full provenance are unsupported.",
        ),
        McpTool::SymbolRelationships => Some(
            "Return bounded typed relationships around explicit stable symbol identifiers; custom scope, candidate projection, and continuation are unsupported.",
        ),
        McpTool::ArchitectureCycles => Some(
            "Use bounded cycle detection in a selected relation projection; custom scope, ranking, budgets, and expanded profiles are unsupported.",
        ),
        McpTool::CodeDead => Some(
            "Return bounded dead-code candidates with entry-point and blind-spot caveats; custom scope, budgets, and expanded profiles are unsupported.",
        ),
        McpTool::PlanChange => Some(
            "Use bounded change planning from an explicit objective and targets; change-context resolution, user constraints, budgets, and expanded profiles are unsupported.",
        ),
        _ => None,
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
    changed_fields: [&'static str; 5],
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
    fn profile_baseline_payloads_match_the_retained_complete_goldens() {
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
    fn profile_baseline_restores_compact_only_limitations() {
        let baseline =
            baseline_payload(ExposureProfile::Developer).expect("baseline payload reconstructs");
        for (tool, field) in [
            (McpTool::CodeLocate, "response_profile"),
            (McpTool::SymbolExplain, "response_profile"),
            (McpTool::SymbolRelationships, "response_profile"),
            (McpTool::FlowTrace, "response_profile"),
            (McpTool::ChangeImpact, "profile"),
            (McpTool::TestsSelect, "profile"),
            (McpTool::ArchitectureOverview, "response_profile"),
            (McpTool::ArchitectureCycles, "response_profile"),
            (McpTool::CodeDead, "response_profile"),
            (McpTool::PlanChange, "profile"),
        ] {
            let capability = &tool_definition(&baseline, tool)["_meta"][DISCOVERY_METADATA_KEY];
            assert!(capability.get("responseProfiles").is_none());
            let profile_limits: Vec<_> = capability["limitations"]
                .as_array()
                .expect("limitations are an array")
                .iter()
                .filter(|limitation| limitation["field"] == field)
                .map(|limitation| {
                    limitation["value"]
                        .as_str()
                        .expect("profile limitation has a value")
                })
                .collect();
            assert_eq!(profile_limits, ["evidence", "standard"]);
        }
    }

    fn tool_definition(payload: &Value, tool: McpTool) -> &Value {
        payload["tools"]
            .as_array()
            .expect("tools list is an array")
            .iter()
            .find(|definition| definition["name"] == tool.name())
            .expect("developer payload contains the requested tool")
    }
}
