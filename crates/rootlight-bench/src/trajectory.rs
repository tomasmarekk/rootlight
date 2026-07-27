//! Preregistered, source-free workflow trajectories and comparison runners.
//!
//! Adapters may briefly hold request, response, and source bytes, but the
//! published package retains only typed counters, normalized labels, and
//! cryptographic digests before conversion to the benchmark bundle schema.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read as _,
    path::{Path, PathBuf},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    ActualTokenizerIdentity, AgentTrajectory, RESULT_BUNDLE_SCHEMA_VERSION,
    TOKEN_ACCOUNTING_SCHEMA_VERSION, TokenInputKind, TokenMeasurement, TrajectoryBudget,
    TrajectoryCompleteness, TrajectoryEvidenceKind, TrajectoryEvidenceManifest,
    TrajectoryEvidenceReference, TrajectoryExposureProfile, TrajectoryOperationStatus,
    TrajectoryStep, TrajectoryToolIdentity, TrajectoryUsage, WorkflowTokenAccounting, sha256_hex,
};

/// Schema for the preregistered workflow protocol and its retained attempts.
pub const TRAJECTORY_PROTOCOL_SCHEMA_VERSION: &str = "rootlight.agent-trajectories/1";
/// Stable implementation identity bound into every produced package.
pub const TRAJECTORY_RUNNER_ID: &str = "rootlight-bench-trajectory-runner-v1";
/// Minimum distinct workflows required by the comparison protocol.
pub const MIN_PREREGISTERED_WORKFLOWS: usize = 12;
/// Maximum deterministic attempts retained for each workflow and condition.
pub const MAX_ATTEMPTS_PER_CONDITION: usize = 16;
/// Maximum serialized bytes in one complete trajectory evidence package.
pub const MAX_TRAJECTORY_PACKAGE_BYTES: usize = 16 * 1024 * 1024;

const TOKENIZER_VERSION: &str = "0.12.0";
const MAX_PROTOCOL_LABEL_BYTES: usize = 128;
const MAX_WORKFLOW_CALLS: usize = 32;
const SPOT_REVIEW_COUNT: usize = 6;

/// Closed comparison conditions executed for every workflow and seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryCondition {
    /// Rootlight through the daemon and MCP process boundary.
    Rootlight,
    /// Reproduced Codebase-Memory process, when executable.
    CodebaseMemory,
    /// Deterministic bounded direct file exploration.
    BoundedFileExploration,
}

impl TrajectoryCondition {
    const ALL: [Self; 3] = [
        Self::Rootlight,
        Self::CodebaseMemory,
        Self::BoundedFileExploration,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Rootlight => "rootlight",
            Self::CodebaseMemory => "codebase_memory",
            Self::BoundedFileExploration => "bounded_file_exploration",
        }
    }
}

/// Closed execution boundary used by one condition adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryExecutionBoundary {
    /// Separate Rootlight daemon and MCP JSON-RPC processes.
    DaemonMcpProcess,
    /// Separate baseline executable speaking the runner protocol.
    ExternalBaselineProcess,
    /// Local bounded regular-file reads without following links.
    LocalBoundedFiles,
    /// Preregistered adapter had no executable in the environment.
    UnavailableAdapter,
}

/// Closed workflow families exercised by the controlled agent benchmark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryWorkflowFamily {
    /// Locate an implementation from a concept.
    LocateImplementation,
    /// Explain an unfamiliar symbol.
    ExplainSymbol,
    /// Find callers and callees.
    CallRelationships,
    /// Prepare minimal context for a bug fix.
    BugFixContext,
    /// Assess change impact before editing.
    AssessChangeImpact,
    /// Select tests after an edit.
    SelectTests,
    /// Build an architecture overview.
    ArchitectureOverview,
    /// Find cyclic dependencies and a safe break point.
    CycleInvestigation,
    /// Find dead-code candidates.
    DeadCodeInvestigation,
    /// Trace a request across service boundaries.
    CrossServiceTrace,
    /// Prepare a refactoring boundary.
    RefactoringBoundary,
    /// Compare two Git states.
    HistoryComparison,
    /// Create a dependent API-migration plan in one batch.
    ApiMigrationBatch,
    /// Coordinate a multi-repository migration.
    MultiRepositoryMigration,
}

impl TrajectoryWorkflowFamily {
    const ALL: [Self; 14] = [
        Self::LocateImplementation,
        Self::ExplainSymbol,
        Self::CallRelationships,
        Self::BugFixContext,
        Self::AssessChangeImpact,
        Self::SelectTests,
        Self::ArchitectureOverview,
        Self::CycleInvestigation,
        Self::DeadCodeInvestigation,
        Self::CrossServiceTrace,
        Self::RefactoringBoundary,
        Self::HistoryComparison,
        Self::ApiMigrationBatch,
        Self::MultiRepositoryMigration,
    ];
}

/// Availability policy frozen before executing one condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterAvailabilityPolicy {
    /// Missing execution capability invalidates the attempt as a failure.
    Required,
    /// Missing executable is retained as `not_available` in the denominator.
    OptionalExecutable,
}

/// Shared resource ceilings applied identically to all conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectorySharedBounds {
    /// Maximum calls retained for one attempt.
    pub tool_calls: u32,
    /// Maximum elapsed nanoseconds for one attempt.
    pub elapsed_ns: u64,
    /// Maximum returned result items.
    pub result_items: u64,
    /// Maximum source bytes made visible to the condition.
    pub source_bytes: u64,
    /// Maximum actual request-plus-response tokens.
    pub total_tokens: u64,
}

impl Default for TrajectorySharedBounds {
    fn default() -> Self {
        Self {
            tool_calls: 8,
            elapsed_ns: 30_000_000_000,
            result_items: 200,
            source_bytes: 128 * 1024,
            total_tokens: 20_000,
        }
    }
}

/// Fixed stopping policy shared by all comparison conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryStoppingPolicy {
    /// Stop after the first terminal task outcome.
    pub stop_on_terminal_outcome: bool,
    /// Stop when any hard shared resource limit is exhausted.
    pub stop_on_hard_limit: bool,
    /// Continuations may be followed only while all original bounds remain.
    pub follow_bounded_continuations: bool,
}

impl Default for TrajectoryStoppingPolicy {
    fn default() -> Self {
        Self {
            stop_on_terminal_outcome: true,
            stop_on_hard_limit: true,
            follow_bounded_continuations: true,
        }
    }
}

/// Fixed retry policy shared by all comparison conditions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryRetryPolicy {
    /// Maximum retries after the initial operation.
    pub max_retries: u8,
    /// Stable error codes eligible for retry.
    pub retryable_error_codes: Vec<String>,
}

impl Default for TrajectoryRetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 1,
            retryable_error_codes: vec![
                "process_unavailable".to_owned(),
                "response_timeout".to_owned(),
            ],
        }
    }
}

/// One comparison-condition configuration frozen before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryConditionProtocol {
    /// Closed condition identity.
    pub condition: TrajectoryCondition,
    /// Normalized adapter implementation identity.
    pub adapter_id: String,
    /// Required process or local execution boundary.
    pub boundary: TrajectoryExecutionBoundary,
    /// Whether executable absence remains an observed denominator outcome.
    pub availability: AdapterAvailabilityPolicy,
    /// Closed logical tool access granted to this condition.
    pub tool_access: Vec<String>,
}

/// One source-free task definition frozen before execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryWorkflowProtocol {
    /// Stable workflow identity.
    pub workflow_id: String,
    /// Required representative family.
    pub family: TrajectoryWorkflowFamily,
    /// Stable task identity shared by all conditions.
    pub task_id: String,
    /// Source-free expected evidence identities used only for later grading.
    pub expected_evidence: Vec<String>,
    /// Exact Rootlight call sequence preregistered for this workflow.
    pub rootlight_tools: Vec<String>,
    /// Whether a status preflight is semantically necessary for this task.
    pub allows_status_preflight: bool,
}

/// Complete preregistered workflow and baseline protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryProtocol {
    /// Protocol schema.
    pub schema: String,
    /// Stable experiment identity; changing inputs requires a new identity.
    pub experiment_id: String,
    /// Legally distributable fixture identity.
    pub fixture_id: String,
    /// SHA-256 of the exact fixture tree manifest.
    pub fixture_sha256: String,
    /// Stable runner implementation identity.
    pub runner_id: String,
    /// Deterministic attempt seeds retained in canonical order.
    pub attempt_seeds: Vec<u64>,
    /// Identical hard bounds for all conditions.
    pub bounds: TrajectorySharedBounds,
    /// Identical stopping rules for all conditions.
    pub stopping: TrajectoryStoppingPolicy,
    /// Identical retry rules for all conditions.
    pub retry: TrajectoryRetryPolicy,
    /// Exclusion reasons frozen before execution.
    pub allowed_exclusion_reasons: Vec<String>,
    /// Canonically ordered condition definitions.
    pub conditions: Vec<TrajectoryConditionProtocol>,
    /// Canonically ordered workflow definitions.
    pub workflows: Vec<TrajectoryWorkflowProtocol>,
}

impl TrajectoryProtocol {
    /// Validates preregistration completeness, canonical ordering, and bounds.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] when the protocol can omit a required
    /// workflow, condition, attempt, failure class, or fixed comparison rule.
    pub fn validate(&self) -> Result<(), TrajectoryError> {
        if self.schema != TRAJECTORY_PROTOCOL_SCHEMA_VERSION {
            return Err(TrajectoryError::UnsupportedSchema);
        }
        for label in [
            self.experiment_id.as_str(),
            self.fixture_id.as_str(),
            self.runner_id.as_str(),
        ] {
            validate_label(label)?;
        }
        validate_sha256(&self.fixture_sha256)?;
        if self.runner_id != TRAJECTORY_RUNNER_ID
            || self.workflows.len() < MIN_PREREGISTERED_WORKFLOWS
            || self.attempt_seeds.is_empty()
            || self.attempt_seeds.len() > MAX_ATTEMPTS_PER_CONDITION
            || self.attempt_seeds.windows(2).any(|pair| pair[0] >= pair[1])
            || self.conditions.len() != TrajectoryCondition::ALL.len()
            || self.retry.max_retries > 3
            || self.bounds.tool_calls == 0
            || usize::try_from(self.bounds.tool_calls).unwrap_or(usize::MAX) > MAX_WORKFLOW_CALLS
            || self.bounds.elapsed_ns == 0
            || self.bounds.result_items == 0
            || self.bounds.source_bytes == 0
            || self.bounds.total_tokens == 0
        {
            return Err(TrajectoryError::InvalidProtocol);
        }
        validate_sorted_labels(&self.retry.retryable_error_codes)?;
        validate_sorted_labels(&self.allowed_exclusion_reasons)?;

        for (condition, expected) in self.conditions.iter().zip(TrajectoryCondition::ALL.iter()) {
            if condition.condition != *expected {
                return Err(TrajectoryError::InvalidProtocol);
            }
            validate_label(&condition.adapter_id)?;
            validate_sorted_labels(&condition.tool_access)?;
            match condition.condition {
                TrajectoryCondition::Rootlight
                    if condition.boundary != TrajectoryExecutionBoundary::DaemonMcpProcess
                        || condition.availability != AdapterAvailabilityPolicy::Required =>
                {
                    return Err(TrajectoryError::InvalidProtocol);
                }
                TrajectoryCondition::CodebaseMemory
                    if condition.availability != AdapterAvailabilityPolicy::OptionalExecutable =>
                {
                    return Err(TrajectoryError::InvalidProtocol);
                }
                TrajectoryCondition::BoundedFileExploration
                    if condition.boundary != TrajectoryExecutionBoundary::LocalBoundedFiles
                        || condition.availability != AdapterAvailabilityPolicy::Required =>
                {
                    return Err(TrajectoryError::InvalidProtocol);
                }
                _ => {}
            }
        }

        let mut families = BTreeSet::new();
        let mut workflow_ids = BTreeSet::new();
        for workflow in &self.workflows {
            validate_label(&workflow.workflow_id)?;
            validate_label(&workflow.task_id)?;
            validate_sorted_labels(&workflow.expected_evidence)?;
            if workflow.rootlight_tools.len() > MAX_WORKFLOW_CALLS {
                return Err(TrajectoryError::InvalidProtocol);
            }
            for tool in &workflow.rootlight_tools {
                validate_label(tool)?;
            }
            if workflow.expected_evidence.is_empty()
                || workflow.rootlight_tools.is_empty()
                || !families.insert(workflow.family)
                || !workflow_ids.insert(workflow.workflow_id.as_str())
            {
                return Err(TrajectoryError::InvalidProtocol);
            }
        }
        if families != BTreeSet::from(TrajectoryWorkflowFamily::ALL) {
            return Err(TrajectoryError::InvalidProtocol);
        }
        Ok(())
    }

