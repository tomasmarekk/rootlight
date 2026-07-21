//! Parity gate for the canonical tool capability registry.
//!
//! Validates that the registry stays consistent with the tool catalog, the
//! batch allowlist, and the exposure profiles, so a schema-only capability, an
//! unregistered handler, a drifted batch flag, or a stale contract version
//! fails CI instead of silently shipping. The gate proves consistency, not
//! behavioral acceptance.

use rootlight_mcp_contract::MCP_SCHEMA_VERSION;
use rootlight_mcp_contract::capability::{
    CAPABILITIES, CapabilityStatus, ToolCapability, is_batch_eligible,
};
use rootlight_mcp_contract::catalog::{ExposureProfile, McpTool};

pub(crate) fn check() -> Result<(), CapabilityError> {
    let registry = CAPABILITIES.to_vec();
    let mut problems: Vec<Problem> = Vec::new();
    validate_catalog_parity(&registry, &mut problems);
    validate_contract_version(&registry, &mut problems);
    validate_batch_eligibility(&registry, &mut problems);
    validate_profile_membership(&mut problems);
    validate_handler_disposition(&registry, &mut problems);

    if problems.is_empty() {
        println!(
            "capability check passed: {} tools consistent with catalog, batch allowlist, and profiles",
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
        problems.push(Problem {
            id: "<registry>".to_owned(),
            kind: ProblemKind::CountMismatch {
                expected: McpTool::ALL.len(),
                observed: registry.len(),
            },
        });
    }
    let mut seen = std::collections::BTreeSet::new();
    for (position, entry) in registry.iter().enumerate() {
        if !seen.insert(entry.tool.name().to_owned()) {
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::DuplicateTool,
            });
        }
        let Some(expected) = McpTool::ALL.get(position) else {
            continue;
        };
        if entry.tool != *expected {
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::OrderMismatch {
                    position,
                    expected: expected.name().to_owned(),
                },
            });
        }
    }
}

fn validate_contract_version(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        if entry.contract_version != MCP_SCHEMA_VERSION {
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::ContractVersion {
                    version: entry.contract_version.to_owned(),
                },
            });
        }
    }
}

fn validate_batch_eligibility(registry: &[ToolCapability], problems: &mut Vec<Problem>) {
    for entry in registry {
        if entry.batch_eligible != is_batch_eligible(entry.tool) {
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::BatchEligibilityDrift,
            });
        }
        if entry.batch_eligible && !entry.tool.read_only() {
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::BatchNotReadOnly,
            });
        }
    }
}