    /// Returns deterministic protocol, configuration, task, fixture, and
    /// runner digests.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] if validation or canonical serialization
    /// fails.
    pub fn digests(&self) -> Result<TrajectoryProtocolDigests, TrajectoryError> {
        self.validate()?;
        let configuration = ConfigurationDigestInput {
            experiment_id: &self.experiment_id,
            attempt_seeds: &self.attempt_seeds,
            bounds: self.bounds,
            stopping: self.stopping,
            retry: &self.retry,
            allowed_exclusion_reasons: &self.allowed_exclusion_reasons,
            conditions: &self.conditions,
        };
        Ok(TrajectoryProtocolDigests {
            protocol_sha256: digest_json("rootlight.trajectory.protocol.v1", self)?,
            configuration_sha256: digest_json(
                "rootlight.trajectory.configuration.v1",
                &configuration,
            )?,
            tasks_sha256: digest_json("rootlight.trajectory.tasks.v1", &self.workflows)?,
            fixture_sha256: self.fixture_sha256.clone(),
            runner_sha256: digest_json(
                "rootlight.trajectory.runner.v1",
                &RunnerDigestInput {
                    runner_id: TRAJECTORY_RUNNER_ID,
                    crate_version: env!("CARGO_PKG_VERSION"),
                    protocol_schema: TRAJECTORY_PROTOCOL_SCHEMA_VERSION,
                    bundle_schema: RESULT_BUNDLE_SCHEMA_VERSION,
                    token_accounting_schema: TOKEN_ACCOUNTING_SCHEMA_VERSION,
                    tokenizer: "o200k_base",
                    tokenizer_version: TOKENIZER_VERSION,
                    max_attempts_per_condition: MAX_ATTEMPTS_PER_CONDITION,
                    max_workflow_calls: MAX_WORKFLOW_CALLS,
                    max_package_bytes: MAX_TRAJECTORY_PACKAGE_BYTES,
                },
            )?,
        })
    }

    /// Returns the digest of one preregistered task definition.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] when the workflow is unknown or cannot be
    /// serialized.
    pub fn task_digest(&self, workflow_id: &str) -> Result<String, TrajectoryError> {
        let workflow = self
            .workflows
            .iter()
            .find(|workflow| workflow.workflow_id == workflow_id)
            .ok_or(TrajectoryError::UnknownWorkflow)?;
        digest_json("rootlight.trajectory.task.v1", workflow)
    }

    fn condition_digest(&self, condition: TrajectoryCondition) -> Result<String, TrajectoryError> {
        let value = self
            .conditions
            .iter()
            .find(|value| value.condition == condition)
            .ok_or(TrajectoryError::InvalidProtocol)?;
        digest_json("rootlight.trajectory.condition.v1", value)
    }
}

#[derive(Serialize)]
struct ConfigurationDigestInput<'a> {
    experiment_id: &'a str,
    attempt_seeds: &'a [u64],
    bounds: TrajectorySharedBounds,
    stopping: TrajectoryStoppingPolicy,
    retry: &'a TrajectoryRetryPolicy,
    allowed_exclusion_reasons: &'a [String],
    conditions: &'a [TrajectoryConditionProtocol],
}

#[derive(Serialize)]
struct RunnerDigestInput<'a> {
    runner_id: &'a str,
    crate_version: &'a str,
    protocol_schema: &'a str,
    bundle_schema: &'a str,
    token_accounting_schema: &'a str,
    tokenizer: &'a str,
    tokenizer_version: &'a str,
    max_attempts_per_condition: usize,
    max_workflow_calls: usize,
    max_package_bytes: usize,
}

/// Frozen digest inventory for one protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryProtocolDigests {
    /// Digest of the complete preregistration.
    pub protocol_sha256: String,
    /// Digest of shared comparison configuration.
    pub configuration_sha256: String,
    /// Digest of the complete ordered task set.
    pub tasks_sha256: String,
    /// Digest of the exact fixture manifest.
    pub fixture_sha256: String,
    /// Digest of the runner implementation identity.
    pub runner_sha256: String,
}

/// Stable observed task outcome retained in the denominator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum TrajectoryAttemptOutcome {
    /// Workflow reached its declared success condition.
    Succeeded,
    /// Workflow reached a terminal failure.
    Failed {
        /// Stable source-free failure code.
        error_code: String,
    },
    /// Workflow exhausted its monotonic deadline.
    TimedOut {
        /// Stable source-free timeout code.
        error_code: String,
    },
    /// Workflow was cancelled.
    Cancelled {
        /// Stable source-free cancellation code.
        error_code: String,
    },
    /// Adapter explicitly reported an unsupported capability.
    Unsupported {
        /// Stable source-free unsupported code.
        error_code: String,
    },
    /// Optional executable was unavailable in the environment.
    NotAvailable {
        /// Stable source-free availability code.
        error_code: String,
    },
    /// Preregistered exclusion applied without removing the denominator row.
    Excluded {
        /// Stable preregistered exclusion reason.
        reason_code: String,
    },
}

/// Source-free later-grading observations for one call or attempt.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryClaimSignals {
    /// Claims not supported by observed evidence.
    pub unsupported_claims: u32,
    /// Claims lacking a typed citation or source reference.
    pub missing_source_references: u32,
    /// Truncation or continuation state ignored by the workflow.
    pub ignored_truncation: u32,
    /// Recovery behavior that contradicted the observed failure.
    pub incorrect_recovery: u32,
}

impl TrajectoryClaimSignals {
    fn checked_add(self, other: Self) -> Result<Self, TrajectoryError> {
        Ok(Self {
            unsupported_claims: self
                .unsupported_claims
                .checked_add(other.unsupported_claims)
                .ok_or(TrajectoryError::CounterOverflow)?,
            missing_source_references: self
                .missing_source_references
                .checked_add(other.missing_source_references)
                .ok_or(TrajectoryError::CounterOverflow)?,
            ignored_truncation: self
                .ignored_truncation
                .checked_add(other.ignored_truncation)
                .ok_or(TrajectoryError::CounterOverflow)?,
            incorrect_recovery: self
                .incorrect_recovery
                .checked_add(other.incorrect_recovery)
                .ok_or(TrajectoryError::CounterOverflow)?,
        })
    }
}

/// Ephemeral call observation returned by a condition adapter.
///
/// Request, response, and source bytes are consumed by the runner and never
/// copied into [`TrajectoryEvidencePackage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTrajectoryCall {
    /// Logical operation identity shared by retries.
    pub operation_id: String,
    /// Tool identity.
    pub tool: TrajectoryToolIdentity,
    /// Exposure profile.
    pub exposure_profile: TrajectoryExposureProfile,
    /// Terminal call status.
    pub operation_status: TrajectoryOperationStatus,
    /// Zero-based retry ordinal.
    pub retry_ordinal: u8,
    /// Exact serialized request frame.
    pub request_frame: Vec<u8>,
    /// Exact serialized response frame.
    pub response_frame: Vec<u8>,
    /// Exact source bytes attributed within the response.
    pub source_frame: Vec<u8>,
    /// Monotonic elapsed nanoseconds.
    pub elapsed_ns: u64,
    /// Returned result items.
    pub result_items: u64,
    /// Whether the response reported truncation.
    pub truncated: bool,
    /// Whether a bounded continuation was available.
    pub continuation_available: bool,
    /// Later-grading observability signals.
    pub claim_signals: TrajectoryClaimSignals,
}

/// Ephemeral complete attempt returned by a condition adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawTrajectoryAttempt {
    /// Observed terminal attempt outcome.
    pub outcome: TrajectoryAttemptOutcome,
    /// Ordered calls, including retries and failures.
    pub calls: Vec<RawTrajectoryCall>,
}

/// Fixed adapter input shared across every condition.
#[derive(Debug, Clone, Copy)]
pub struct TrajectoryExecutionInput<'a> {
    /// Preregistered workflow.
    pub workflow: &'a TrajectoryWorkflowProtocol,
    /// Digest of the exact workflow definition.
    pub task_sha256: &'a str,
    /// Exact fixture digest.
    pub fixture_sha256: &'a str,
    /// Zero-based attempt ordinal.
    pub attempt_index: u16,
    /// Deterministic seed.
    pub seed: u64,
    /// Shared hard bounds.
    pub bounds: TrajectorySharedBounds,
    /// Shared stopping rules.
    pub stopping: TrajectoryStoppingPolicy,
    /// Shared retry rules.
    pub retry: &'a TrajectoryRetryPolicy,
}

/// Adapter boundary for one comparison condition.
pub trait TrajectoryAdapter {
    /// Returns the fixed condition implemented by this adapter.
    fn condition(&self) -> TrajectoryCondition;

    /// Returns the actual process or local boundary used for this run.
    fn execution_boundary(&self) -> TrajectoryExecutionBoundary;

    /// Executes one workflow and returns every terminal call.
    fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt;
}

/// Tokenizer boundary used for mandatory actual counts.
pub trait TrajectoryTokenizer {
    /// Returns pinned tokenizer provenance.
    fn identity(&self) -> ActualTokenizerIdentity;

    /// Counts exact UTF-8 input bytes.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] when the input is invalid UTF-8 or the
    /// tokenizer cannot produce a count.
    fn count(&self, input: &[u8]) -> Result<u64, TrajectoryError>;
}

/// Pinned `o200k_base` tokenizer used by trajectory evidence production.
pub struct O200kTrajectoryTokenizer {
    tokenizer: tiktoken_rs::CoreBPE,
}

impl O200kTrajectoryTokenizer {
    /// Initializes the pinned tokenizer.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError::TokenizerUnavailable`] if its static
    /// vocabulary cannot be initialized.
    pub fn new() -> Result<Self, TrajectoryError> {
        let tokenizer =
            tiktoken_rs::o200k_base().map_err(|_| TrajectoryError::TokenizerUnavailable)?;
        Ok(Self { tokenizer })
    }
}

impl TrajectoryTokenizer for O200kTrajectoryTokenizer {
    fn identity(&self) -> ActualTokenizerIdentity {
        ActualTokenizerIdentity {
            provider: "openai".to_owned(),
            model: "provider_neutral_workflow".to_owned(),
            tokenizer: "o200k_base".to_owned(),
            implementation: "tiktoken_rs".to_owned(),
            implementation_version: Some(TOKENIZER_VERSION.to_owned()),
            implementation_sha256: None,
            asset_sha256: None,
        }
    }

    fn count(&self, input: &[u8]) -> Result<u64, TrajectoryError> {
        let text = std::str::from_utf8(input).map_err(|_| TrajectoryError::InvalidUtf8)?;
        u64::try_from(self.tokenizer.encode_with_special_tokens(text).len())
            .map_err(|_| TrajectoryError::CounterOverflow)
    }
}

/// One sanitized call retained in a trajectory evidence package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryCallRecord {
    /// Contiguous call index within the attempt.
    pub call_index: u32,
    /// Logical operation identity shared by retries.
    pub operation_id: String,
    /// Zero-based retry ordinal.
    pub retry_ordinal: u8,
    /// Versioned tool identity.
    pub tool: TrajectoryToolIdentity,
    /// Exposure profile.
    pub exposure_profile: TrajectoryExposureProfile,
    /// Closed terminal status.
    pub operation_status: TrajectoryOperationStatus,
    /// Exact request, response, source, and total token evidence.
    pub accounting: WorkflowTokenAccounting,
    /// Monotonic elapsed nanoseconds.
    pub elapsed_ns: u64,
    /// Returned result items.
    pub result_items: u64,
    /// Whether the response reported truncation.
    pub truncated: bool,
    /// Whether a bounded continuation was available.
    pub continuation_available: bool,
    /// Later-grading observability signals.
    pub claim_signals: TrajectoryClaimSignals,
}

/// One complete source-free attempt retained in the denominator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryAttemptRecord {
    /// Stable attempt identity.
    pub attempt_id: String,
    /// Stable experiment identity.
    pub experiment_id: String,
    /// Protocol digest.
    pub protocol_sha256: String,
    /// Workflow identity.
    pub workflow_id: String,
    /// Workflow-definition digest.
    pub task_sha256: String,
    /// Fixture digest.
    pub fixture_sha256: String,
    /// Runner digest.
    pub runner_sha256: String,
    /// Condition-configuration digest.
    pub condition_sha256: String,
    /// Comparison condition.
    pub condition: TrajectoryCondition,
    /// Actual execution boundary.
    pub execution_boundary: TrajectoryExecutionBoundary,
    /// Zero-based attempt index.
    pub attempt_index: u16,
    /// Deterministic attempt seed.
    pub deterministic_seed: u64,
    /// Terminal task outcome, including exclusions and unavailable baselines.
    pub outcome: TrajectoryAttemptOutcome,
    /// Ordered complete call record.
    pub calls: Vec<TrajectoryCallRecord>,
    /// Total monotonic elapsed nanoseconds.
    pub elapsed_ns: u64,
    /// Number of calls whose retry ordinal is nonzero.
    pub retry_count: u32,
    /// Redundant status preflights observed in this attempt.
    pub redundant_status_preflights: u32,
    /// Aggregate later-grading observability signals.
    pub claim_signals: TrajectoryClaimSignals,
    /// Typed digest references for bundle conversion.
    pub evidence: Vec<TrajectoryEvidenceReference>,
}

/// Complete denominator counts derived from retained attempts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryDenominator {
    /// Attempts required by workflow × condition × seed.
    pub expected_attempts: u32,
    /// Attempts actually retained.
    pub observed_attempts: u32,
    /// Successful attempts.
    pub succeeded: u32,
    /// Failed attempts.
    pub failed: u32,
    /// Timed-out attempts.
    pub timed_out: u32,
    /// Cancelled attempts.
    pub cancelled: u32,
    /// Unsupported attempts.
    pub unsupported: u32,
    /// Optional executable not available attempts.
    pub not_available: u32,
    /// Preregistered exclusions retained in the denominator.
    pub excluded: u32,
    /// Attempts containing at least one retry.
    pub retried: u32,
    /// Total retained calls.
    pub calls: u32,
    /// Total redundant status preflights.
    pub redundant_status_preflights: u32,
    /// Aggregate claim-observability signals.
    pub claim_signals: TrajectoryClaimSignals,
}

/// Source-free package ready for bundle conversion and later blinded grading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrajectoryEvidencePackage {
    /// Package schema.
    pub schema: String,
    /// Complete preregistered protocol.
    pub protocol: TrajectoryProtocol,
    /// Frozen digest inventory.
    pub digests: TrajectoryProtocolDigests,
    /// Canonically ordered complete denominator.
    pub attempts: Vec<TrajectoryAttemptRecord>,
    /// Reconciled denominator summary.
    pub denominator: TrajectoryDenominator,
    /// Deterministically selected attempt IDs for manual spot review.
    pub spot_review_attempt_ids: Vec<String>,
}

impl TrajectoryEvidencePackage {
    /// Validates digest bindings, denominator completeness, accounting,
    /// canonical order, observability counters, and the privacy boundary.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] for missing or duplicate attempts,
    /// inconsistent accounting, mutated digests, invalid exclusions, or
    /// source-bearing/free-form fields.
    pub fn validate(&self) -> Result<(), TrajectoryError> {
        if self.schema != TRAJECTORY_PROTOCOL_SCHEMA_VERSION {
            return Err(TrajectoryError::UnsupportedSchema);
        }
        self.protocol.validate()?;
        if self.digests != self.protocol.digests()? {
            return Err(TrajectoryError::DigestMismatch);
        }
        validate_tokenizer_identity_fields(
            self.attempts
                .first()
                .and_then(|attempt| attempt.calls.first())
                .and_then(|call| call.accounting.tokenizer.as_ref()),
        )?;

        let expected_keys = expected_attempt_keys(&self.protocol)?;
        if self.attempts.len() != expected_keys.len() {
            return Err(TrajectoryError::IncompleteDenominator);
        }
        let mut observed_keys = BTreeSet::new();
        for (attempt, expected) in self.attempts.iter().zip(&expected_keys) {
            let key = (
                attempt.workflow_id.clone(),
                attempt.attempt_index,
                attempt.condition,
            );
            if key != *expected || !observed_keys.insert(key) {
                return Err(TrajectoryError::IncompleteDenominator);
            }
            self.validate_attempt(attempt)?;
        }
        if observed_keys.len() != expected_keys.len() {
            return Err(TrajectoryError::IncompleteDenominator);
        }

        if self.denominator != summarize_attempts(&self.protocol, &self.attempts)? {
            return Err(TrajectoryError::DenominatorMismatch);
        }
        if self.spot_review_attempt_ids != spot_review_selection(&self.attempts) {
            return Err(TrajectoryError::SpotReviewMismatch);
        }
        let bytes = serde_json::to_vec(self).map_err(|_| TrajectoryError::Serialization)?;
        if bytes.len() > MAX_TRAJECTORY_PACKAGE_BYTES {
            return Err(TrajectoryError::PackageTooLarge);
        }
        privacy_scan_json(&bytes)?;
        Ok(())
    }

    fn validate_attempt(&self, attempt: &TrajectoryAttemptRecord) -> Result<(), TrajectoryError> {
        for label in [
            attempt.attempt_id.as_str(),
            attempt.experiment_id.as_str(),
            attempt.workflow_id.as_str(),
        ] {
            validate_label(label)?;
        }
        for digest in [
            attempt.protocol_sha256.as_str(),
            attempt.task_sha256.as_str(),
            attempt.fixture_sha256.as_str(),
            attempt.runner_sha256.as_str(),
            attempt.condition_sha256.as_str(),
        ] {
            validate_sha256(digest)?;
        }
        if attempt.experiment_id != self.protocol.experiment_id
            || attempt.protocol_sha256 != self.digests.protocol_sha256
            || attempt.fixture_sha256 != self.digests.fixture_sha256
            || attempt.runner_sha256 != self.digests.runner_sha256
            || attempt.task_sha256 != self.protocol.task_digest(&attempt.workflow_id)?
            || attempt.condition_sha256 != self.protocol.condition_digest(attempt.condition)?
            || attempt.calls.is_empty()
            || attempt.calls.len()
                > usize::try_from(self.protocol.bounds.tool_calls).unwrap_or(usize::MAX)
        {
            return Err(TrajectoryError::InvalidAttempt);
        }
        let workflow = self
            .protocol
            .workflows
            .iter()
            .find(|workflow| workflow.workflow_id == attempt.workflow_id)
            .ok_or(TrajectoryError::UnknownWorkflow)?;
        let expected_boundary = self
            .protocol
            .conditions
            .iter()
            .find(|condition| condition.condition == attempt.condition)
            .ok_or(TrajectoryError::InvalidProtocol)?
            .boundary;
        if attempt.execution_boundary != expected_boundary
            && attempt.execution_boundary != TrajectoryExecutionBoundary::UnavailableAdapter
        {
            return Err(TrajectoryError::InvalidAttempt);
        }
        if let TrajectoryAttemptOutcome::Excluded { reason_code } = &attempt.outcome {
            validate_label(reason_code)?;
            if self
                .protocol
                .allowed_exclusion_reasons
                .binary_search(reason_code)
                .is_err()
            {
                return Err(TrajectoryError::UnregisteredExclusion);
            }
        }

        let mut elapsed_ns = 0_u64;
        let mut result_items = 0_u64;
        let mut source_bytes = 0_u64;
        let mut actual_tokens = 0_u64;
        let mut retry_count = 0_u32;
        let mut claims = TrajectoryClaimSignals::default();
        let mut redundant_status_preflights = 0_u32;
        let mut prior_calls: BTreeMap<&str, &TrajectoryCallRecord> = BTreeMap::new();
        for (index, call) in attempt.calls.iter().enumerate() {
            if call.call_index != u32::try_from(index).unwrap_or(u32::MAX) {
                return Err(TrajectoryError::InvalidAttempt);
            }
            validate_label(&call.operation_id)?;
            validate_label(&call.tool.tool_id)?;
            validate_label(&call.tool.tool_version)?;
            call.accounting.validate()?;
            validate_tokenizer_identity_fields(call.accounting.tokenizer.as_ref())?;
            if call.accounting.request.actual_tokens.is_none()
                || call.accounting.response.actual_tokens.is_none()
                || call.accounting.source.actual_tokens.is_none()
            {
                return Err(TrajectoryError::MissingActualTokens);
            }
            let actual_total = call
                .accounting
                .total
                .actual_tokens
                .ok_or(TrajectoryError::MissingActualTokens)?;
            let source_tokens = call
                .accounting
                .source
                .actual_tokens
                .ok_or(TrajectoryError::MissingActualTokens)?;
            if actual_total > self.protocol.bounds.total_tokens
                || source_tokens > actual_total
                || call.accounting.source.serialized_bytes > self.protocol.bounds.source_bytes
                || call.result_items > self.protocol.bounds.result_items
                || call.elapsed_ns > self.protocol.bounds.elapsed_ns
            {
                return Err(TrajectoryError::AttemptLimitExceeded);
            }
            let prior_call = prior_calls.get(call.operation_id.as_str()).copied();
            let expected_retry =
                prior_call.map_or(0, |prior| prior.retry_ordinal.saturating_add(1));
            if call.retry_ordinal != expected_retry
                || call.retry_ordinal > self.protocol.retry.max_retries
                || prior_call.is_some_and(|prior| {
                    prior.tool != call.tool
                        || !retryable_status(
                            &prior.operation_status,
                            &self.protocol.retry.retryable_error_codes,
                        )
                })
            {
                return Err(TrajectoryError::InvalidRetry);
            }
            prior_calls.insert(&call.operation_id, call);
            retry_count = retry_count
                .checked_add(u32::from(call.retry_ordinal > 0))
                .ok_or(TrajectoryError::CounterOverflow)?;
            elapsed_ns = elapsed_ns
                .checked_add(call.elapsed_ns)
                .ok_or(TrajectoryError::CounterOverflow)?;
            result_items = result_items
                .checked_add(call.result_items)
                .ok_or(TrajectoryError::CounterOverflow)?;
            source_bytes = source_bytes
                .checked_add(call.accounting.source.serialized_bytes)
                .ok_or(TrajectoryError::CounterOverflow)?;
            actual_tokens = actual_tokens
                .checked_add(actual_total)
                .ok_or(TrajectoryError::CounterOverflow)?;
            claims = claims.checked_add(call.claim_signals)?;
            if call.tool.tool_id == "repo.status" && !workflow.allows_status_preflight {
                redundant_status_preflights = redundant_status_preflights
                    .checked_add(1)
                    .ok_or(TrajectoryError::CounterOverflow)?;
            }
        }
        if elapsed_ns != attempt.elapsed_ns
            || retry_count != attempt.retry_count
            || claims != attempt.claim_signals
            || redundant_status_preflights != attempt.redundant_status_preflights
        {
            return Err(TrajectoryError::AttemptAccountingMismatch);
        }
        if elapsed_ns > self.protocol.bounds.elapsed_ns
            || result_items > self.protocol.bounds.result_items
            || source_bytes > self.protocol.bounds.source_bytes
            || actual_tokens > self.protocol.bounds.total_tokens
        {
            return Err(TrajectoryError::AttemptLimitExceeded);
        }
        validate_terminal_outcome(&attempt.outcome, &attempt.calls)?;
        validate_evidence(&attempt.evidence)?;
        Ok(())
    }

    /// Converts retained records to the existing closed bundle trajectory
    /// schema without weakening failed or excluded outcomes.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] if package validation fails.
    pub fn agent_trajectories(&self) -> Result<Vec<AgentTrajectory>, TrajectoryError> {
        self.validate()?;
        let mut trajectories = self
            .attempts
            .iter()
            .map(|attempt| {
                let completeness = match &attempt.outcome {
                    TrajectoryAttemptOutcome::Excluded { reason_code } => {
                        TrajectoryCompleteness::Excluded {
                            reason_code: reason_code.clone(),
                        }
                    }
                    _ => TrajectoryCompleteness::Complete,
                };
                let steps = attempt
                    .calls
                    .iter()
                    .map(|call| {
                        let actual_tokens = call
                            .accounting
                            .total
                            .actual_tokens
                            .ok_or(TrajectoryError::MissingActualTokens)?;
                        let source_tokens = call
                            .accounting
                            .source
                            .actual_tokens
                            .ok_or(TrajectoryError::MissingActualTokens)?;
                        Ok(TrajectoryStep {
                            step_index: call.call_index,
                            tool: call.tool.clone(),
                            exposure_profile: call.exposure_profile,
                            operation_status: call.operation_status.clone(),
                            budget: TrajectoryBudget {
                                tool_calls: u64::from(self.protocol.bounds.tool_calls),
                                elapsed_ns: self.protocol.bounds.elapsed_ns,
                                result_items: self.protocol.bounds.result_items,
                                source_bytes: self.protocol.bounds.source_bytes,
                                tokens: self.protocol.bounds.total_tokens,
                            },
                            usage: TrajectoryUsage {
                                tool_calls: 1,
                                elapsed_ns: call.elapsed_ns,
                                result_items: call.result_items,
                                source_bytes: call.accounting.source.serialized_bytes,
                                tokens: actual_tokens,
                            },
                            request_tokens: call
                                .accounting
                                .request
                                .actual_tokens
                                .ok_or(TrajectoryError::MissingActualTokens)?,
                            response_tokens: call
                                .accounting
                                .response
                                .actual_tokens
                                .ok_or(TrajectoryError::MissingActualTokens)?,
                            source_tokens,
                        })
                    })
                    .collect::<Result<Vec<_>, TrajectoryError>>()?;
                Ok(AgentTrajectory {
                    schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
                    workflow_id: attempt.workflow_id.clone(),
                    attempt_id: attempt.attempt_id.clone(),
                    baseline_variant: attempt.condition.label().to_owned(),
                    completeness,
                    steps,
                    evidence: attempt.evidence.clone(),
                })
            })
            .collect::<Result<Vec<_>, TrajectoryError>>()?;
        trajectories.sort_by(|left, right| {
            left.workflow_id
                .cmp(&right.workflow_id)
                .then_with(|| left.attempt_id.cmp(&right.attempt_id))
                .then_with(|| left.baseline_variant.cmp(&right.baseline_variant))
        });
        Ok(trajectories)
    }

    /// Builds the closed digest inventory resolving converted trajectories.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] if package validation fails.
    pub fn evidence_manifest(&self) -> Result<TrajectoryEvidenceManifest, TrajectoryError> {
        self.validate()?;
        let mut artifacts = self
            .attempts
            .iter()
            .flat_map(|attempt| attempt.evidence.iter().cloned())
            .collect::<Vec<_>>();
        artifacts.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.artifact_id.cmp(&right.artifact_id))
                .then_with(|| left.sha256.cmp(&right.sha256))
        });
        artifacts.dedup();
        Ok(TrajectoryEvidenceManifest {
            schema_version: RESULT_BUNDLE_SCHEMA_VERSION.to_owned(),
            artifacts,
        })
    }
}

/// Encodes one validated source-free evidence package.
///
/// # Errors
///
/// Returns [`TrajectoryError`] when validation, serialization, or the encoded
/// byte ceiling fails.
pub fn encode_trajectory_evidence(
    package: &TrajectoryEvidencePackage,
) -> Result<Vec<u8>, TrajectoryError> {
    package.validate()?;
    let encoded = serde_json::to_vec(package).map_err(|_| TrajectoryError::Serialization)?;
    if encoded.len() > MAX_TRAJECTORY_PACKAGE_BYTES {
        return Err(TrajectoryError::PackageTooLarge);
    }
    Ok(encoded)
}