fn validate_profile_membership(problems: &mut Vec<Problem>) {
    let expected = [
        (ExposureProfile::Scout, 6usize),
        (ExposureProfile::Analysis, 13usize),
        (ExposureProfile::Developer, 19usize),
    ];
    for (profile, count) in expected {
        let observed = profile.tools().len();
        if observed != count {
            problems.push(Problem {
                id: profile.name().to_owned(),
                kind: ProblemKind::ProfileCount {
                    expected: count,
                    observed,
                },
            });
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
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::MissingHandlerOrDisposition,
            });
        }
        if entry.fallback_summary.trim().is_empty() {
            problems.push(Problem {
                id: entry.tool.name().to_owned(),
                kind: ProblemKind::EmptyFallbackSummary,
            });
        }
    }
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Problem {
    id: String,
    kind: ProblemKind,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProblemKind {
    CountMismatch { expected: usize, observed: usize },
    DuplicateTool,
    OrderMismatch { position: usize, expected: String },
    ContractVersion { version: String },
    BatchEligibilityDrift,
    BatchNotReadOnly,
    ProfileCount { expected: usize, observed: usize },
    MissingHandlerOrDisposition,
    EmptyFallbackSummary,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            ProblemKind::CountMismatch { expected, observed } => write!(
                formatter,
                "{}: registry has {observed} entries, catalog has {expected}",
                self.id
            ),
            ProblemKind::DuplicateTool => {
                write!(formatter, "{}: duplicate tool in registry", self.id)
            }
            ProblemKind::OrderMismatch { position, expected } => write!(
                formatter,
                "{}: registry position {position} should be {expected}",
                self.id
            ),
            ProblemKind::ContractVersion { version } => write!(
                formatter,
                "{}: contract_version {version} does not match {MCP_SCHEMA_VERSION}",
                self.id
            ),
            ProblemKind::BatchEligibilityDrift => write!(
                formatter,
                "{}: batch_eligible drifted from the batch allowlist",
                self.id
            ),
            ProblemKind::BatchNotReadOnly => write!(
                formatter,
                "{}: batch-eligible tool must be read-only",
                self.id
            ),
            ProblemKind::ProfileCount { expected, observed } => write!(
                formatter,
                "{}: profile exposes {observed} tools, expected {expected}",
                self.id
            ),
            ProblemKind::MissingHandlerOrDisposition => write!(
                formatter,
                "{}: no handler and no explicit pre-execution disposition",
                self.id
            ),
            ProblemKind::EmptyFallbackSummary => {
                write!(formatter, "{}: fallback_summary is empty", self.id)
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CapabilityError {
    #[error("capability parity check failed:\n{report}")]
    Problems { report: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_mcp_contract::capability::CAPABILITIES;

    fn entry(tool: McpTool) -> ToolCapability {
        ToolCapability {
            tool,
            contract_version: MCP_SCHEMA_VERSION,
            batch_eligible: is_batch_eligible(tool),
            explain_supported: false,
            handler_available: true,
            status: CapabilityStatus::FallbackLimited,
            fallback_summary: "bounded",
        }
    }

    #[test]
    fn live_registry_passes_every_validation() {
        let mut problems = Vec::new();
        validate_catalog_parity(&CAPABILITIES, &mut problems);
        validate_contract_version(&CAPABILITIES, &mut problems);
        validate_batch_eligibility(&CAPABILITIES, &mut problems);
        validate_profile_membership(&mut problems);
        validate_handler_disposition(&CAPABILITIES, &mut problems);
        assert!(problems.is_empty(), "unexpected problems: {problems:?}");
    }

    #[test]
    fn schema_only_tool_without_catalog_entry_is_rejected() {
        // A registry that drops a catalog tool creates a count/order mismatch.
        let truncated: Vec<ToolCapability> = CAPABILITIES.iter().take(18).copied().collect();
        let mut problems = Vec::new();
        validate_catalog_parity(&truncated, &mut problems);
        assert!(!problems.is_empty(), "missing catalog tool must be caught");
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
                .any(|p| matches!(p.kind, ProblemKind::ContractVersion { .. }))
        );
    }

    #[test]
    fn drifted_batch_flag_is_rejected() {
        // repo.index is not batch-eligible; flagging it eligible is drift.
        let mut drifted = entry(McpTool::RepoIndex);
        drifted.batch_eligible = true;
        let mut problems = Vec::new();
        validate_batch_eligibility(&[drifted], &mut problems);
        assert!(problems.iter().any(|p| matches!(
            p.kind,
            ProblemKind::BatchEligibilityDrift | ProblemKind::BatchNotReadOnly
        )));
    }

    #[test]
    fn unregistered_handler_without_disposition_is_rejected() {
        let mut no_handler = entry(McpTool::FlowTrace);
        no_handler.handler_available = false;
        no_handler.status = CapabilityStatus::FallbackLimited;
        let mut problems = Vec::new();
        validate_handler_disposition(&[no_handler], &mut problems);
        assert!(
            problems
                .iter()
                .any(|p| matches!(p.kind, ProblemKind::MissingHandlerOrDisposition))
        );
    }

    #[test]
    fn explicit_blocked_disposition_without_handler_is_allowed() {
        let mut blocked = entry(McpTool::FlowTrace);
        blocked.handler_available = false;
        blocked.status = CapabilityStatus::Blocked;
        let mut problems = Vec::new();
        validate_handler_disposition(&[blocked], &mut problems);
        assert!(problems.is_empty(), "explicit disposition is acceptable");
    }
}