/// Decodes and validates one bounded source-free evidence package.
///
/// # Errors
///
/// Returns [`TrajectoryError`] before allocation-heavy decoding when the byte
/// ceiling is exceeded, or when syntax, schema, or integrity is invalid.
pub fn decode_trajectory_evidence(
    encoded: &[u8],
) -> Result<TrajectoryEvidencePackage, TrajectoryError> {
    if encoded.len() > MAX_TRAJECTORY_PACKAGE_BYTES {
        return Err(TrajectoryError::PackageTooLarge);
    }
    let package: TrajectoryEvidencePackage =
        serde_json::from_slice(encoded).map_err(|_| TrajectoryError::InvalidEncoding)?;
    package.validate()?;
    Ok(package)
}

/// Executes the complete preregistered suite without dropping adapter failures.
///
/// # Errors
///
/// Returns [`TrajectoryError`] only when preregistration, tokenizer, digest,
/// serialization, or final package integrity fails. Adapter-level failures
/// remain ordinary denominator attempts.
pub fn run_trajectory_suite(
    protocol: TrajectoryProtocol,
    rootlight: &mut dyn TrajectoryAdapter,
    codebase_memory: &mut dyn TrajectoryAdapter,
    bounded_files: &mut dyn TrajectoryAdapter,
    tokenizer: &dyn TrajectoryTokenizer,
) -> Result<TrajectoryEvidencePackage, TrajectoryError> {
    protocol.validate()?;
    let digests = protocol.digests()?;
    if rootlight.condition() != TrajectoryCondition::Rootlight
        || codebase_memory.condition() != TrajectoryCondition::CodebaseMemory
        || bounded_files.condition() != TrajectoryCondition::BoundedFileExploration
    {
        return Err(TrajectoryError::AdapterConditionMismatch);
    }

    let mut attempts = Vec::new();
    for workflow in &protocol.workflows {
        let task_sha256 = protocol.task_digest(&workflow.workflow_id)?;
        for (attempt_index, seed) in protocol.attempt_seeds.iter().copied().enumerate() {
            let attempt_index =
                u16::try_from(attempt_index).map_err(|_| TrajectoryError::CounterOverflow)?;
            attempts.push(execute_and_sanitize(
                &protocol,
                &digests,
                workflow,
                TrajectoryCondition::Rootlight,
                attempt_index,
                seed,
                rootlight,
                tokenizer,
                &task_sha256,
            )?);
            attempts.push(execute_and_sanitize(
                &protocol,
                &digests,
                workflow,
                TrajectoryCondition::CodebaseMemory,
                attempt_index,
                seed,
                codebase_memory,
                tokenizer,
                &task_sha256,
            )?);
            attempts.push(execute_and_sanitize(
                &protocol,
                &digests,
                workflow,
                TrajectoryCondition::BoundedFileExploration,
                attempt_index,
                seed,
                bounded_files,
                tokenizer,
                &task_sha256,
            )?);
        }
    }

    let denominator = summarize_attempts(&protocol, &attempts)?;
    let spot_review_attempt_ids = spot_review_selection(&attempts);
    let package = TrajectoryEvidencePackage {
        schema: TRAJECTORY_PROTOCOL_SCHEMA_VERSION.to_owned(),
        protocol,
        digests,
        attempts,
        denominator,
        spot_review_attempt_ids,
    };
    package.validate()?;
    Ok(package)
}

#[allow(clippy::too_many_arguments)]
fn execute_and_sanitize(
    protocol: &TrajectoryProtocol,
    digests: &TrajectoryProtocolDigests,
    workflow: &TrajectoryWorkflowProtocol,
    condition: TrajectoryCondition,
    attempt_index: u16,
    seed: u64,
    adapter: &mut dyn TrajectoryAdapter,
    tokenizer: &dyn TrajectoryTokenizer,
    task_sha256: &str,
) -> Result<TrajectoryAttemptRecord, TrajectoryError> {
    debug_assert_eq!(adapter.condition(), condition);
    let execution_boundary = adapter.execution_boundary();
    let raw = adapter.execute(TrajectoryExecutionInput {
        workflow,
        task_sha256,
        fixture_sha256: &protocol.fixture_sha256,
        attempt_index,
        seed,
        bounds: protocol.bounds,
        stopping: protocol.stopping,
        retry: &protocol.retry,
    });
    sanitize_attempt(
        protocol,
        digests,
        workflow,
        condition,
        execution_boundary,
        attempt_index,
        seed,
        raw,
        tokenizer,
    )
}

#[allow(clippy::too_many_arguments)]
fn sanitize_attempt(
    protocol: &TrajectoryProtocol,
    digests: &TrajectoryProtocolDigests,
    workflow: &TrajectoryWorkflowProtocol,
    condition: TrajectoryCondition,
    execution_boundary: TrajectoryExecutionBoundary,
    attempt_index: u16,
    seed: u64,
    raw: RawTrajectoryAttempt,
    tokenizer: &dyn TrajectoryTokenizer,
) -> Result<TrajectoryAttemptRecord, TrajectoryError> {
    if raw.calls.is_empty()
        || raw.calls.len() > usize::try_from(protocol.bounds.tool_calls).unwrap_or(usize::MAX)
    {
        return Err(TrajectoryError::InvalidAttempt);
    }
    let tokenizer_identity = tokenizer.identity();
    validate_tokenizer_identity_fields(Some(&tokenizer_identity))?;
    let mut calls = Vec::with_capacity(raw.calls.len());
    let mut elapsed_ns = 0_u64;
    let mut result_items = 0_u64;
    let mut source_bytes = 0_u64;
    let mut actual_tokens = 0_u64;
    let mut retry_count = 0_u32;
    let mut claim_signals = TrajectoryClaimSignals::default();
    let mut redundant_status_preflights = 0_u32;
    for (index, raw_call) in raw.calls.into_iter().enumerate() {
        validate_label(&raw_call.operation_id)?;
        validate_label(&raw_call.tool.tool_id)?;
        validate_label(&raw_call.tool.tool_version)?;
        let request_actual = tokenizer.count(&raw_call.request_frame)?;
        let response_actual = tokenizer.count(&raw_call.response_frame)?;
        let source_actual = tokenizer.count(&raw_call.source_frame)?;
        let accounting = WorkflowTokenAccounting::new(
            Some(tokenizer_identity.clone()),
            TokenMeasurement::from_input(
                TokenInputKind::Request,
                &raw_call.request_frame,
                deterministic_token_estimate(raw_call.request_frame.len()),
                Some(request_actual),
                "none",
                "exact_request_frame",
            ),
            TokenMeasurement::from_input(
                TokenInputKind::Response,
                &raw_call.response_frame,
                deterministic_token_estimate(raw_call.response_frame.len()),
                Some(response_actual),
                "none",
                "exact_response_frame",
            ),
            TokenMeasurement::from_input(
                TokenInputKind::Source,
                &raw_call.source_frame,
                deterministic_token_estimate(raw_call.source_frame.len()),
                Some(source_actual),
                "none",
                "source_attribution_within_response",
            ),
        )?;
        if accounting.total.actual_tokens.unwrap_or(u64::MAX) > protocol.bounds.total_tokens
            || accounting.source.serialized_bytes > protocol.bounds.source_bytes
            || raw_call.result_items > protocol.bounds.result_items
            || raw_call.elapsed_ns > protocol.bounds.elapsed_ns
        {
            return Err(TrajectoryError::AttemptLimitExceeded);
        }
        elapsed_ns = elapsed_ns
            .checked_add(raw_call.elapsed_ns)
            .ok_or(TrajectoryError::CounterOverflow)?;
        result_items = result_items
            .checked_add(raw_call.result_items)
            .ok_or(TrajectoryError::CounterOverflow)?;
        source_bytes = source_bytes
            .checked_add(accounting.source.serialized_bytes)
            .ok_or(TrajectoryError::CounterOverflow)?;
        actual_tokens = actual_tokens
            .checked_add(
                accounting
                    .total
                    .actual_tokens
                    .ok_or(TrajectoryError::MissingActualTokens)?,
            )
            .ok_or(TrajectoryError::CounterOverflow)?;
        if elapsed_ns > protocol.bounds.elapsed_ns
            || result_items > protocol.bounds.result_items
            || source_bytes > protocol.bounds.source_bytes
            || actual_tokens > protocol.bounds.total_tokens
        {
            return Err(TrajectoryError::AttemptLimitExceeded);
        }
        retry_count = retry_count
            .checked_add(u32::from(raw_call.retry_ordinal > 0))
            .ok_or(TrajectoryError::CounterOverflow)?;
        claim_signals = claim_signals.checked_add(raw_call.claim_signals)?;
        if raw_call.tool.tool_id == "repo.status" && !workflow.allows_status_preflight {
            redundant_status_preflights = redundant_status_preflights
                .checked_add(1)
                .ok_or(TrajectoryError::CounterOverflow)?;
        }
        calls.push(TrajectoryCallRecord {
            call_index: u32::try_from(index).map_err(|_| TrajectoryError::CounterOverflow)?,
            operation_id: raw_call.operation_id,
            retry_ordinal: raw_call.retry_ordinal,
            tool: raw_call.tool,
            exposure_profile: raw_call.exposure_profile,
            operation_status: raw_call.operation_status,
            accounting,
            elapsed_ns: raw_call.elapsed_ns,
            result_items: raw_call.result_items,
            truncated: raw_call.truncated,
            continuation_available: raw_call.continuation_available,
            claim_signals: raw_call.claim_signals,
        });
    }
    validate_terminal_outcome(&raw.outcome, &calls)?;
    let attempt_id = format!(
        "{}-{}-{attempt_index:02}",
        workflow.workflow_id,
        condition.label()
    );
    let token_artifact_id = format!("token-report-{attempt_id}");
    let token_sha256 = digest_json(
        "rootlight.trajectory.token-report.v1",
        &calls
            .iter()
            .map(|call| &call.accounting)
            .collect::<Vec<_>>(),
    )?;
    let environment_artifact_id = format!("environment-{}", condition.label());
    let evidence = vec![
        TrajectoryEvidenceReference {
            kind: TrajectoryEvidenceKind::TokenizerReport,
            artifact_id: token_artifact_id,
            sha256: token_sha256,
        },
        TrajectoryEvidenceReference {
            kind: TrajectoryEvidenceKind::EnvironmentManifest,
            artifact_id: environment_artifact_id,
            sha256: protocol.condition_digest(condition)?,
        },
    ];
    Ok(TrajectoryAttemptRecord {
        attempt_id,
        experiment_id: protocol.experiment_id.clone(),
        protocol_sha256: digests.protocol_sha256.clone(),
        workflow_id: workflow.workflow_id.clone(),
        task_sha256: protocol.task_digest(&workflow.workflow_id)?,
        fixture_sha256: digests.fixture_sha256.clone(),
        runner_sha256: digests.runner_sha256.clone(),
        condition_sha256: protocol.condition_digest(condition)?,
        condition,
        execution_boundary,
        attempt_index,
        deterministic_seed: seed,
        outcome: raw.outcome,
        calls,
        elapsed_ns,
        retry_count,
        redundant_status_preflights,
        claim_signals,
        evidence,
    })
}

fn validate_terminal_outcome(
    outcome: &TrajectoryAttemptOutcome,
    calls: &[impl CallStatus],
) -> Result<(), TrajectoryError> {
    let last = calls.last().ok_or(TrajectoryError::InvalidAttempt)?;
    let status = last.operation_status();
    let valid = match (outcome, status) {
        (TrajectoryAttemptOutcome::Succeeded, TrajectoryOperationStatus::Succeeded) => true,
        (
            TrajectoryAttemptOutcome::Failed { error_code: left },
            TrajectoryOperationStatus::Failed { error_code: right },
        )
        | (
            TrajectoryAttemptOutcome::Unsupported { error_code: left },
            TrajectoryOperationStatus::Failed { error_code: right },
        )
        | (
            TrajectoryAttemptOutcome::NotAvailable { error_code: left },
            TrajectoryOperationStatus::Failed { error_code: right },
        )
        | (
            TrajectoryAttemptOutcome::Excluded { reason_code: left },
            TrajectoryOperationStatus::Failed { error_code: right },
        )
        | (
            TrajectoryAttemptOutcome::TimedOut { error_code: left },
            TrajectoryOperationStatus::TimedOut { error_code: right },
        )
        | (
            TrajectoryAttemptOutcome::Cancelled { error_code: left },
            TrajectoryOperationStatus::Cancelled { error_code: right },
        ) => left == right,
        _ => false,
    };
    if !valid {
        return Err(TrajectoryError::OutcomeMismatch);
    }
    match outcome {
        TrajectoryAttemptOutcome::Failed { error_code }
        | TrajectoryAttemptOutcome::TimedOut { error_code }
        | TrajectoryAttemptOutcome::Cancelled { error_code }
        | TrajectoryAttemptOutcome::Unsupported { error_code }
        | TrajectoryAttemptOutcome::NotAvailable { error_code } => validate_label(error_code)?,
        TrajectoryAttemptOutcome::Excluded { reason_code } => validate_label(reason_code)?,
        TrajectoryAttemptOutcome::Succeeded => {}
    }
    Ok(())
}

fn retryable_status(status: &TrajectoryOperationStatus, retryable_codes: &[String]) -> bool {
    let error_code = match status {
        TrajectoryOperationStatus::Failed { error_code }
        | TrajectoryOperationStatus::TimedOut { error_code } => error_code,
        TrajectoryOperationStatus::Succeeded | TrajectoryOperationStatus::Cancelled { .. } => {
            return false;
        }
    };
    retryable_codes.binary_search(error_code).is_ok()
}

trait CallStatus {
    fn operation_status(&self) -> &TrajectoryOperationStatus;
}

impl CallStatus for TrajectoryCallRecord {
    fn operation_status(&self) -> &TrajectoryOperationStatus {
        &self.operation_status
    }
}

impl CallStatus for RawTrajectoryCall {
    fn operation_status(&self) -> &TrajectoryOperationStatus {
        &self.operation_status
    }
}

/// Adapter that truthfully retains executable absence in the denominator.
#[derive(Debug, Clone)]
pub struct UnavailableTrajectoryAdapter {
    condition: TrajectoryCondition,
    adapter_id: String,
    reason_code: String,
}

impl UnavailableTrajectoryAdapter {
    /// Creates a source-free unavailable condition adapter.
    ///
    /// # Errors
    ///
    /// Returns [`TrajectoryError`] if labels are malformed or Rootlight is
    /// selected as optional unavailable.
    pub fn new(
        condition: TrajectoryCondition,
        adapter_id: impl Into<String>,
        reason_code: impl Into<String>,
    ) -> Result<Self, TrajectoryError> {
        let adapter_id = adapter_id.into();
        let reason_code = reason_code.into();
        validate_label(&adapter_id)?;
        validate_label(&reason_code)?;
        if condition == TrajectoryCondition::Rootlight {
            return Err(TrajectoryError::InvalidProtocol);
        }
        Ok(Self {
            condition,
            adapter_id,
            reason_code,
        })
    }
}

impl TrajectoryAdapter for UnavailableTrajectoryAdapter {
    fn condition(&self) -> TrajectoryCondition {
        self.condition
    }

    fn execution_boundary(&self) -> TrajectoryExecutionBoundary {
        TrajectoryExecutionBoundary::UnavailableAdapter
    }

    fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt {
        let request_frame = serde_json::to_vec(&json!({
            "workflow_id": input.workflow.workflow_id,
            "task_sha256": input.task_sha256,
            "fixture_sha256": input.fixture_sha256,
            "attempt_index": input.attempt_index,
            "seed": input.seed,
        }))
        .unwrap_or_else(|_| b"{\"status\":\"serialization_failed\"}".to_vec());
        let response_frame = serde_json::to_vec(&json!({
            "status": "not_available",
            "reason_code": self.reason_code,
        }))
        .unwrap_or_else(|_| b"{\"status\":\"serialization_failed\"}".to_vec());
        let error_code = self.reason_code.clone();
        RawTrajectoryAttempt {
            outcome: TrajectoryAttemptOutcome::NotAvailable {
                error_code: error_code.clone(),
            },
            calls: vec![RawTrajectoryCall {
                operation_id: "adapter_availability".to_owned(),
                tool: TrajectoryToolIdentity {
                    tool_id: self.adapter_id.clone(),
                    tool_version: "v1".to_owned(),
                },
                exposure_profile: TrajectoryExposureProfile::Analysis,
                operation_status: TrajectoryOperationStatus::Failed { error_code },
                retry_ordinal: 0,
                request_frame,
                response_frame,
                source_frame: Vec::new(),
                elapsed_ns: 0,
                result_items: 0,
                truncated: false,
                continuation_available: false,
                claim_signals: TrajectoryClaimSignals::default(),
            }],
        }
    }
}

/// Deterministic bounded regular-file exploration baseline.
#[derive(Debug, Clone)]
pub struct BoundedFileExplorationAdapter {
    root: PathBuf,
    observations: Vec<BoundedFileObservation>,
}

/// Ephemeral task-driven baseline observation available to benchmark graders.
///
/// This type intentionally has no serialization implementation. Callers must
/// consume its source-bearing fields before publishing source-free evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedFileObservation {
    /// Workflow identity from the execution input.
    pub workflow_id: String,
    /// Digest of the exact trajectory task.
    pub task_sha256: String,
    /// Digest of the exact fixture tree.
    pub fixture_sha256: String,
    /// Zero-based attempt index.
    pub attempt_index: u16,
    /// Deterministic seed used for target and tie-break selection.
    pub deterministic_seed: u64,
    /// Digest of the exact prompt shared with the Rootlight candidate.
    pub prompt_sha256: String,
    /// Task-selected root-relative regular-file paths.
    pub selected_paths: Vec<String>,
    /// Source-bearing response consumed only by the benchmark grader.
    pub response: serde_json::Value,
    /// Exact selected source bytes consumed only by the benchmark grader.
    pub source_frame: Vec<u8>,
}

impl BoundedFileExplorationAdapter {
    /// Creates the local baseline over a fixture root.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            observations: Vec::new(),
        }
    }

    /// Removes and returns source-bearing observations for immediate grading.
    #[must_use]
    pub fn take_observations(&mut self) -> Vec<BoundedFileObservation> {
        std::mem::take(&mut self.observations)
    }
}

impl TrajectoryAdapter for BoundedFileExplorationAdapter {
    fn condition(&self) -> TrajectoryCondition {
        TrajectoryCondition::BoundedFileExploration
    }

    fn execution_boundary(&self) -> TrajectoryExecutionBoundary {
        TrajectoryExecutionBoundary::LocalBoundedFiles
    }

    fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt {
        let started = Instant::now();
        let prompt = trajectory_task_prompt(input.workflow.family, input.seed);
        let prompt_sha256 = sha256_hex(prompt.as_bytes());
        let request_frame = serde_json::to_vec(&json!({
            "workflow_id": input.workflow.workflow_id,
            "task_sha256": input.task_sha256,
            "fixture_sha256": input.fixture_sha256,
            "attempt_index": input.attempt_index,
            "seed": input.seed,
            "prompt_sha256": prompt_sha256,
            "bounds": input.bounds,
        }))
        .unwrap_or_else(|_| b"{\"status\":\"serialization_failed\"}".to_vec());
        match collect_bounded_source(
            &self.root,
            input.workflow.family,
            input.seed,
            &prompt,
            input.bounds,
        ) {
            Ok(selection) => {
                let source_frame = selection.source_frame;
                let result_items = u64::try_from(selection.paths.len()).unwrap_or(u64::MAX);
                let truncated = selection.truncated;
                let aggregate_sha256 = sha256_hex(&source_frame);
                let response = json!({
                    "status": "succeeded",
                    "workflow_id": input.workflow.workflow_id,
                    "task_sha256": input.task_sha256,
                    "prompt_sha256": prompt_sha256,
                    "result_items": result_items,
                    "source_sha256": aggregate_sha256,
                    "source": String::from_utf8_lossy(&source_frame),
                    "source_references": selection.paths,
                    "truncated": truncated,
                });
                let response_frame = serde_json::to_vec(&response)
                    .unwrap_or_else(|_| b"{\"status\":\"serialization_failed\"}".to_vec());
                self.observations.push(BoundedFileObservation {
                    workflow_id: input.workflow.workflow_id.clone(),
                    task_sha256: input.task_sha256.to_owned(),
                    fixture_sha256: input.fixture_sha256.to_owned(),
                    attempt_index: input.attempt_index,
                    deterministic_seed: input.seed,
                    prompt_sha256,
                    selected_paths: selection.paths,
                    response,
                    source_frame: source_frame.clone(),
                });
                RawTrajectoryAttempt {
                    outcome: TrajectoryAttemptOutcome::Succeeded,
                    calls: vec![RawTrajectoryCall {
                        operation_id: "bounded_file_exploration".to_owned(),
                        tool: TrajectoryToolIdentity {
                            tool_id: "bounded_file.explore".to_owned(),
                            tool_version: "v1".to_owned(),
                        },
                        exposure_profile: TrajectoryExposureProfile::Analysis,
                        operation_status: TrajectoryOperationStatus::Succeeded,
                        retry_ordinal: 0,
                        request_frame,
                        response_frame,
                        source_frame,
                        elapsed_ns: elapsed_nanos(started),
                        result_items,
                        truncated,
                        continuation_available: false,
                        claim_signals: TrajectoryClaimSignals::default(),
                    }],
                }
            }
            Err(error_code) => {
                let response_frame = serde_json::to_vec(&json!({
                    "status": "failed",
                    "error_code": error_code,
                }))
                .unwrap_or_else(|_| b"{\"status\":\"serialization_failed\"}".to_vec());
                RawTrajectoryAttempt {
                    outcome: TrajectoryAttemptOutcome::Failed {
                        error_code: error_code.clone(),
                    },
                    calls: vec![RawTrajectoryCall {
                        operation_id: "bounded_file_exploration".to_owned(),
                        tool: TrajectoryToolIdentity {
                            tool_id: "bounded_file.explore".to_owned(),
                            tool_version: "v1".to_owned(),
                        },
                        exposure_profile: TrajectoryExposureProfile::Analysis,
                        operation_status: TrajectoryOperationStatus::Failed { error_code },
                        retry_ordinal: 0,
                        request_frame,
                        response_frame,
                        source_frame: Vec::new(),
                        elapsed_ns: elapsed_nanos(started),
                        result_items: 0,
                        truncated: false,
                        continuation_available: false,
                        claim_signals: TrajectoryClaimSignals::default(),
                    }],
                }
            }
        }
    }
}

struct BoundedSourceSelection {
    paths: Vec<String>,
    source_frame: Vec<u8>,
    truncated: bool,
}

fn collect_bounded_source(
    root: &Path,
    family: TrajectoryWorkflowFamily,
    seed: u64,
    prompt: &str,
    bounds: TrajectorySharedBounds,
) -> Result<BoundedSourceSelection, String> {
    let metadata = fs::symlink_metadata(root).map_err(|_| "fixture_unavailable".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("fixture_unavailable".to_owned());
    }
    let mut pending = vec![root.to_owned()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|_| "fixture_unavailable".to_owned())?;
        for entry in entries {
            let entry = entry.map_err(|_| "fixture_unavailable".to_owned())?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| "fixture_unavailable".to_owned())?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                files.push(path);
            }
        }
    }
    let prompt_terms = prompt
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|term| term.len() >= 4)
        .map(str::to_ascii_lowercase)
        .collect::<BTreeSet<_>>();
    let mut ranked = Vec::new();
    let discovery_truncated = files.len() > 512;
    for path in files.into_iter().take(512) {
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "fixture_unavailable".to_owned())?
            .to_string_lossy()
            .replace('\\', "/");
        let metadata = fs::metadata(&path).map_err(|_| "fixture_unavailable".to_owned())?;
        let mut preview = Vec::new();
        fs::File::open(&path)
            .map_err(|_| "fixture_unavailable".to_owned())?
            .take(metadata.len().min(64 * 1024))
            .read_to_end(&mut preview)
            .map_err(|_| "fixture_unavailable".to_owned())?;
        let Ok(preview) = std::str::from_utf8(&preview) else {
            continue;
        };
        let score = baseline_file_score(family, &relative, preview, &prompt_terms);
        let mut tie_hasher = Sha256::new();
        tie_hasher.update(b"rootlight.bounded-file-selection.v1");
        tie_hasher.update(seed.to_le_bytes());
        tie_hasher.update(relative.as_bytes());
        ranked.push((
            std::cmp::Reverse(score),
            tie_hasher.finalize(),
            path,
            relative,
        ));
    }
    ranked.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| left.1.as_slice().cmp(right.1.as_slice()))
            .then_with(|| left.3.cmp(&right.3))
    });
    let selected_file_limit = match family {
        TrajectoryWorkflowFamily::ArchitectureOverview
        | TrajectoryWorkflowFamily::CycleInvestigation
        | TrajectoryWorkflowFamily::CrossServiceTrace
        | TrajectoryWorkflowFamily::MultiRepositoryMigration => 3,
        TrajectoryWorkflowFamily::AssessChangeImpact
        | TrajectoryWorkflowFamily::SelectTests
        | TrajectoryWorkflowFamily::BugFixContext
        | TrajectoryWorkflowFamily::RefactoringBoundary
        | TrajectoryWorkflowFamily::HistoryComparison
        | TrajectoryWorkflowFamily::ApiMigrationBatch => 2,
        TrajectoryWorkflowFamily::LocateImplementation
        | TrajectoryWorkflowFamily::ExplainSymbol
        | TrajectoryWorkflowFamily::CallRelationships
        | TrajectoryWorkflowFamily::DeadCodeInvestigation => 1,
    };
    let mut source = Vec::new();
    let mut paths = Vec::new();
    let mut truncated = discovery_truncated || ranked.len() > selected_file_limit;
    let source_limit = usize::try_from(bounds.source_bytes).unwrap_or(usize::MAX);
    for (_, _, path, relative) in ranked.into_iter().take(selected_file_limit) {
        if u64::try_from(paths.len()).unwrap_or(u64::MAX) >= bounds.result_items
            || source.len() >= source_limit
        {
            truncated = true;
            break;
        }
        let remaining = source_limit.saturating_sub(source.len());
        let header = format!("--- {relative} ---\n");
        let content_limit = remaining.saturating_sub(header.len().saturating_add(1));
        if content_limit == 0 {
            truncated = true;
            break;
        }
        let file_length = fs::metadata(&path)
            .map_err(|_| "fixture_unavailable".to_owned())?
            .len();
        let mut file = fs::File::open(&path).map_err(|_| "fixture_unavailable".to_owned())?;
        let mut bytes = Vec::with_capacity(content_limit.min(8 * 1024));
        file.by_ref()
            .take(u64::try_from(content_limit).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|_| "fixture_unavailable".to_owned())?;
        match std::str::from_utf8(&bytes) {
            Ok(_) => {}
            Err(error) if error.error_len().is_none() => bytes.truncate(error.valid_up_to()),
            Err(_) => continue,
        }
        source.extend_from_slice(header.as_bytes());
        source.extend_from_slice(&bytes);
        source.push(b'\n');
        paths.push(relative);
        truncated |= u64::try_from(bytes.len()).unwrap_or(u64::MAX) < file_length;
    }
    source.truncate(source_limit);
    Ok(BoundedSourceSelection {
        paths,
        source_frame: source,
        truncated,
    })
}

fn baseline_file_score(
    family: TrajectoryWorkflowFamily,
    relative_path: &str,
    content: &str,
    prompt_terms: &BTreeSet<String>,
) -> u64 {
    let path = relative_path.to_ascii_lowercase();
    let content = content.to_ascii_lowercase();
    let term_score = prompt_terms.iter().fold(0_u64, |score, term| {
        score.saturating_add(u64::from(content.contains(term)) * 20)
    });
    let role_score = match family {
        TrajectoryWorkflowFamily::SelectTests if path.contains("test") => 120,
        TrajectoryWorkflowFamily::ArchitectureOverview
        | TrajectoryWorkflowFamily::CycleInvestigation
            if path == "cargo.toml" =>
        {
            100
        }
        TrajectoryWorkflowFamily::ArchitectureOverview
        | TrajectoryWorkflowFamily::CycleInvestigation
            if path.starts_with("src/") =>
        {
            80
        }
        TrajectoryWorkflowFamily::AssessChangeImpact
        | TrajectoryWorkflowFamily::BugFixContext
        | TrajectoryWorkflowFamily::RefactoringBoundary
        | TrajectoryWorkflowFamily::ApiMigrationBatch
            if path.contains("test") =>
        {
            70
        }
        TrajectoryWorkflowFamily::MultiRepositoryMigration
            if path.contains("consumer-service")
                || content.contains("rootlight_budget_runtime_fixture") =>
        {
            180
        }
        TrajectoryWorkflowFamily::CrossServiceTrace
            if content.contains("submit_budget_request")
                || content.contains("handle_budget_message") =>
        {
            160
        }
        TrajectoryWorkflowFamily::LocateImplementation
        | TrajectoryWorkflowFamily::ExplainSymbol
        | TrajectoryWorkflowFamily::CallRelationships
        | TrajectoryWorkflowFamily::DeadCodeInvestigation
        | TrajectoryWorkflowFamily::HistoryComparison
        | TrajectoryWorkflowFamily::BugFixContext
        | TrajectoryWorkflowFamily::AssessChangeImpact
        | TrajectoryWorkflowFamily::SelectTests
        | TrajectoryWorkflowFamily::RefactoringBoundary
        | TrajectoryWorkflowFamily::ApiMigrationBatch
            if path == "src/lib.rs" =>
        {
            100
        }
        _ if path.ends_with(".rs") => 20,
        _ => 0,
    };
    term_score.saturating_add(role_score)
}

/// Returns the exact deterministic prompt shared by compared candidates.
#[must_use]
pub fn trajectory_task_prompt(family: TrajectoryWorkflowFamily, seed: u64) -> String {
    let (primary, secondary) = if seed % 5 >= 3 {
        ("budget_helper", "budget_entry")
    } else {
        ("budget_entry", "budget_helper")
    };
    match family {
        TrajectoryWorkflowFamily::LocateImplementation => {
            format!("locate the implementation of the concept {primary} and explain the exact symbol")
        }
        TrajectoryWorkflowFamily::ExplainSymbol => {
            format!("explain the unfamiliar symbol {primary}, including its definition and evidence")
        }
        TrajectoryWorkflowFamily::CallRelationships => {
            format!("find the exact callers and callees that connect {primary} with {secondary}")
        }
        TrajectoryWorkflowFamily::BugFixContext => {
            format!("prepare the minimal context needed to fix {primary} without breaking {secondary}")
        }
        TrajectoryWorkflowFamily::AssessChangeImpact => {
            format!("assess the impact of changing {primary}, select tests, and produce a safe change plan")
        }
        TrajectoryWorkflowFamily::SelectTests => {
            format!("select the exact tests required after editing {primary} and explain each selection")
        }
        TrajectoryWorkflowFamily::ArchitectureOverview => {
            "build a repository architecture overview with concrete components and dependency edges"
                .to_owned()
        }
        TrajectoryWorkflowFamily::CycleInvestigation => {
            "find the cycle between cycle_alpha and cycle_beta, trace it, and plan a break point"
                .to_owned()
        }
        TrajectoryWorkflowFamily::DeadCodeInvestigation => {
            "identify budget_unused as a dead-code candidate and explain its exact definition"
                .to_owned()
        }
        TrajectoryWorkflowFamily::CrossServiceTrace => {
            "trace submit_budget_request through handle_budget_message to transform and pack the evidence"
                .to_owned()
        }
        TrajectoryWorkflowFamily::RefactoringBoundary => {
            format!("prepare a refactoring boundary around {primary} using relationships, impact, context, and a plan")
        }
        TrajectoryWorkflowFamily::HistoryComparison => {
            "compare the two indexed states and assess the impact of the added trajectory_added API"
                .to_owned()
        }
        TrajectoryWorkflowFamily::ApiMigrationBatch => {
            format!("create an API migration plan for {primary} with one dependent locate-impact-plan batch")
        }
        TrajectoryWorkflowFamily::MultiRepositoryMigration => {
            "coordinate the migrate_budget_api migration across runtime-service and consumer-service with cross-repository evidence"
                .to_owned()
        }
    }
}

/// Builds the frozen 14-workflow comparison protocol for an exact fixture.
///
/// # Errors
///
/// Returns [`TrajectoryError`] if the fixture digest is malformed or the
/// embedded protocol violates its own completeness contract.
pub fn preregistered_trajectory_protocol(
    fixture_sha256: impl Into<String>,
) -> Result<TrajectoryProtocol, TrajectoryError> {
    let fixture_sha256 = fixture_sha256.into();
    validate_sha256(&fixture_sha256)?;
    let workflows = vec![
        workflow(
            "locate-implementation",
            TrajectoryWorkflowFamily::LocateImplementation,
            &["definition_evidence", "implementation_identity"],
            &["code.locate", "symbol.explain"],
        ),
        workflow(
            "explain-symbol",
            TrajectoryWorkflowFamily::ExplainSymbol,
            &["definition_evidence", "symbol_identity"],
            &["symbol.explain"],
        ),
        workflow(
            "callers-callees",
            TrajectoryWorkflowFamily::CallRelationships,
            &["caller_identity", "callee_identity", "relation_evidence"],
            &["symbol.relationships"],
        ),
        workflow(
            "bug-fix-context",
            TrajectoryWorkflowFamily::BugFixContext,
            &["context_roles", "implementation_identity", "test_identity"],
            &["code.locate", "context.pack"],
        ),
        workflow(
            "change-impact",
            TrajectoryWorkflowFamily::AssessChangeImpact,
            &["impact_edge", "plan_target", "test_identity"],
            &["change.impact", "tests.select", "plan.change"],
        ),
        workflow(
            "select-tests",
            TrajectoryWorkflowFamily::SelectTests,
            &["selection_rationale", "test_identity"],
            &["tests.select"],
        ),
        workflow(
            "architecture-overview",
            TrajectoryWorkflowFamily::ArchitectureOverview,
            &["component_identity", "dependency_edge"],
            &["architecture.overview"],
        ),
        workflow(
            "cyclic-dependencies",
            TrajectoryWorkflowFamily::CycleInvestigation,
            &["break_plan", "cycle_identity", "witness_path"],
            &["architecture.cycles", "flow.trace", "plan.change"],
        ),
        workflow(
            "dead-code",
            TrajectoryWorkflowFamily::DeadCodeInvestigation,
            &[
                "candidate_identity",
                "definition_evidence",
                "reachability_reason",
            ],
            &["code.dead", "symbol.explain"],
        ),
        workflow(
            "cross-service-trace",
            TrajectoryWorkflowFamily::CrossServiceTrace,
            &["context_roles", "cross_service_path", "route_identity"],
            &["code.locate", "flow.trace", "context.pack"],
        ),
        workflow(
            "refactoring-boundary",
            TrajectoryWorkflowFamily::RefactoringBoundary,
            &[
                "context_roles",
                "impact_edge",
                "plan_target",
                "relation_evidence",
            ],
            &[
                "symbol.relationships",
                "change.impact",
                "context.pack",
                "plan.change",
            ],
        ),
        workflow(
            "history-comparison",
            TrajectoryWorkflowFamily::HistoryComparison,
            &["change_identity", "generation_identity", "impact_edge"],
            &["history.compare", "change.impact"],
        ),
        workflow(
            "api-migration-batch",
            TrajectoryWorkflowFamily::ApiMigrationBatch,
            &[
                "impact_edge",
                "operation_outcome",
                "ordered_result",
                "plan_target",
            ],
            &["query.batch"],
        ),
        workflow(
            "multi-repository-migration",
            TrajectoryWorkflowFamily::MultiRepositoryMigration,
            &[
                "context_per_repository",
                "cross_repository_path",
                "repository_identity",
            ],
            &[
                "repo.list",
                "code.locate",
                "code.locate",
                "flow.trace",
                "change.impact",
                "context.pack",
                "context.pack",
            ],
        ),
    ];
    let protocol = TrajectoryProtocol {
        schema: TRAJECTORY_PROTOCOL_SCHEMA_VERSION.to_owned(),
        experiment_id: "agent-workflow-comparison-v2".to_owned(),
        fixture_id: "cross-service-multi-repository-v2".to_owned(),
        fixture_sha256,
        runner_id: TRAJECTORY_RUNNER_ID.to_owned(),
        attempt_seeds: vec![17, 43],
        bounds: TrajectorySharedBounds::default(),
        stopping: TrajectoryStoppingPolicy::default(),
        retry: TrajectoryRetryPolicy::default(),
        allowed_exclusion_reasons: vec![
            "environment_incompatible".to_owned(),
            "fixture_unavailable".to_owned(),
            "preregistered_platform_exclusion".to_owned(),
        ],
        conditions: vec![
            TrajectoryConditionProtocol {
                condition: TrajectoryCondition::Rootlight,
                adapter_id: "rootlight_daemon_mcp_v1".to_owned(),
                boundary: TrajectoryExecutionBoundary::DaemonMcpProcess,
                availability: AdapterAvailabilityPolicy::Required,
                tool_access: vec![
                    "architecture.cycles".to_owned(),
                    "architecture.overview".to_owned(),
                    "change.impact".to_owned(),
                    "code.dead".to_owned(),
                    "code.locate".to_owned(),
                    "context.pack".to_owned(),
                    "flow.trace".to_owned(),
                    "history.compare".to_owned(),
                    "plan.change".to_owned(),
                    "query.advanced".to_owned(),
                    "query.batch".to_owned(),
                    "repo.list".to_owned(),
                    "source.read".to_owned(),
                    "symbol.explain".to_owned(),
                    "symbol.relationships".to_owned(),
                    "tests.select".to_owned(),
                ],
            },
            TrajectoryConditionProtocol {
                condition: TrajectoryCondition::CodebaseMemory,
                adapter_id: "codebase_memory_process_v1".to_owned(),
                boundary: TrajectoryExecutionBoundary::ExternalBaselineProcess,
                availability: AdapterAvailabilityPolicy::OptionalExecutable,
                tool_access: vec!["codebase_memory.query".to_owned()],
            },
            TrajectoryConditionProtocol {
                condition: TrajectoryCondition::BoundedFileExploration,
                adapter_id: "bounded_file_exploration_v1".to_owned(),
                boundary: TrajectoryExecutionBoundary::LocalBoundedFiles,
                availability: AdapterAvailabilityPolicy::Required,
                tool_access: vec!["bounded_file.explore".to_owned()],
            },
        ],
        workflows,
    };
    protocol.validate()?;
    Ok(protocol)
}

fn workflow(
    workflow_id: &str,
    family: TrajectoryWorkflowFamily,
    expected_evidence: &[&str],
    rootlight_tools: &[&str],
) -> TrajectoryWorkflowProtocol {
    let mut expected_evidence = expected_evidence
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    expected_evidence.sort();
    let rootlight_tools = rootlight_tools
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    TrajectoryWorkflowProtocol {
        workflow_id: workflow_id.to_owned(),
        family,
        task_id: workflow_id.replacen("workflow", "task", 1),
        expected_evidence,
        rootlight_tools,
        allows_status_preflight: false,
    }
}

fn summarize_attempts(
    protocol: &TrajectoryProtocol,
    attempts: &[TrajectoryAttemptRecord],
) -> Result<TrajectoryDenominator, TrajectoryError> {
    let expected_attempts = protocol
        .workflows
        .len()
        .checked_mul(protocol.attempt_seeds.len())
        .and_then(|value| value.checked_mul(protocol.conditions.len()))
        .ok_or(TrajectoryError::CounterOverflow)?;
    let mut summary = TrajectoryDenominator {
        expected_attempts: u32::try_from(expected_attempts)
            .map_err(|_| TrajectoryError::CounterOverflow)?,
        observed_attempts: u32::try_from(attempts.len())
            .map_err(|_| TrajectoryError::CounterOverflow)?,
        ..TrajectoryDenominator::default()
    };
    for attempt in attempts {
        let counter = match attempt.outcome {
            TrajectoryAttemptOutcome::Succeeded => &mut summary.succeeded,
            TrajectoryAttemptOutcome::Failed { .. } => &mut summary.failed,
            TrajectoryAttemptOutcome::TimedOut { .. } => &mut summary.timed_out,
            TrajectoryAttemptOutcome::Cancelled { .. } => &mut summary.cancelled,
            TrajectoryAttemptOutcome::Unsupported { .. } => &mut summary.unsupported,
            TrajectoryAttemptOutcome::NotAvailable { .. } => &mut summary.not_available,
            TrajectoryAttemptOutcome::Excluded { .. } => &mut summary.excluded,
        };
        *counter = counter
            .checked_add(1)
            .ok_or(TrajectoryError::CounterOverflow)?;
        summary.retried = summary
            .retried
            .checked_add(u32::from(attempt.retry_count > 0))
            .ok_or(TrajectoryError::CounterOverflow)?;
        summary.calls = summary
            .calls
            .checked_add(
                u32::try_from(attempt.calls.len()).map_err(|_| TrajectoryError::CounterOverflow)?,
            )
            .ok_or(TrajectoryError::CounterOverflow)?;
        summary.redundant_status_preflights = summary
            .redundant_status_preflights
            .checked_add(attempt.redundant_status_preflights)
            .ok_or(TrajectoryError::CounterOverflow)?;
        summary.claim_signals = summary.claim_signals.checked_add(attempt.claim_signals)?;
    }
    Ok(summary)
}

fn expected_attempt_keys(
    protocol: &TrajectoryProtocol,
) -> Result<Vec<(String, u16, TrajectoryCondition)>, TrajectoryError> {
    let mut keys = Vec::new();
    for workflow in &protocol.workflows {
        for attempt_index in 0..protocol.attempt_seeds.len() {
            let attempt_index =
                u16::try_from(attempt_index).map_err(|_| TrajectoryError::CounterOverflow)?;
            for condition in TrajectoryCondition::ALL {
                keys.push((workflow.workflow_id.clone(), attempt_index, condition));
            }
        }
    }
    Ok(keys)
}

fn spot_review_selection(attempts: &[TrajectoryAttemptRecord]) -> Vec<String> {
    if attempts.is_empty() {
        return Vec::new();
    }
    let count = SPOT_REVIEW_COUNT.min(attempts.len());
    (0..count)
        .map(|index| {
            let offset = index.saturating_mul(attempts.len()) / count;
            attempts[offset].attempt_id.clone()
        })
        .collect()
}

fn validate_evidence(references: &[TrajectoryEvidenceReference]) -> Result<(), TrajectoryError> {
    if references.is_empty() {
        return Err(TrajectoryError::InvalidAttempt);
    }
    let mut prior = None;
    for reference in references {
        validate_label(&reference.artifact_id)?;
        validate_sha256(&reference.sha256)?;
        let key = (
            reference.kind,
            reference.artifact_id.as_str(),
            reference.sha256.as_str(),
        );
        if prior.is_some_and(|prior| prior >= key) {
            return Err(TrajectoryError::InvalidAttempt);
        }
        prior = Some(key);
    }
    Ok(())
}

fn validate_tokenizer_identity_fields(
    tokenizer: Option<&ActualTokenizerIdentity>,
) -> Result<(), TrajectoryError> {
    let tokenizer = tokenizer.ok_or(TrajectoryError::MissingActualTokens)?;
    for label in [
        tokenizer.provider.as_str(),
        tokenizer.model.as_str(),
        tokenizer.tokenizer.as_str(),
        tokenizer.implementation.as_str(),
    ] {
        validate_label(label)?;
    }
    if let Some(version) = &tokenizer.implementation_version {
        validate_label(version)?;
    }
    Ok(())
}

fn validate_sorted_labels(values: &[String]) -> Result<(), TrajectoryError> {
    if values.is_empty() || values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(TrajectoryError::InvalidProtocol);
    }
    for value in values {
        validate_label(value)?;
    }
    Ok(())
}

fn validate_label(value: &str) -> Result<(), TrajectoryError> {
    if value.is_empty()
        || value.len() > MAX_PROTOCOL_LABEL_BYTES
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        return Err(TrajectoryError::InvalidLabel);
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), TrajectoryError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(TrajectoryError::InvalidDigest);
    }
    Ok(())
}

fn digest_json<T: Serialize + ?Sized>(domain: &str, value: &T) -> Result<String, TrajectoryError> {
    let bytes = serde_json::to_vec(value).map_err(|_| TrajectoryError::Serialization)?;
    Ok(digest_bytes(domain, &bytes))
}

fn digest_bytes(domain: &str, bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(
        u64::try_from(domain.len())
            .unwrap_or(u64::MAX)
            .to_le_bytes(),
    );
    hasher.update(domain.as_bytes());
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn deterministic_token_estimate(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX).div_ceil(4)
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

fn privacy_scan_json(bytes: &[u8]) -> Result<(), TrajectoryError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TrajectoryError::InvalidUtf8)?;
    for forbidden in [
        "\"request_frame\"",
        "\"response_frame\"",
        "\"source_frame\"",
        "\"prompt\"",
        "\"completion\"",
        "\"raw_source\"",
        "file://",
        "\\\\users\\\\",
        "/home/",
        "authorization:",
        "bearer ",
        "api_key",
        "private_key",
    ] {
        if text.to_ascii_lowercase().contains(forbidden) {
            return Err(TrajectoryError::PrivacyViolation);
        }
    }
    Ok(())
}

/// Workflow protocol, runner, or evidence integrity failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TrajectoryError {
    /// Protocol or package schema is unsupported.
    #[error("unsupported trajectory schema")]
    UnsupportedSchema,
    /// Preregistered protocol is incomplete or noncanonical.
    #[error("trajectory protocol is invalid")]
    InvalidProtocol,
    /// Normalized identifier is invalid.
    #[error("trajectory label is invalid")]
    InvalidLabel,
    /// Digest is not canonical lowercase SHA-256.
    #[error("trajectory digest is invalid")]
    InvalidDigest,
    /// A retained digest does not match recomputed input.
    #[error("trajectory digest does not match recomputed input")]
    DigestMismatch,
    /// A workflow identifier is unknown.
    #[error("trajectory workflow is unknown")]
    UnknownWorkflow,
    /// Adapter was wired to the wrong condition.
    #[error("trajectory adapter condition does not match the protocol")]
    AdapterConditionMismatch,
    /// Attempt record is malformed.
    #[error("trajectory attempt is invalid")]
    InvalidAttempt,
    /// Attempt outcome and final call status disagree.
    #[error("trajectory outcome and terminal call status differ")]
    OutcomeMismatch,
    /// Retry ordinals or ceilings are invalid.
    #[error("trajectory retry sequence is invalid")]
    InvalidRetry,
    /// Attempt exceeds a preregistered hard limit.
    #[error("trajectory attempt exceeds its fixed bounds")]
    AttemptLimitExceeded,
    /// Actual tokenizer counts are absent.
    #[error("trajectory actual token counts are required")]
    MissingActualTokens,
    /// Tokenizer could not be initialized.
    #[error("trajectory tokenizer is unavailable")]
    TokenizerUnavailable,
    /// Tokenizer input is not UTF-8.
    #[error("trajectory tokenizer input is not valid UTF-8")]
    InvalidUtf8,
    /// Attempt counters do not reconcile.
    #[error("trajectory attempt accounting does not reconcile")]
    AttemptAccountingMismatch,
    /// Expected workflow × condition × seed rows are missing or duplicated.
    #[error("trajectory denominator is incomplete")]
    IncompleteDenominator,
    /// Stored denominator summary differs from retained attempts.
    #[error("trajectory denominator summary does not reconcile")]
    DenominatorMismatch,
    /// An exclusion reason was not preregistered.
    #[error("trajectory exclusion reason was not preregistered")]
    UnregisteredExclusion,
    /// Manual spot-review selection differs from the deterministic protocol.
    #[error("trajectory spot-review selection does not reconcile")]
    SpotReviewMismatch,
    /// Published shape contains a forbidden source or secret boundary.
    #[error("trajectory package violates the source-free privacy boundary")]
    PrivacyViolation,
    /// Serialized package exceeds its hard limit.
    #[error("trajectory package exceeds its encoded byte limit")]
    PackageTooLarge,
    /// Canonical serialization failed.
    #[error("trajectory serialization failed")]
    Serialization,
    /// Encoded package is malformed or contains unknown fields.
    #[error("trajectory package encoding is invalid")]
    InvalidEncoding,
    /// Counter arithmetic overflowed.
    #[error("trajectory counter overflow")]
    CounterOverflow,
    /// Existing token-accounting evidence is inconsistent.
    #[error(transparent)]
    TokenAccounting(#[from] crate::TokenAccountingError),
}

#[cfg(test)]
mod tests {
    use std::{fs, sync::Arc};

    use super::*;

    #[derive(Clone)]
    struct ByteTokenizer;

    impl TrajectoryTokenizer for ByteTokenizer {
        fn identity(&self) -> ActualTokenizerIdentity {
            ActualTokenizerIdentity {
                provider: "fixture".to_owned(),
                model: "fixture_model".to_owned(),
                tokenizer: "utf8_bytes".to_owned(),
                implementation: "fixture_tokenizer".to_owned(),
                implementation_version: Some("v1".to_owned()),
                implementation_sha256: None,
                asset_sha256: None,
            }
        }

        fn count(&self, input: &[u8]) -> Result<u64, TrajectoryError> {
            std::str::from_utf8(input).map_err(|_| TrajectoryError::InvalidUtf8)?;
            u64::try_from(input.len()).map_err(|_| TrajectoryError::CounterOverflow)
        }
    }

    struct ScriptedAdapter {
        condition: TrajectoryCondition,
        boundary: TrajectoryExecutionBoundary,
    }

    impl ScriptedAdapter {
        fn new(condition: TrajectoryCondition, boundary: TrajectoryExecutionBoundary) -> Self {
            Self {
                condition,
                boundary,
            }
        }
    }

    impl TrajectoryAdapter for ScriptedAdapter {
        fn condition(&self) -> TrajectoryCondition {
            self.condition
        }

        fn execution_boundary(&self) -> TrajectoryExecutionBoundary {
            self.boundary
        }

        fn execute(&mut self, input: TrajectoryExecutionInput<'_>) -> RawTrajectoryAttempt {
            let ordinal = match input.workflow.family {
                TrajectoryWorkflowFamily::LocateImplementation => 1,
                TrajectoryWorkflowFamily::ExplainSymbol => 2,
                TrajectoryWorkflowFamily::CallRelationships => 3,
                TrajectoryWorkflowFamily::BugFixContext => 4,
                TrajectoryWorkflowFamily::AssessChangeImpact => 5,
                TrajectoryWorkflowFamily::SelectTests => 6,
                TrajectoryWorkflowFamily::ArchitectureOverview => 7,
                TrajectoryWorkflowFamily::CycleInvestigation
                | TrajectoryWorkflowFamily::DeadCodeInvestigation
                | TrajectoryWorkflowFamily::CrossServiceTrace
                | TrajectoryWorkflowFamily::RefactoringBoundary
                | TrajectoryWorkflowFamily::HistoryComparison
                | TrajectoryWorkflowFamily::ApiMigrationBatch
                | TrajectoryWorkflowFamily::MultiRepositoryMigration => 0,
            };
            let (outcome, status, retry, signals, tool) = match ordinal {
                1 => (
                    TrajectoryAttemptOutcome::Succeeded,
                    TrajectoryOperationStatus::Succeeded,
                    false,
                    TrajectoryClaimSignals::default(),
                    "code.locate",
                ),
                2 => (
                    TrajectoryAttemptOutcome::Failed {
                        error_code: "fixture_failure".to_owned(),
                    },
                    TrajectoryOperationStatus::Failed {
                        error_code: "fixture_failure".to_owned(),
                    },
                    false,
                    TrajectoryClaimSignals::default(),
                    "source.read",
                ),
                3 => (
                    TrajectoryAttemptOutcome::TimedOut {
                        error_code: "response_timeout".to_owned(),
                    },
                    TrajectoryOperationStatus::TimedOut {
                        error_code: "response_timeout".to_owned(),
                    },
                    false,
                    TrajectoryClaimSignals::default(),
                    "symbol.relationships",
                ),
                4 => (
                    TrajectoryAttemptOutcome::Cancelled {
                        error_code: "cancelled".to_owned(),
                    },
                    TrajectoryOperationStatus::Cancelled {
                        error_code: "cancelled".to_owned(),
                    },
                    false,
                    TrajectoryClaimSignals::default(),
                    "flow.trace",
                ),
                5 => (
                    TrajectoryAttemptOutcome::Unsupported {
                        error_code: "unsupported".to_owned(),
                    },
                    TrajectoryOperationStatus::Failed {
                        error_code: "unsupported".to_owned(),
                    },
                    false,
                    TrajectoryClaimSignals {
                        unsupported_claims: 1,
                        ..TrajectoryClaimSignals::default()
                    },
                    "architecture.overview",
                ),
                6 => (
                    TrajectoryAttemptOutcome::Excluded {
                        reason_code: "environment_incompatible".to_owned(),
                    },
                    TrajectoryOperationStatus::Failed {
                        error_code: "environment_incompatible".to_owned(),
                    },
                    false,
                    TrajectoryClaimSignals::default(),
                    "architecture.cycles",
                ),
                7 => (
                    TrajectoryAttemptOutcome::Succeeded,
                    TrajectoryOperationStatus::Succeeded,
                    true,
                    TrajectoryClaimSignals {
                        ignored_truncation: 1,
                        ..TrajectoryClaimSignals::default()
                    },
                    "repo.status",
                ),
                _ => (
                    TrajectoryAttemptOutcome::Succeeded,
                    TrajectoryOperationStatus::Succeeded,
                    false,
                    TrajectoryClaimSignals::default(),
                    "query.advanced",
                ),
            };
            let request = br#"{"request":"fixture"}"#.to_vec();
            let response = br#"{"response":"fixture"}"#.to_vec();
            let source = b"fixture".to_vec();
            let first_status = if retry {
                TrajectoryOperationStatus::Failed {
                    error_code: "response_timeout".to_owned(),
                }
            } else {
                status.clone()
            };
            let mut calls = vec![RawTrajectoryCall {
                operation_id: "operation".to_owned(),
                tool: TrajectoryToolIdentity {
                    tool_id: tool.to_owned(),
                    tool_version: "v1".to_owned(),
                },
                exposure_profile: TrajectoryExposureProfile::Analysis,
                operation_status: first_status,
                retry_ordinal: 0,
                request_frame: request.clone(),
                response_frame: response.clone(),
                source_frame: source.clone(),
                elapsed_ns: 10,
                result_items: 1,
                truncated: signals.ignored_truncation > 0,
                continuation_available: false,
                claim_signals: signals,
            }];
            if retry {
                calls.push(RawTrajectoryCall {
                    operation_id: "operation".to_owned(),
                    tool: TrajectoryToolIdentity {
                        tool_id: tool.to_owned(),
                        tool_version: "v1".to_owned(),
                    },
                    exposure_profile: TrajectoryExposureProfile::Analysis,
                    operation_status: status,
                    retry_ordinal: 1,
                    request_frame: request,
                    response_frame: response,
                    source_frame: source,
                    elapsed_ns: 10,
                    result_items: 1,
                    truncated: false,
                    continuation_available: false,
                    claim_signals: TrajectoryClaimSignals::default(),
                });
            }
            RawTrajectoryAttempt { outcome, calls }
        }
    }

    fn protocol() -> TrajectoryProtocol {
        preregistered_trajectory_protocol("ab".repeat(32)).expect("fixture protocol is valid")
    }

    fn package() -> TrajectoryEvidencePackage {
        let mut rootlight = ScriptedAdapter::new(
            TrajectoryCondition::Rootlight,
            TrajectoryExecutionBoundary::DaemonMcpProcess,
        );
        let mut codebase = UnavailableTrajectoryAdapter::new(
            TrajectoryCondition::CodebaseMemory,
            "codebase_memory_process_v1",
            "executable_not_available",
        )
        .expect("unavailable adapter is valid");
        let mut files = ScriptedAdapter::new(
            TrajectoryCondition::BoundedFileExploration,
            TrajectoryExecutionBoundary::LocalBoundedFiles,
        );
        run_trajectory_suite(
            protocol(),
            &mut rootlight,
            &mut codebase,
            &mut files,
            &ByteTokenizer,
        )
        .expect("complete source-free package is produced")
    }

    #[test]
    fn protocol_preregisters_all_families_conditions_and_fixed_rules() {
        let protocol = protocol();
        protocol.validate().expect("protocol validates");
        assert_eq!(protocol.workflows.len(), 14);
        assert_eq!(protocol.conditions.len(), 3);
        assert_eq!(protocol.attempt_seeds, [17, 43]);
        assert_eq!(
            protocol
                .workflows
                .iter()
                .map(|workflow| workflow.family)
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(TrajectoryWorkflowFamily::ALL)
        );
        assert!(
            protocol
                .workflows
                .iter()
                .all(|workflow| !workflow.allows_status_preflight)
        );
        let observed = protocol
            .workflows
            .iter()
            .map(|workflow| {
                (
                    workflow.workflow_id.as_str(),
                    workflow.rootlight_tools.clone(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            observed,
            vec![
                (
                    "locate-implementation",
                    vec!["code.locate".to_owned(), "symbol.explain".to_owned()],
                ),
                ("explain-symbol", vec!["symbol.explain".to_owned()],),
                ("callers-callees", vec!["symbol.relationships".to_owned()],),
                (
                    "bug-fix-context",
                    vec!["code.locate".to_owned(), "context.pack".to_owned()],
                ),
                (
                    "change-impact",
                    vec![
                        "change.impact".to_owned(),
                        "tests.select".to_owned(),
                        "plan.change".to_owned(),
                    ],
                ),
                ("select-tests", vec!["tests.select".to_owned()]),
                (
                    "architecture-overview",
                    vec!["architecture.overview".to_owned()],
                ),
                (
                    "cyclic-dependencies",
                    vec![
                        "architecture.cycles".to_owned(),
                        "flow.trace".to_owned(),
                        "plan.change".to_owned(),
                    ],
                ),
                (
                    "dead-code",
                    vec!["code.dead".to_owned(), "symbol.explain".to_owned()],
                ),
                (
                    "cross-service-trace",
                    vec![
                        "code.locate".to_owned(),
                        "flow.trace".to_owned(),
                        "context.pack".to_owned(),
                    ],
                ),
                (
                    "refactoring-boundary",
                    vec![
                        "symbol.relationships".to_owned(),
                        "change.impact".to_owned(),
                        "context.pack".to_owned(),
                        "plan.change".to_owned(),
                    ],
                ),
                (
                    "history-comparison",
                    vec!["history.compare".to_owned(), "change.impact".to_owned()],
                ),
                ("api-migration-batch", vec!["query.batch".to_owned()],),
                (
                    "multi-repository-migration",
                    vec![
                        "repo.list".to_owned(),
                        "code.locate".to_owned(),
                        "code.locate".to_owned(),
                        "flow.trace".to_owned(),
                        "change.impact".to_owned(),
                        "context.pack".to_owned(),
                        "context.pack".to_owned(),
                    ],
                ),
            ]
        );
    }

    #[test]
    fn protocol_and_task_digests_are_deterministic_and_semantic() {
        let first = protocol();
        let second = protocol();
        assert_eq!(
            first.digests().expect("first digests"),
            second.digests().expect("second digests")
        );
        let mut changed = second;
        changed.bounds.result_items = changed.bounds.result_items.saturating_sub(1);
        assert_ne!(
            first.digests().expect("first digests"),
            changed.digests().expect("changed digests")
        );
    }

    #[test]
    fn runner_retains_success_failure_timeout_cancel_retry_exclusion_and_absence() {
        let package = package();
        package.validate().expect("package validates");
        assert_eq!(package.denominator.expected_attempts, 84);
        assert_eq!(package.denominator.observed_attempts, 84);
        assert!(package.denominator.succeeded > 0);
        assert!(package.denominator.failed > 0);
        assert!(package.denominator.timed_out > 0);
        assert!(package.denominator.cancelled > 0);
        assert!(package.denominator.unsupported > 0);
        assert!(package.denominator.not_available > 0);
        assert!(package.denominator.excluded > 0);
        assert!(package.denominator.retried > 0);
        assert!(package.denominator.redundant_status_preflights > 0);
        assert!(package.denominator.claim_signals.unsupported_claims > 0);
        assert!(package.denominator.claim_signals.ignored_truncation > 0);
    }

    #[test]
    fn bundle_conversion_preserves_all_denominator_attempts_and_actual_tokens() {
        let package = package();
        let trajectories = package
            .agent_trajectories()
            .expect("package converts to bundle trajectories");
        assert_eq!(trajectories.len(), package.attempts.len());
        assert!(trajectories.windows(2).all(|pair| {
            (
                pair[0].workflow_id.as_str(),
                pair[0].attempt_id.as_str(),
                pair[0].baseline_variant.as_str(),
            ) < (
                pair[1].workflow_id.as_str(),
                pair[1].attempt_id.as_str(),
                pair[1].baseline_variant.as_str(),
            )
        }));
        assert!(trajectories.iter().all(|trajectory| {
            !trajectory.steps.is_empty()
                && trajectory.steps.iter().all(|step| {
                    step.usage.tokens == step.request_tokens.saturating_add(step.response_tokens)
                        && step.source_tokens <= step.response_tokens
                })
        }));
        let manifest = package
            .evidence_manifest()
            .expect("evidence manifest converts");
        assert!(!manifest.artifacts.is_empty());
    }

    #[test]
    fn denominator_accounting_digest_and_privacy_mutations_fail_closed() {
        let mut missing = package();
        missing.attempts.pop();
        assert!(matches!(
            missing.validate(),
            Err(TrajectoryError::IncompleteDenominator)
        ));

        let mut denominator = package();
        denominator.denominator.failed = denominator.denominator.failed.saturating_add(1);
        assert!(matches!(
            denominator.validate(),
            Err(TrajectoryError::DenominatorMismatch)
        ));

        let mut digest = package();
        digest.attempts[0].task_sha256 = "cd".repeat(32);
        assert!(matches!(
            digest.validate(),
            Err(TrajectoryError::DigestMismatch | TrajectoryError::InvalidAttempt)
        ));

        let mut privacy = package();
        privacy.protocol.workflows[0].task_id = "c-users-private".to_owned();
        privacy.protocol.workflows[0].task_id.push('\\');
        assert!(matches!(
            privacy.validate(),
            Err(TrajectoryError::InvalidLabel)
        ));
    }

    #[test]
    fn actual_token_and_attempt_reconciliation_mutations_fail_closed() {
        let mut actual = package();
        actual.attempts[0].calls[0].accounting.total.actual_tokens = actual.attempts[0].calls[0]
            .accounting
            .total
            .actual_tokens
            .map(|value| value.saturating_add(1));
        assert!(actual.validate().is_err());

        let mut claims = package();
        claims.attempts[0].claim_signals.unsupported_claims = 99;
        assert!(matches!(
            claims.validate(),
            Err(TrajectoryError::AttemptAccountingMismatch)
        ));

        let mut retry = package();
        let retried = retry
            .attempts
            .iter_mut()
            .find(|attempt| attempt.retry_count > 0)
            .expect("fixture includes a retried attempt");
        match &mut retried.calls[0].operation_status {
            TrajectoryOperationStatus::Failed { error_code } => {
                *error_code = "nonretryable".to_owned();
            }
            status => panic!("unexpected retry predecessor: {status:?}"),
        }
        assert!(matches!(
            retry.validate(),
            Err(TrajectoryError::InvalidRetry)
        ));
    }

    #[test]
    fn evidence_encoding_round_trips_and_rejects_unknown_fields() {
        let package = package();
        let encoded = encode_trajectory_evidence(&package).expect("package encodes");
        let decoded = decode_trajectory_evidence(&encoded).expect("package decodes");
        assert_eq!(decoded, package);

        let mut value: serde_json::Value =
            serde_json::from_slice(&encoded).expect("encoded package is JSON");
        value["raw_source"] = serde_json::Value::String("forbidden".to_owned());
        let mutated = serde_json::to_vec(&value).expect("mutated package serializes");
        assert!(matches!(
            decode_trajectory_evidence(&mutated),
            Err(TrajectoryError::InvalidEncoding)
        ));
    }

    #[test]
    fn bounded_file_baseline_reads_only_regular_utf8_files_under_shared_limits() {
        let temporary = tempfile::tempdir().expect("temporary fixture is available");
        fs::write(temporary.path().join("one.rs"), "fn one() {}")
            .expect("first source fixture is written");
        fs::create_dir(temporary.path().join("nested")).expect("nested fixture directory exists");
        fs::write(
            temporary.path().join("nested").join("two.rs"),
            "fn two() {}",
        )
        .expect("second source fixture is written");
        let mut adapter = BoundedFileExplorationAdapter::new(temporary.path());
        let protocol = Arc::new(protocol());
        let workflow = &protocol.workflows[0];
        let task_sha256 = protocol
            .task_digest(&workflow.workflow_id)
            .expect("task digest exists");
        let attempt = adapter.execute(TrajectoryExecutionInput {
            workflow,
            task_sha256: &task_sha256,
            fixture_sha256: &protocol.fixture_sha256,
            attempt_index: 0,
            seed: 17,
            bounds: protocol.bounds,
            stopping: protocol.stopping,
            retry: &protocol.retry,
        });
        assert_eq!(attempt.outcome, TrajectoryAttemptOutcome::Succeeded);
        assert_eq!(attempt.calls[0].result_items, 1);
        assert!(!attempt.calls[0].source_frame.is_empty());
        let observations = adapter.take_observations();
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0].workflow_id, workflow.workflow_id);
        assert_eq!(
            observations[0].prompt_sha256,
            sha256_hex(trajectory_task_prompt(workflow.family, 17).as_bytes())
        );
        assert_eq!(observations[0].selected_paths.len(), 1);
        let encoded = serde_json::to_vec(&attempt.calls[0].response_frame)
            .expect("ephemeral response is serializable");
        assert!(!encoded.is_empty());
    }
}
